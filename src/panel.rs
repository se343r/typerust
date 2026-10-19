//! The panel: what it knows, what it does when the host says something, and what it asks for next.
//!
//! The shape of a panel is a loop. The host sends one event; the panel answers with a list of
//! operations — draw this, run that command, remember this — and is then silent until the next
//! event. There is no thread and no blocking call, so anything that has to happen in a particular
//! order (measure the layer, then fit the box, then move it) has to be written as a state machine
//! and driven by the answers. [`crate::actions`] is those machines; this file is the loop that
//! drives them, plus everything the panel remembers.
//!
//! Three decisions worth stating, because they are not obvious from the code:
//!
//! * **The document is a snapshot.** `doc.info`'s reply *is* the document JSON, so `doc.changed`
//!   carries a whole new one and the panel never has to ask twice for the same thing. It asks again
//!   after every finished action, because that action may have added or renumbered layers.
//! * **One source of truth for values.** The panel rebuilds the whole view on every change instead
//!   of patching widget values with `ui.set`. A host's value map outlives a view, so a value set for
//!   one style's editor would shadow the next style's field; declaring everything every time cannot
//!   go stale. It also means a text field's buffer never fights the panel: what the user typed is
//!   adopted, and the view that comes back says the same thing.
//! * **Nothing is assumed about the document.** A panel is handed whatever is open — no document, no
//!   selection, a raster layer selected, a script that is empty. Every one of those is a message in
//!   the footer, never a failed reply.

use std::collections::{BTreeMap, BTreeSet, HashMap};

use serde_json::{Value, json};

use crate::actions::{Kind, Run, Step};
use crate::doc::{Doc, Fonts};
use crate::geom;
use crate::library::{self, Folder, Library, Style};
use crate::props;
use crate::script::{self, Line};
use crate::view;

/// Where the library, the settings and the script are kept between sessions.
pub const STORAGE_LIBRARY: &str = "library";
pub const STORAGE_SETTINGS: &str = "settings";
pub const STORAGE_SCRIPT: &str = "script";
/// The script rows already lettered, and whether the script box hides them. Its own key rather than
/// part of `script`: `script` has always been a plain string, and an older panel's storage must keep
/// loading unchanged.
pub const STORAGE_DONE: &str = "done";
pub const STORAGE_STATES: &str = "states";

/// One named snapshot of the working position, TypeR's "states" (`settings.jsx` › Data): the script,
/// the line it was on and the style in use. TypeR also snapshots its page images and the last PSD it
/// opened; a panel can neither open a file nor see one, so those two are absent rather than stored
/// as fields that would do nothing.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct SavedState {
    pub script: String,
    /// An index into the raw script rows, as TypeR's `currentLineIndex` is.
    pub cursor: usize,
    pub style_id: Option<String>,
}

impl SavedState {
    fn to_json(&self) -> Value {
        json!({
            "text": self.script,
            "currentLineIndex": self.cursor,
            "currentStyleId": self.style_id,
        })
    }

    fn from_json(v: &Value) -> SavedState {
        SavedState {
            script: v.get("text").and_then(Value::as_str).unwrap_or_default().to_string(),
            cursor: v.get("currentLineIndex").and_then(Value::as_u64).unwrap_or(0).min(usize::MAX as u64) as usize,
            style_id: v.get("currentStyleId").and_then(Value::as_str).map(str::to_string),
        }
    }
}

/// Which pane the panel is showing.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Page {
    Lettering,
    Styles,
    Style,
    Folders,
    Settings,
    Help,
}

impl Page {
    pub fn id(self) -> &'static str {
        match self {
            Page::Lettering => "lettering",
            Page::Styles => "styles",
            Page::Style => "style",
            Page::Folders => "folders",
            Page::Settings => "settings",
            Page::Help => "help",
        }
    }

    fn from_id(id: &str) -> Page {
        match id {
            "styles" => Page::Styles,
            "style" => Page::Style,
            "folders" => Page::Folders,
            "settings" => Page::Settings,
            "help" => Page::Help,
            _ => Page::Lettering,
        }
    }
}

/// A line in the footer: what just happened, or why nothing did.
#[derive(Clone, Debug, PartialEq)]
pub struct Message {
    pub text: String,
    /// `info`, `warning` or `error` — the host's own label levels.
    pub level: &'static str,
}

impl Message {
    fn info(text: impl Into<String>) -> Message {
        Message { text: text.into(), level: "info" }
    }

    fn warn(text: impl Into<String>) -> Message {
        Message { text: text.into(), level: "warning" }
    }

    fn error(text: impl Into<String>) -> Message {
        Message { text: text.into(), level: "error" }
    }
}

/// A style open in the editor. The name, the prefixes and the colour are the editor's buffers; the
/// properties are the style's own `textProps`, edited in place.
#[derive(Clone, Debug, PartialEq)]
pub struct Edit {
    pub id: String,
    pub name: String,
    pub prefixes: String,
    pub color: String,
    pub folder: Option<String>,
    pub props: Value,
    pub is_new: bool,
    /// When the style was last changed, from the event's `now` — TyperTools' `edited`, which decides
    /// what a merge keeps.
    pub edited: u64,
}

/// What an operation the panel sent was for, so its answer can be routed when it comes back.
///
/// The host echoes the operation's own `id` in the `result` event, so this is a table keyed by that
/// id rather than a queue: an answer to something the panel has forgotten — a late answer after a
/// Cancel, an answer to a panel that was closed and reopened — is dropped instead of being mistaken
/// for the next thing it is waiting for.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Asking {
    /// The panel asked for the document, to refresh its snapshot.
    Doc,
    /// The run's first step asked for the document: the answer goes to the run.
    RunDoc,
    /// A `type.fonts` answer: the family list, or one family's faces.
    Fonts,
    /// A command from a run.
    Command,
    /// `type.info` on the active layer, to copy its style into the editor.
    CopyStyle,
}

/// Everything the panel remembers.
pub struct Panel {
    pub library: Library,
    /// The Text Size percentage: it scales the style's size and leading on Create and Insert.
    pub scale: f64,
    /// Interpret `**bold**`, `*italic*` and `***both***` in a line (TypeR's `interpretMarkdown`,
    /// off by default).
    pub markdown: bool,
    /// `ltr` or `rtl`: the paragraph direction handed to every style a line is lettered with, as
    /// TypeR's `direction` setting is.
    pub direction: String,
    /// TypeR's `internalPadding`: pixels taken off each side of the balloon box's width, 0–100.
    pub padding: f64,
    /// TypeR's `resizeTextBoxOnCenter`: whether Align reshapes the box, or only centres the layer.
    pub resize_on_center: bool,
    /// TypeR's `textSizeIncrement`: what the −/+ buttons add to a layer's size, 1–99.
    pub size_increment: f64,
    /// TypeR's `styleSizeStep`: what the quick style-size editor steps by, in the style's own unit.
    pub style_step: f64,
    /// TypeR's `showQuickStyleSize`: whether the library rows carry a size editor.
    pub quick_size: bool,
    /// Named snapshots, by name. TypeR keeps these in its own storage under `states`.
    pub states: BTreeMap<String, SavedState>,
    /// The name being typed in the states field.
    pub state_name: String,
    pub page: Page,
    pub script: String,
    pub lines: Vec<Line>,
    /// Which line the buttons work on: an index into `lines`, which is the raw script row.
    pub cursor: usize,
    /// The raw script rows that have been lettered: Create and Insert add the row they set. An index
    /// into `lines`; emptied whenever the script itself is edited, because rows move then.
    pub done: BTreeSet<usize>,
    /// Whether the script box leaves the finished rows out (the Done section lists them).
    pub hide_done: bool,
    pub doc: Option<Doc>,
    pub fonts: Fonts,
    pub edit: Option<Edit>,
    /// The folder being renamed, when one is. The name being typed is [`Panel::folder_name`], because
    /// the field is drawn whether or not a folder is open — naming a new folder and renaming an old
    /// one are the same three presses.
    pub folder: Option<String>,
    /// What the folder name field holds.
    pub folder_name: String,
    /// The folder the open folder editor is inside: `None` for the root of the library. Read when
    /// naming a new folder and when saving a rename, and drawn as the editor's parent dropdown.
    pub folder_parent: Option<String>,
    /// The JSON box on the Help page.
    pub io: String,
    pub message: Option<Message>,
    /// The action in flight, if any.
    pub run: Option<Run>,
    /// Operations that are awaiting a `result`, by the id they were sent with.
    asks: HashMap<u64, Asking>,
    /// Operation ids, so an answer can be matched to what it answers (and a stale one ignored).
    seq: u64,
    /// How many runs this session, for the coalescing key.
    runs: u64,
    /// The starter library was laid down before the host had told us which fonts it has. The styles
    /// are re-fonted the moment the family list arrives, because the font a starter library is built
    /// on is a guess about the machine — and a guess that Tahoma exists is a bad one on Linux.
    starter_pending: bool,
    /// The widget features this host draws, from `init.host.widgets` (`list`, `scroll`,
    /// `label.color`, `label.font`, `button.icon`). Empty on a host that sends none, which is how
    /// one module serves both: the view asks [`Panel::has`] and sends the plain layout otherwise.
    pub widgets: Vec<String>,
}

impl Default for Panel {
    fn default() -> Panel {
        Panel::new()
    }
}

impl Panel {
    pub fn new() -> Panel {
        Panel {
            library: Library::default(),
            scale: 100.0,
            markdown: false,
            direction: "ltr".to_string(),
            padding: geom::DEFAULT_INTERNAL_PADDING,
            resize_on_center: false,
            size_increment: 1.0,
            style_step: 0.1,
            quick_size: true,
            states: BTreeMap::new(),
            state_name: String::new(),
            page: Page::Lettering,
            script: String::new(),
            lines: Vec::new(),
            cursor: 0,
            done: BTreeSet::new(),
            hide_done: false,
            doc: None,
            fonts: Fonts::default(),
            edit: None,
            folder: None,
            folder_name: String::new(),
            folder_parent: None,
            io: String::new(),
            message: None,
            run: None,
            asks: HashMap::new(),
            seq: 0,
            runs: 0,
            starter_pending: false,
            widgets: Vec::new(),
        }
    }

    /// Whether this host draws a widget feature (`init.host.widgets`). A feature the host never
    /// named is absent, so the module falls back rather than relying on a tree the host refuses.
    pub fn has(&self, feature: &str) -> bool {
        self.widgets.iter().any(|w| w == feature)
    }

    /// The raw script rows the script list draws, in order: every row, or — with finished rows
    /// hidden — only the rows still to letter. Capped at what a `list` widget may hold; the rows
    /// past it stay in the script box and are reachable there.
    pub fn list_rows(&self) -> Vec<usize> {
        let visible = (0..self.lines.len()).filter(|i| !(self.hide_done && self.done.contains(i)));
        visible.take(view::LIST_MAX).collect()
    }

    /// The document's resolution, or 72 dpi when there is no document: every point/pixel conversion
    /// needs a number, and 72 is the one where a point is a pixel.
    pub fn dpi(&self) -> f64 {
        self.doc.as_ref().map(|d| d.dpi).unwrap_or(72.0)
    }

    /// Re-reads the script against the library's prefixes. Called whenever either changes.
    pub fn reparse(&mut self) {
        // TypeR gives priorities to the prefixes of the folder the *style in use* belongs to, so
        // the current line's style is resolved first — and a line whose prefix changes which style
        // is in use does not re-choose its own folder until the next parse, exactly as the original
        // reducer reads `state.currentStyle` rather than the new state's.
        let current_folder = self.library.current_style(self.current_line().and_then(|l| l.style_id.as_deref())).and_then(|s| s.folder.clone());
        let prefixes = self.library.prefixes(current_folder.as_deref(), self.library.folder_priority);
        let rules = script::Rules { ignore_prefixes: &self.library.ignore_prefixes, ignore_tags: &self.library.ignore_tags, prefixes: &prefixes };
        self.lines = script::parse(&self.script, &rules);
        self.cursor = script::settle(&self.lines, self.cursor).unwrap_or(0);
    }

    /// The line the buttons work on.
    pub fn current_line(&self) -> Option<&Line> {
        self.lines.get(self.cursor)
    }

    /// The dialogue number of the current line, 1-based, for the footer and the number field.
    pub fn line_number(&self) -> usize {
        match self.lines.get(self.cursor) {
            Some(l) if l.number > 0 => l.number,
            _ => 1,
        }
    }

    /// The script as the script box shows it: every row, or — with finished rows hidden — only the
    /// rows that have not been lettered yet. The hidden rows stay in [`Panel::script`]; they are
    /// what the Done section lists and what a change made in the box is merged back around.
    pub fn visible_script(&self) -> String {
        if !self.hide_done || self.done.is_empty() {
            return self.script.clone();
        }
        script::split(&self.script)
            .enumerate()
            .filter(|(i, _)| !self.done.contains(i))
            .map(|(_, row)| row)
            .collect::<Vec<_>>()
            .join("\n")
    }

    /// The layers an action would act on.
    pub fn target_layers(&self) -> Vec<u64> {
        self.doc.as_ref().map(Doc::target_layers).unwrap_or_default()
    }

    /// Opens a style in the editor — an existing one, or a new one when `is_new`.
    pub fn start_edit(&mut self, id: &str, is_new: bool) -> bool {
        let (props, edited) = if is_new {
            let font = self.fonts.families.first().map(String::as_str);
            let props = match font {
                Some(f) => props::default_props_for(f, props::DEFAULT_FONT_STYLE),
                None => props::default_props(),
            };
            (props, 0)
        } else {
            match self.library.style(id) {
                Some(s) => (s.text_props.clone(), s.edited),
                None => return false,
            }
        };
        let (name, prefixes, color, folder) = match self.library.style(id) {
            Some(s) if !is_new => (s.name.clone(), s.prefixes_text(), s.prefix_color.clone(), s.folder.clone()),
            _ => ("New style".to_string(), String::new(), library::DEFAULT_PREFIX_COLOR.to_string(), None),
        };
        self.edit = Some(Edit { id: id.to_string(), name, prefixes, color, folder, props, is_new, edited });
        true
    }

    /// The family the editor is showing, for the faces request.
    fn edit_family(&self) -> Option<String> {
        self.edit.as_ref().map(|e| props::family(&e.props)).filter(|f| !f.is_empty())
    }

    // --- the loop ------------------------------------------------------------------------------

    /// One event in, a list of operations out. This is the whole interface the host uses.
    pub fn handle(&mut self, event: &Value) -> Vec<Value> {
        match text(event, "type").as_str() {
            "init" => self.init(event),
            "panel.visible" => {
                if event.get("visible").and_then(Value::as_bool) == Some(false) {
                    // Hidden: nothing to draw, and no reason to spend a round trip. The panel is
                    // told again when it is shown, and asks for the document then.
                    Vec::new()
                } else {
                    // Shown again, or for the first time: the document may have changed while
                    // nothing was looking at it, so it is asked for afresh.
                    let ops = self.ask_doc();
                    self.reply(ops)
                }
            }
            "doc.changed" => {
                // The event's `doc` is the whole document or `null`; a null clears the snapshot,
                // because "there is no document" is a fact the panel has to act on.
                self.doc = Doc::from_json(&value(event, "doc"));
                self.reply(Vec::new())
            }
            "ui.change" => self.change(text(event, "id").as_str(), &value(event, "value"), now(event)),
            // A click carries a value only where the widget has one to report — a `list` row's
            // index. Everything else sends `null`, which the handler ignores.
            "ui.click" => self.click(text(event, "id").as_str(), &value(event, "value"), now(event)),
            "result" => self.result(event),
            "cancel" => {
                self.run = None;
                self.asks.clear();
                self.message = Some(Message::warn("Cancelled."));
                self.reply(Vec::new())
            }
            other => {
                // A host that grows new events must not stop a panel that predates them.
                self.message = Some(Message::warn(format!("This panel does not know the event “{other}”.")));
                self.reply(Vec::new())
            }
        }
    }

    fn init(&mut self, event: &Value) -> Vec<Value> {
        let storage = event.get("storage").and_then(Value::as_object);
        if let Some(s) = storage.and_then(|s| s.get(STORAGE_LIBRARY)).and_then(Value::as_str)
            && let Ok(v) = serde_json::from_str::<Value>(s)
        {
            self.library = Library::from_json(&v);
        }
        if let Some(s) = storage.and_then(|s| s.get(STORAGE_SETTINGS)).and_then(Value::as_str)
            && let Ok(v) = serde_json::from_str::<Value>(s)
        {
            self.scale = number(&v, "scale", 100.0).clamp(1.0, 999.0);
            self.page = Page::from_id(text(&v, "page").as_str());
            self.markdown = v.get("markdown").and_then(Value::as_bool).unwrap_or(false);
            self.direction = match text(&v, "direction").as_str() {
                "rtl" => "rtl".to_string(),
                _ => "ltr".to_string(),
            };
            self.padding = integer(number(&v, "internalPadding", geom::DEFAULT_INTERNAL_PADDING), 0.0, 100.0, geom::DEFAULT_INTERNAL_PADDING);
            self.resize_on_center = v.get("resizeTextBoxOnCenter").and_then(Value::as_bool).unwrap_or(false);
            self.size_increment = integer(number(&v, "textSizeIncrement", 1.0), 1.0, 99.0, 1.0);
            self.style_step = positive(number(&v, "styleSizeStep", 0.1), 0.1);
            self.quick_size = v.get("showQuickStyleSize").and_then(Value::as_bool).unwrap_or(true);
        }
        if let Some(s) = storage.and_then(|s| s.get(STORAGE_SCRIPT)).and_then(Value::as_str) {
            self.script = s.to_string();
        }
        if let Some(s) = storage.and_then(|s| s.get(STORAGE_DONE)).and_then(Value::as_str)
            && let Ok(v) = serde_json::from_str::<Value>(s)
        {
            self.hide_done = v.get("hide").and_then(Value::as_bool).unwrap_or(false);
            if let Some(list) = v.get("lines").and_then(Value::as_array) {
                for i in list.iter().filter_map(Value::as_u64).filter_map(|i| usize::try_from(i).ok()) {
                    self.done.insert(i);
                }
            }
        }
        if let Some(s) = storage.and_then(|s| s.get(STORAGE_STATES)).and_then(Value::as_str)
            && let Ok(v) = serde_json::from_str::<Value>(s)
            && let Some(map) = v.as_object()
        {
            for (name, saved) in map {
                let name = name.trim();
                if !name.is_empty() && name.chars().count() <= view::LABEL_MAX {
                    self.states.insert(name.to_string(), SavedState::from_json(saved));
                }
            }
        }
        self.widgets.clear();
        if let Some(list) = event.get("host").and_then(|h| h.get("widgets")).and_then(Value::as_array) {
            for w in list.iter().filter_map(Value::as_str).take(32) {
                let w = w.trim();
                if !w.is_empty() && w.len() <= 64 && !self.widgets.iter().any(|x| x == w) {
                    self.widgets.push(w.to_string());
                }
            }
        }
        self.reparse();
        // A stored finished-row index past the end of the script (it was shortened before it was
        // saved, or `script` was edited elsewhere) would mark a row that does not exist.
        self.done.retain(|i| *i < self.lines.len());
        if self.library.styles.is_empty() {
            // Nothing stored: a letterer opening the panel for the first time gets something to work
            // with rather than an empty list and a blank page. The starter library is this port's own.
            let font = self.fonts.families.first().cloned();
            self.library = library::starter(font.as_deref());
            // At `init` the host has not answered `type.fonts` yet, so the starter styles are almost
            // always built on the placeholder font and re-fonted when the list arrives.
            self.starter_pending = font.is_none();
            self.message = Some(Message::info("Loaded the starter style library."));
            let mut ops = self.save_library();
            ops.extend(self.ask_doc());
            ops.extend(self.ask_fonts(None));
            return self.reply(ops);
        }
        let mut ops = self.ask_doc();
        ops.extend(self.ask_fonts(None));
        self.reply(ops)
    }

    /// A `result` event: whatever the answer belongs to gets it.
    ///
    /// Anything the panel did not ask for is dropped. That is not defensive padding: a timer result,
    /// or an answer left over from an action that was cancelled, would otherwise be handed to the run
    /// in flight and read as *its* measurement — which is how a panel quietly letters the wrong box.
    fn result(&mut self, event: &Value) -> Vec<Value> {
        let Some(id) = event.get("id").and_then(Value::as_u64) else {
            return self.reply(Vec::new());
        };
        let Some(ask) = self.asks.remove(&id) else {
            return self.reply(Vec::new());
        };
        match ask {
            Asking::Doc => {
                // `doc.info` with nothing open answers `ok: false` and an error, not a document, so
                // the answer's own `ok` decides — reading the event itself would invent an empty
                // document where the host said there is none.
                self.doc = match event.get("ok").and_then(Value::as_bool) {
                    Some(true) => Doc::from_json(event),
                    _ => None,
                };
                if self.starter_font() {
                    return self.reply(self.save_library());
                }
                self.reply(Vec::new())
            }
            Asking::RunDoc => {
                self.doc = Doc::from_json(event);
                self.feed(Some(event))
            }
            Asking::Fonts => {
                self.fonts.absorb(event);
                if self.starter_font() {
                    return self.reply(self.save_library());
                }
                self.reply(Vec::new())
            }
            Asking::CopyStyle => {
                self.copy_style(event);
                self.reply(Vec::new())
            }
            Asking::Command => self.feed(Some(event)),
        }
    }

    /// Moves the starter library onto a font the host actually has, once it has said which ones it
    /// has. A starter library is laid down during `init`, before any `type.fonts` answer, so it is
    /// built on [`props::DEFAULT_FONT`] — "Tahoma", which is what the original assumes and which most
    /// Linux systems do not have. The styles are re-fonted when the family list arrives, but only if
    /// the letterer has not already chosen fonts of their own: an edit outranks our placeholder.
    fn starter_font(&mut self) -> bool {
        if !self.starter_pending {
            return false;
        }
        let Some(family) = self.fonts.families.first().cloned() else {
            return false; // Still waiting: the starter styles keep their placeholder font.
        };
        self.starter_pending = false;
        let untouched = self.library.styles.iter().all(|s| {
            let f = props::family(&s.text_props);
            f == props::DEFAULT_FONT || f.is_empty()
        });
        if !untouched {
            return true; // Nothing to move, and nothing left to wait for either.
        }
        for style in &mut self.library.styles {
            let face = props::font_style(&style.text_props);
            props::set_font(&mut style.text_props, &family, &face, None);
        }
        true
    }

    /// Moves the run on: one step, then whatever it asks for next.
    fn feed(&mut self, answer: Option<&Value>) -> Vec<Value> {
        if self.run.is_none() {
            // An answer to something already abandoned (a cancel, a panel that was closed): nothing
            // to do with it, and definitely not a reason to fail.
            return self.reply(Vec::new());
        }
        let dpi = self.dpi();
        let step = match self.run.as_mut() {
            Some(run) => run.step(dpi, answer),
            None => return self.reply(Vec::new()),
        };
        self.act(step)
    }

    /// Emits the operation a step asks for.
    fn act(&mut self, step: Step) -> Vec<Value> {
        match step {
            Step::Doc => {
                let id = self.ask(Asking::RunDoc);
                self.reply(vec![json!({"op": "doc.info", "id": id})])
            }
            Step::Call { command, params } => {
                let id = self.ask(Asking::Command);
                self.message = Some(Message::info(format!("{}…", self.run.as_ref().map(|r| r.kind.label()).unwrap_or("Working"))));
                self.reply(vec![json!({"op": "doc.command", "id": id, "command": command, "params": params})])
            }
            Step::Done(what) => {
                let kind = self.run.as_ref().map(|r| r.kind);
                self.run = None;
                self.message = Some(Message::info(what));
                // The action has changed the document: the layer list may be different now, and the
                // next button press has to work on what is there rather than on what was.
                let mut ops = self.ask_doc();
                ops.extend(self.after_run(kind));
                self.reply(ops)
            }
            Step::Failed(why) => {
                self.run = None;
                self.message = Some(Message::error(why.clone()));
                self.asks.clear();
                self.reply(vec![json!({"op": "ui.notice", "level": "error", "text": why})])
            }
        }
    }

    /// After an action: move to the next line of dialogue, which is what a letterer working down a
    /// page wants — but only for the two actions that *letter a line*. Align and the size buttons act
    /// on a layer and leave the script where it is, and TyperTools does not advance on them either
    /// (`previewBlock.jsx` sends `nextLine` from Create and Insert only).
    ///
    /// The line the action set is marked done first: that is what the Done section lists and what
    /// the script box leaves out when finished rows are hidden.
    fn after_run(&mut self, kind: Option<Kind>) -> Vec<Value> {
        if !matches!(kind, Some(Kind::Create) | Some(Kind::Insert)) {
            return Vec::new();
        }
        let mut ops = Vec::new();
        if self.cursor < self.lines.len() && self.done.insert(self.cursor) {
            ops.extend(self.save_done());
        }
        if let Some(at) = script::next(&self.lines, self.cursor) {
            self.cursor = at;
        }
        ops
    }

    /// The reply: whatever else was asked for, and the panel as it now stands.
    fn reply(&self, ops: Vec<Value>) -> Vec<Value> {
        let mut out = ops;
        out.push(json!({"op": "ui.view", "view": view::render(self)}));
        out
    }

    /// Reserves the id for an operation that expects an answer, and remembers what the answer is for.
    fn ask(&mut self, what: Asking) -> u64 {
        self.seq += 1;
        self.asks.insert(self.seq, what);
        self.seq
    }

    fn ask_doc(&mut self) -> Vec<Value> {
        let id = self.ask(Asking::Doc);
        vec![json!({"op": "doc.info", "id": id})]
    }

    /// Asks for the family list (`None`) or one family's faces.
    fn ask_fonts(&mut self, family: Option<&str>) -> Vec<Value> {
        let id = self.ask(Asking::Fonts);
        let params = match family {
            Some(f) => json!({"family": f}),
            None => json!({}),
        };
        vec![json!({"op": "doc.command", "id": id, "command": "type.fonts", "params": params})]
    }

    // --- clicks and typed values ------------------------------------------------------------------

    fn change(&mut self, id: &str, value: &Value, at: u64) -> Vec<Value> {
        // A library row's quick size editor: the field takes the style's own unit, as TypeR's does.
        if let Some(i) = row_index(id, "s", "_size") {
            let v = num(value);
            if v.is_finite()
                && v > 0.0
                && let Some(style) = self.library.styles.get_mut(i)
            {
                props::set_char(&mut style.text_props, "size", json!(v));
                props::set_char(&mut style.text_props, "impliedFontSize", json!(v));
                style.edited = at;
                return self.reply(self.save_library());
            }
            return self.reply(Vec::new());
        }
        match id {
            "page" => {
                self.page = Page::from_id(&as_str(value));
                return self.reply(self.save_settings());
            }
            "script" => {
                let edited: String = as_str(value).chars().take(view::SCRIPT_MAX).collect();
                // While finished rows are hidden, the field holds only the rows still to do: put
                // them back where they were and leave the done rows alone. If the row count no
                // longer matches, there is no telling where an added or removed row belongs, so the
                // edit is refused rather than guessed at (and the view sends the field back).
                if self.hide_done && !self.done.is_empty() {
                    let Some(full) = merge_hidden(&self.script, &self.done, &edited) else {
                        self.message = Some(Message::warn("Turn off Hide finished lines to add or remove rows."));
                        return self.reply(Vec::new());
                    };
                    if full != self.script {
                        self.script = full;
                        self.reparse();
                    }
                    return self.reply(self.save_script());
                }
                if edited == self.script {
                    return self.reply(Vec::new());
                }
                // Rows have probably moved: what was finished may not be finished any more.
                self.script = edited;
                self.done.clear();
                self.reparse();
                let mut ops = self.save_script();
                ops.extend(self.save_done());
                return self.reply(ops);
            }
            "hideDone" => {
                self.hide_done = on(value);
                return self.reply(self.save_done());
            }
            "lineNo" => {
                // The field shows the dialogue number (1-based, ignored lines unnumbered), so that is
                // what it sends back; `script::nth` counts them from zero.
                if let Some(n) = value.as_f64().filter(|n| n.is_finite()).filter(|n| *n >= 1.0)
                    && let Some(at) = script::nth(&self.lines, n as usize - 1)
                {
                    self.cursor = at;
                }
                return self.reply(Vec::new());
            }
            "textScale" => {
                self.scale = value.as_f64().filter(|n| n.is_finite()).unwrap_or(100.0).clamp(1.0, 999.0);
                return self.reply(self.save_settings());
            }
            "defaultStyle" => {
                self.library.default_style_id = Some(as_str(value));
                return self.reply(self.save_library());
            }
            "set_pointText" => {
                self.library.point_text = value.as_bool().unwrap_or(false);
                return self.reply(self.save_library());
            }
            "set_ignore" => {
                self.library.ignore_prefixes = split_tags(&as_str(value));
                self.reparse();
                return self.reply(self.save_library());
            }
            "set_ignoreTags" => {
                self.library.ignore_tags = split_tags(&as_str(value));
                self.reparse();
                return self.reply(self.save_library());
            }
            "set_folderPriority" => {
                self.library.folder_priority = value.as_bool().unwrap_or(true);
                self.reparse();
                return self.reply(self.save_library());
            }
            "set_markdown" => {
                self.markdown = value.as_bool().unwrap_or(false);
                return self.reply(self.save_settings());
            }
            "set_direction" => {
                self.direction = match as_str(value).as_str() {
                    "rtl" => "rtl".to_string(),
                    _ => "ltr".to_string(),
                };
                return self.reply(self.save_settings());
            }
            "set_padding" => {
                self.padding = integer(num(value), 0.0, 100.0, geom::DEFAULT_INTERNAL_PADDING);
                return self.reply(self.save_settings());
            }
            "set_resizeOnCenter" => {
                self.resize_on_center = value.as_bool().unwrap_or(false);
                return self.reply(self.save_settings());
            }
            "set_sizeIncrement" => {
                self.size_increment = integer(num(value), 1.0, 99.0, 1.0);
                return self.reply(self.save_settings());
            }
            "set_styleStep" => {
                self.style_step = positive(num(value), 0.1);
                return self.reply(self.save_settings());
            }
            "set_quickStyleSize" => {
                self.quick_size = value.as_bool().unwrap_or(true);
                return self.reply(self.save_settings());
            }
            "state_name" => {
                self.state_name = as_str(value).chars().take(view::LABEL_MAX).collect();
                return self.reply(Vec::new());
            }
            "f_name" => {
                // A buffer, not a folder: the field is drawn before there is a folder to name, and
                // `f_new` and `f_save` are what write it into the library.
                self.folder_name = as_str(value).chars().take(view::LABEL_MAX).collect();
                return self.reply(Vec::new());
            }
            "f_parent" => {
                let parent = Some(as_str(value)).filter(|f| f != "-");
                // The picker leaves out the folder and its descendants; a stale event is refused
                // here too, so a loop cannot be made by racing the view.
                let allowed = match self.folder.as_deref() {
                    Some(id) => self.library.can_move_into(id, parent.as_deref()),
                    None => parent.as_deref().is_none_or(|p| self.library.folder(p).is_some()),
                };
                if allowed {
                    self.folder_parent = parent;
                } else {
                    self.message = Some(Message::error("A folder cannot go inside itself or one of its own folders."));
                }
                return self.reply(Vec::new());
            }
            "io_json" => {
                self.io = as_str(value).chars().take(view::SCRIPT_MAX).collect();
                return self.reply(Vec::new());
            }
            _ => {}
        }
        // Everything else is a style-editor field.
        self.edit_field(id, value, at);
        // A new family means new faces: the Face dropdown is filled from what the host last said
        // about *this* family, so it is asked again rather than left showing the old one's.
        let ops = if id == "ed_font" { self.ask_edit_fonts() } else { Vec::new() };
        self.reply(ops)
    }

    /// Writes one editor field into the style being edited. The mapping is TyperTools' own jam key
    /// names (`props.rs`), because the library format is the interface with Photoshop's TyperTools.
    fn edit_field(&mut self, id: &str, value: &Value, at: u64) {
        let Some(e) = self.edit.as_mut() else { return };
        let num = |v: &Value| v.as_f64().filter(|n| n.is_finite()).unwrap_or(0.0);
        match id {
            "ed_name" => e.name = as_str(value),
            "ed_prefixes" => e.prefixes = as_str(value),
            "ed_color" => e.color = as_str(value),
            "ed_folder" => e.folder = Some(as_str(value)).filter(|f| f != "-"),
            "ed_font" => {
                let face = props::font_style(&e.props);
                props::set_font(&mut e.props, &as_str(value), &face, None);
            }
            "ed_face" => {
                let family = props::family(&e.props);
                props::set_font(&mut e.props, &family, &as_str(value), None);
            }
            "ed_size" => {
                let v = num(value).clamp(geom::MIN_SIZE_PT, geom::MAX_SIZE_PT);
                props::set_char(&mut e.props, "size", json!(v));
                props::set_char(&mut e.props, "impliedFontSize", json!(v));
            }
            "ed_leading" => props::set_char(&mut e.props, "leading", json!(num(value).max(0.0))),
            "ed_autoLeading" => props::set_char(&mut e.props, "autoLeading", json!(value.as_bool().unwrap_or(false))),
            "ed_autoLeadingPct" => props::set_para(&mut e.props, "autoLeadingPercentage", json!((num(value) / 100.0).clamp(0.1, 10.0))),
            "ed_tracking" => props::set_char(&mut e.props, "tracking", json!(num(value).clamp(geom::TRACKING_MIN, geom::TRACKING_MAX))),
            "ed_kerning" => props::set_char(&mut e.props, "autoKern", json!(as_str(value))),
            "ed_kernPair" => props::set_char(&mut e.props, "kerning", json!(num(value).clamp(geom::KERN_MIN, geom::KERN_MAX))),
            "ed_hScale" => props::set_char(&mut e.props, "horizontalScale", json!(num(value).clamp(1.0, 1000.0))),
            "ed_vScale" => props::set_char(&mut e.props, "verticalScale", json!(num(value).clamp(1.0, 1000.0))),
            "ed_baseline" => {
                props::set_char(&mut e.props, "baselineShift", json!(num(value).clamp(-geom::BASELINE_SHIFT_MAX_PT * 4.0, geom::BASELINE_SHIFT_MAX_PT * 4.0)))
            }
            "ed_script" => props::set_char(&mut e.props, "baseline", json!(as_str(value))),
            "ed_caps" => props::set_char(&mut e.props, "fontCaps", json!(as_str(value))),
            "ed_align" => props::set_para(&mut e.props, "alignment", json!(as_str(value))),
            "ed_fauxBold" => props::set_char(&mut e.props, "syntheticBold", json!(value.as_bool().unwrap_or(false))),
            "ed_fauxItalic" => props::set_char(&mut e.props, "syntheticItalic", json!(value.as_bool().unwrap_or(false))),
            "ed_underline" => props::set_char(&mut e.props, "underline", json!(if on(value) { "underlineOn" } else { "underlineOff" })),
            "ed_strike" => props::set_char(&mut e.props, "strikethrough", json!(if on(value) { "strikethroughOn" } else { "strikethroughOff" })),
            "ed_ligatures" => props::set_char(&mut e.props, "ligature", json!(value.as_bool().unwrap_or(false))),
            "ed_discLigatures" => props::set_char(&mut e.props, "altligature", json!(value.as_bool().unwrap_or(false))),
            "ed_hyphenate" => props::set_para(&mut e.props, "hyphenate", json!(value.as_bool().unwrap_or(false))),
            "ed_firstIndent" => props::set_para(&mut e.props, "firstLineIndent", json!(num(value))),
            "ed_startIndent" => props::set_para(&mut e.props, "leftIndent", json!(num(value))),
            "ed_endIndent" => props::set_para(&mut e.props, "rightIndent", json!(num(value))),
            "ed_spaceBefore" => props::set_para(&mut e.props, "spaceBefore", json!(num(value).max(0.0))),
            "ed_spaceAfter" => props::set_para(&mut e.props, "spaceAfter", json!(num(value).max(0.0))),
            "ed_colorHex" => props::set_char(&mut e.props, "color", props::hex_color(&as_str(value))),
            "ed_antiAlias" => props::set_anti_alias(&mut e.props, &as_str(value)),
            _ => return,
        }
        e.edited = at;
    }

    fn click(&mut self, id: &str, value: &Value, at: u64) -> Vec<Value> {
        // A row of the script list: the host sends its index among the rows it drew, and
        // `list_rows` is the same list the view built, so the mapping back to a raw script row is
        // exact — including while finished rows are hidden.
        if id == "scriptLine" {
            if let Some(i) = value.as_u64().and_then(|n| usize::try_from(n).ok())
                && let Some(raw) = self.list_rows().get(i).copied()
            {
                self.cursor = raw;
            }
            return self.reply(Vec::new());
        }
        // Library rows first: their ids carry an index.
        if let Some(i) = row_index(id, "s", "_apply") {
            let index = self.library.styles.len();
            if i < index {
                let style = self.library.styles[i].clone();
                return self.start(Kind::Apply, Some(&style), false, at);
            }
            return self.reply(Vec::new());
        }
        if let Some(i) = row_index(id, "s", "_edit") {
            if i < self.library.styles.len() {
                let id = self.library.styles[i].id.clone();
                self.start_edit(&id, false);
                self.page = Page::Style;
                // The editor opens on a family, so its faces are asked for with it.
                let ops = self.ask_edit_fonts();
                return self.reply(ops);
            }
            return self.reply(Vec::new());
        }
        // Folder rows are numbered by their place in the tree the Folders page draws, not by their
        // place in the library array: nesting means the two orders differ.
        let folder_row = |lib: &Library, i: usize| lib.folder_order().get(i).and_then(|id| lib.folder(id)).cloned();
        if let Some(i) = row_index(id, "f", "_edit") {
            if let Some(f) = folder_row(&self.library, i) {
                // Renaming loads the folder's own name into the field, which is the one place a
                // folder name is edited; its parent is loaded beside it.
                self.folder = Some(f.id.clone());
                self.folder_name = f.name.clone();
                self.folder_parent = f.parent_id.clone();
            }
            return self.reply(Vec::new());
        }
        if let Some(i) = row_index(id, "f", "_del") {
            if let Some(f) = folder_row(&self.library, i) {
                let id = f.id.clone();
                self.library.remove_folder(&id);
                if self.folder.as_deref() == Some(id.as_str()) {
                    self.folder = None;
                    self.folder_parent = None;
                }
                self.message = Some(Message::info("Deleted the folder and the folders inside it. Their styles are back in the unsorted list."));
            }
            return self.reply(self.save_library());
        }
        if let Some(i) = row_index(id, "f", "_up") {
            if let Some(f) = folder_row(&self.library, i) {
                self.library.move_folder(&f.id, true);
            }
            return self.reply(self.save_library());
        }
        if let Some(i) = row_index(id, "f", "_down") {
            if let Some(f) = folder_row(&self.library, i) {
                self.library.move_folder(&f.id, false);
            }
            return self.reply(self.save_library());
        }
        if let Some(i) = row_index(id, "f", "_export") {
            if let Some(f) = folder_row(&self.library, i) {
                // TypeR writes `<folder name>.json` through a save dialog. A panel has no file
                // dialog, so the JSON goes on the Help page to copy out — the same shape TypeR's
                // "Export folder" button writes.
                self.io = match serde_json::to_string_pretty(&library::export_folder_json(&self.library, &f.id)) {
                    Ok(s) => s,
                    Err(_) => "{}".to_string(),
                };
                self.message = Some(Message::info(format!("“{}” is in the box on the Help page: copy it out to keep it, or load it on another machine.", f.name)));
            }
            return self.reply(Vec::new());
        }

        if let Some(i) = row_index(id, "s", "_sizeDown") {
            return self.quick_size(i, -1.0, at);
        }
        if let Some(i) = row_index(id, "s", "_sizeUp") {
            return self.quick_size(i, 1.0, at);
        }
        if let Some(i) = row_index(id, "st", "_load") {
            self.load_state(i);
            return self.reply(self.save_script());
        }
        if let Some(i) = row_index(id, "st", "_del") {
            if let Some(name) = self.states.keys().nth(i).cloned() {
                self.states.remove(&name);
                self.message = Some(Message::info(format!("Deleted the state “{name}”.")));
            }
            return self.reply(self.save_states());
        }

        match id {
            "back" => {
                self.page = Page::Lettering;
                self.reply(Vec::new())
            }
            "openSettings" => {
                self.page = Page::Settings;
                self.reply(Vec::new())
            }
            "openHelp" => {
                self.page = Page::Help;
                self.reply(Vec::new())
            }
            "openFolders" => {
                self.page = Page::Folders;
                self.reply(Vec::new())
            }
            "state_save" => {
                let name = self.state_name.trim().to_string();
                if name.is_empty() {
                    self.message = Some(Message::error("A state needs a name: that is what the list on the Settings page shows."));
                    return self.reply(Vec::new());
                }
                let state = SavedState {
                    script: self.script.clone(),
                    cursor: self.cursor,
                    style_id: self.current_line().and_then(|l| l.style_id.clone()).or_else(|| self.library.default_style_id.clone()),
                };
                self.states.insert(name.clone(), state);
                self.state_name.clear();
                self.message = Some(Message::info(format!("Saved the state “{name}”.")));
                self.reply(self.save_states())
            }
            "prev" => {
                if let Some(at) = script::previous(&self.lines, self.cursor) {
                    self.cursor = at;
                }
                self.reply(Vec::new())
            }
            "next" => {
                if let Some(at) = script::next(&self.lines, self.cursor) {
                    self.cursor = at;
                }
                self.reply(Vec::new())
            }
            "first" => {
                self.cursor = script::settle(&self.lines, 0).unwrap_or(0);
                self.reply(Vec::new())
            }
            "last" => {
                self.cursor = script::settle(&self.lines, self.lines.len().saturating_sub(1)).unwrap_or(0);
                self.reply(Vec::new())
            }
            "create" => self.start(Kind::Create, None, false, at),
            "insert" => self.start(Kind::Insert, None, false, at),
            "align" => self.start(Kind::Align, None, false, at),
            "sizeDown" => self.resize(-1.0, at),
            "sizeUp" => self.resize(1.0, at),
            "apply" => self.start(Kind::Apply, None, false, at),
            "applyKeep" => self.start(Kind::Apply, None, true, at),
            "newStyle" => {
                let id = self.library.new_style_id();
                self.start_edit(&id, true);
                self.page = Page::Style;
                let ops = self.ask_edit_fonts();
                self.reply(ops)
            }
            "ed_save" => self.save_edit(),
            "ed_delete" => self.delete_edit(),
            "ed_cancel" => {
                self.edit = None;
                self.page = Page::Styles;
                self.reply(Vec::new())
            }
            "ed_up" => self.move_style(-1),
            "ed_down" => self.move_style(1),
            "ed_copy" => self.copy_from_layer(),
            "f_new" => {
                let name = self.folder_name.trim().to_string();
                let name = if name.is_empty() { format!("Folder {}", self.library.folders.len() + 1) } else { name };
                let id = self.library.new_folder_id();
                let parent = self.folder_parent.clone().filter(|p| self.library.folder(p).is_some());
                let order = self.library.child_folders(parent.as_deref()).len();
                self.library.folders.push(Folder {
                    id: id.clone(),
                    name: name.clone(),
                    parent_id: parent,
                    order: u32::try_from(order).unwrap_or(0),
                });
                self.library.normalize_folders();
                self.folder = Some(id);
                self.folder_name = name;
                self.message = Some(Message::info("Made the folder. Put a style in it with the editor's Folder list."));
                self.reply(self.save_library())
            }
            "f_save" => {
                let name = self.folder_name.trim().to_string();
                if let Some(id) = self.folder.clone() {
                    if name.is_empty() {
                        self.message = Some(Message::error("A folder needs a name."));
                        return self.reply(Vec::new());
                    }
                    let parent = self.folder_parent.clone().filter(|p| self.library.folder(p).is_some());
                    if !self.library.can_move_into(&id, parent.as_deref()) {
                        self.message = Some(Message::error("A folder cannot go inside itself or one of its own folders."));
                        return self.reply(Vec::new());
                    }
                    if let Some(slot) = self.library.folders.iter_mut().find(|x| x.id == id) {
                        slot.name = name;
                        slot.parent_id = parent;
                    }
                    self.library.normalize_folders();
                }
                self.folder = None;
                self.folder_parent = None;
                self.message = Some(Message::info("Saved the folder. The styles in it are untouched."));
                self.reply(self.save_library())
            }
            "f_cancel" => {
                self.folder = None;
                self.folder_parent = None;
                self.reply(Vec::new())
            }
            "set_scaleDefault" => {
                self.scale = 100.0;
                self.reply(self.save_settings())
            }
            "set_starter" => {
                let font = self.fonts.families.first().cloned();
                self.library = library::starter(font.as_deref());
                self.starter_pending = font.is_none();
                self.reparse();
                self.message = Some(Message::info("Loaded the starter style library."));
                self.reply(self.save_library())
            }
            "set_reset" => {
                self.library = Library::default();
                self.script.clear();
                self.scale = 100.0;
                self.edit = None;
                self.folder = None;
                self.reparse();
                self.message = Some(Message::info("Everything is back to how it starts."));
                let mut ops = self.save_library();
                ops.extend(self.save_settings());
                ops.extend(self.save_script());
                self.reply(ops)
            }
            "io_export" => {
                // TypeR's export is its storage shape minus the fields a different machine must not
                // inherit (the `exported` timestamp is the one it writes with a clock; a panel has
                // none, so it is absent). Every folder and every style directly in one goes out.
                let ids: Vec<String> = self.library.folders.iter().map(|f| f.id.clone()).collect();
                let exported = library::export_json(&self.library, &ids, true);
                self.io = match serde_json::to_string_pretty(&exported) {
                    Ok(s) => s,
                    Err(_) => "{}".to_string(),
                };
                self.message = Some(Message::info("This is the library in TypeR's own export format (`TypeR_Export.json`): copy it out to keep it, or load it on another machine."));
                self.reply(Vec::new())
            }
            "io_reset" => {
                self.io.clear();
                self.reply(Vec::new())
            }
            "io_load" | "io_merge" => self.import(id == "io_merge"),
            other => {
                self.message = Some(Message::warn(format!("“{other}” is not a control this panel knows.")));
                self.reply(Vec::new())
            }
        }
    }

    // --- the actions ---------------------------------------------------------------------------

    /// Starts one of the five actions. The line's own style is used when the action letters a line;
    /// `style` names one explicitly for a library row.
    fn start(&mut self, kind: Kind, style: Option<&Style>, keep_size: bool, _at: u64) -> Vec<Value> {
        if self.run.is_some() {
            return self.reply(Vec::new());
        }
        let line = self.current_line().cloned();
        let line_style = line.as_ref().and_then(|l| l.style_id.clone());
        let chosen = style.cloned().or_else(|| self.library.current_style(line_style.as_deref()).cloned());
        let Some(style) = chosen else {
            self.message = Some(Message::error("There is no style to letter with: make one on the Styles page, or load the starter library from Help."));
            return self.reply(Vec::new());
        };
        let text = match kind {
            Kind::Create | Kind::Insert => {
                let Some(line) = &line else {
                    self.message = Some(Message::error("There is no script line here: paste a script, or move to a line."));
                    return self.reply(Vec::new());
                };
                if line.ignored || line.text.trim().is_empty() {
                    // An empty script is one blank row rather than no row at all, so the two cases
                    // are told apart here rather than by the row being missing.
                    self.message = Some(Message::error(if self.script.trim().is_empty() {
                        "There is nothing to letter: paste a script, one line of dialogue per row, into the box above."
                    } else {
                        "That line is a stage direction (or empty), so there is nothing to letter. Move to a line of dialogue."
                    }));
                    return self.reply(Vec::new());
                }
                // TyperTools strips the newlines out of a line before it letters it.
                line.text.replace(['\n', '\r'], " ").trim().to_string()
            }
            _ => String::new(),
        };
        let targets = match kind {
            Kind::Create => Vec::new(),
            _ => {
                let t = self.target_layers();
                if t.is_empty() {
                    self.message = Some(Message::error("Select a type layer first: the panel works on the layers the Layers panel has selected."));
                    return self.reply(Vec::new());
                }
                t
            }
        };
        if matches!(kind, Kind::Apply | Kind::Resize) && style.text_props.is_null() {
            self.message = Some(Message::error("That style has no properties."));
            return self.reply(Vec::new());
        }
        // The Text Size percentage is TyperTools' own: it scales the size and the leading on the two
        // actions that letter a line, and never when a style is applied to what is already there.
        let scale = if matches!(kind, Kind::Create | Kind::Insert) { self.scale / 100.0 } else { 1.0 };
        // TypeR's markdown setting: the markers are stripped before the layer is made, and the
        // weights they asked for go on as character runs afterwards (a panel's `type.create` has no
        // run list, so it takes one extra `type.edit`). Apply does not touch what a layer says.
        let (text, runs) = if self.markdown && matches!(kind, Kind::Create | Kind::Insert) {
            let parsed = script::parse_markdown(&text);
            let runs = if parsed.has_formatting() { markdown_runs(&parsed.runs) } else { Vec::new() };
            (parsed.plain, runs)
        } else {
            (text, Vec::new())
        };
        self.runs += 1;
        let mut run = Run::begin(kind, format!("typertools-{}", self.runs), targets);
        run.text = text;
        run.runs = runs;
        run.padding = self.padding;
        run.resize = self.resize_on_center;
        run.style_name = style.name.clone();
        run.params = props::to_params(&style.text_props, self.dpi(), scale);
        if kind != Kind::Resize
            && let Some(o) = run.params.as_object_mut()
        {
            // TypeR hands its direction to every path that writes text or a style
            // (`host.js:781`, `:1474`); `type.create`'s and `type.setStyle`'s paragraph keys both
            // take `auto|ltr|rtl`.
            o.insert("direction".into(), json!(self.direction));
        }
        run.antialias = props::anti_alias_param(props::anti_alias(&style.text_props)).to_string();
        // Applying a style applies its size too — TyperTools' own arrow does, and Ctrl+click on it is
        // the exception, which is the second button here.
        run.keep_size = keep_size;
        self.message = Some(Message::info(format!("{}…", kind.label())));
        self.run = Some(run);
        let dpi = self.dpi();
        let step = match self.run.as_mut() {
            Some(run) => run.step(dpi, None),
            None => Step::Failed("the action could not be started".into()),
        };
        self.act(step)
    }

    /// TypeR's quick style size: a library row's size moved by one `styleSizeStep`, in the style's own
    /// unit, and stored. It changes the library entry only — no document, no layer.
    fn quick_size(&mut self, i: usize, direction: f64, at: u64) -> Vec<Value> {
        let step = positive(self.style_step, 0.1);
        let decimals = ((format!("{step}").split('.').nth(1).unwrap_or("")).len()).min(12);
        let Some(style) = self.library.styles.get_mut(i) else {
            return self.reply(Vec::new());
        };
        let base = props::char(&style.text_props, "size").and_then(Value::as_f64).unwrap_or(1.0);
        let next = normalize_step(base + direction * step, step, decimals);
        if !(next.is_finite() && next > 0.0) {
            return self.reply(Vec::new());
        }
        props::set_char(&mut style.text_props, "size", json!(next));
        props::set_char(&mut style.text_props, "impliedFontSize", json!(next));
        style.edited = at;
        self.reply(self.save_library())
    }

    /// Loads a saved state: the script, the line, and the style in use (TypeR's `import` reducer).
    fn load_state(&mut self, i: usize) {
        let Some((name, state)) = self.states.iter().nth(i).map(|(n, s)| (n.clone(), s.clone())) else {
            return;
        };
        self.script = state.script;
        self.reparse();
        self.cursor = script::settle(&self.lines, state.cursor).unwrap_or(0);
        if let Some(id) = state.style_id.filter(|id| self.library.style(id).is_some()) {
            self.library.default_style_id = Some(id);
        }
        self.page = Page::Lettering;
        self.message = Some(Message::info(format!("Loaded the state “{name}”.")));
    }

    /// The size buttons: one document pixel's worth of points, keeping the layer where it was.
    ///
    /// One step, not two. TyperTools 1.4.9 carries a second stepper (`_changeSize_alt`, which would
    /// move by 0.2 mm / 1 px / 0.5 pt depending on the layer's ruler unit) but nothing calls it — the
    /// only live path is `changeActiveLayerTextSize(±1)`, which the preview's two buttons use. A port
    /// invents nothing, so there is one step here and two buttons.
    fn resize(&mut self, direction: f64, _at: u64) -> Vec<Value> {
        let targets = self.target_layers();
        if targets.is_empty() {
            self.message = Some(Message::error("Select a type layer first: the size buttons work on the layers the Layers panel has selected."));
            return self.reply(Vec::new());
        }
        if self.run.is_some() {
            return self.reply(Vec::new());
        }
        self.runs += 1;
        let mut run = Run::begin(Kind::Resize, format!("typertools-{}", self.runs), targets);
        // TypeR's `textSizeIncrement`, in the layer's own unit — which in PhotoCraft is points.
        run.delta = direction * self.size_increment;
        run.padding = self.padding;
        run.params = json!({});
        self.message = Some(Message::info(if direction > 0.0 { "Bigger…" } else { "Smaller…" }));
        self.run = Some(run);
        let dpi = self.dpi();
        let step = match self.run.as_mut() {
            Some(run) => run.step(dpi, None),
            None => Step::Failed("the action could not be started".into()),
        };
        self.act(step)
    }

    // --- the style library ---------------------------------------------------------------------

    /// Saves the editor's style into the library.
    fn save_edit(&mut self) -> Vec<Value> {
        let Some(e) = self.edit.clone() else {
            return self.reply(Vec::new());
        };
        if e.name.trim().is_empty() {
            self.message = Some(Message::error("A style needs a name: that is what the Styles page lists."));
            return self.reply(Vec::new());
        }
        if e.is_new {
            let mut style = Style::new(e.id.clone(), e.name.trim());
            style.folder = e.folder.clone();
            style.prefix_color = e.color.clone();
            style.text_props = e.props.clone();
            style.edited = e.edited;
            style.set_prefixes_text(&e.prefixes);
            self.library.styles.push(style);
            self.library.resort();
            if self.library.default_style_id.is_none() {
                self.library.default_style_id = Some(e.id.clone());
            }
            self.message = Some(Message::info(format!("Added the style “{}”.", e.name.trim())));
        } else if self.library.style(&e.id).is_some() {
            let name = e.name.trim().to_string();
            // The borrow of the style ends with this block: `resort` reorders the whole library, and
            // a saved style can move in it (renaming it can change nothing, but changing its folder
            // can), so the message is written from a name taken first.
            if let Some(style) = self.library.style_mut(&e.id) {
                style.name = name.clone();
                style.folder = e.folder.clone();
                style.prefix_color = e.color.clone();
                style.text_props = e.props.clone();
                style.set_prefixes_text(&e.prefixes);
                style.edited = e.edited;
            }
            self.library.resort();
            self.message = Some(Message::info(format!("Saved “{name}”.")));
        } else {
            self.message = Some(Message::error("That style is gone: it was deleted while it was open."));
            self.edit = None;
            return self.reply(self.save_library());
        }
        self.edit = None;
        self.page = Page::Styles;
        self.reparse();
        self.reply(self.save_library())
    }

    fn delete_edit(&mut self) -> Vec<Value> {
        let Some(e) = self.edit.clone() else {
            return self.reply(Vec::new());
        };
        if e.is_new {
            return self.reply(Vec::new());
        }
        let name = e.name.clone();
        self.library.styles.retain(|s| s.id != e.id);
        if self.library.default_style_id.as_deref() == Some(e.id.as_str()) {
            self.library.default_style_id = self.library.styles.first().map(|s| s.id.clone());
        }
        self.edit = None;
        self.page = Page::Styles;
        self.reparse();
        self.message = Some(Message::info(format!("Deleted “{name}”.")));
        self.reply(self.save_library())
    }

    /// Moves the open style one place up or down in the matching order. Order is behaviour — the
    /// first style whose prefix matches a line wins — which is why this is here and not a drag.
    fn move_style(&mut self, by: i64) -> Vec<Value> {
        let Some(e) = self.edit.clone() else {
            return self.reply(Vec::new());
        };
        let Some(at) = self.library.styles.iter().position(|s| s.id == e.id) else {
            return self.reply(Vec::new());
        };
        let to = at as i64 + by;
        if to < 0 || to as usize >= self.library.styles.len() {
            self.message = Some(Message::warn("That is as far as it goes."));
            return self.reply(Vec::new());
        }
        self.library.styles.swap(at, to as usize);
        self.message = Some(Message::info("Moved. The first style whose prefix matches a line is the one that letters it, so order matters."));
        self.reply(self.save_library())
    }

    /// Copies the style of the active (or first selected) type layer into the editor, as TyperTools'
    /// Edit Style ▸ copy does.
    fn copy_from_layer(&mut self) -> Vec<Value> {
        let Some(layer) = self.target_layers().first().copied() else {
            self.message = Some(Message::error("Select a type layer to copy the style from."));
            return self.reply(Vec::new());
        };
        let id = self.ask(Asking::CopyStyle);
        self.reply(vec![json!({"op": "doc.command", "id": id, "command": "type.info", "params": {"layer": layer}})])
    }

    /// A `type.info` answer for [`copy_from_layer`].
    fn copy_style(&mut self, info: &Value) {
        if info.get("ok").and_then(Value::as_bool) != Some(true) {
            self.message = Some(Message::error("The layer's type could not be read."));
            return;
        }
        let unit = self.edit.as_ref().map(|e| props::type_unit(&e.props).to_string()).unwrap_or_else(|| props::DEFAULT_TYPE_UNIT.to_string());
        let mut props = props::from_info(info, &unit, self.dpi());
        if let Some(layer) = props.get_mut("layerText").and_then(Value::as_object_mut) {
            library::strip_layer_keys(layer);
        }
        if let Some(e) = self.edit.as_mut() {
            e.props = props;
            e.prefixes = info.get("text").and_then(Value::as_str).map(prefix_from_line).unwrap_or_default();
        }
        self.message = Some(Message::info("Copied the layer's style into the editor. Save to keep it."));
    }

    /// Asks for the faces of the family the editor is showing, so the Face dropdown is real. The
    /// request is the caller's to send: an answer outstanding is one the panel has to be able to
    /// match, so the op travels back with whatever else the caller was replying with.
    fn ask_edit_fonts(&mut self) -> Vec<Value> {
        match self.edit_family() {
            Some(f) => self.ask_fonts(Some(&f)),
            None => Vec::new(),
        }
    }

    fn import(&mut self, merge: bool) -> Vec<Value> {
        let raw = self.io.trim().to_string();
        if raw.is_empty() {
            self.message = Some(Message::error("Paste a library into the box first — or press Show JSON to see this one."));
            return self.reply(Vec::new());
        }
        let Ok(v) = serde_json::from_str::<Value>(&raw) else {
            self.message = Some(Message::error("That is not JSON. A TyperTools library is an object with `styles` and `folders`."));
            return self.reply(Vec::new());
        };
        let imported = Library::from_json(&v);
        if imported.styles.is_empty() && imported.folders.is_empty() {
            self.message = Some(Message::error("That JSON has no styles in it. TyperTools' own export has a `styles` array."));
            return self.reply(Vec::new());
        }
        if merge {
            let kept = self.library.merge(&imported);
            self.message = Some(Message::info(format!("Merged {} style(s) in, keeping {kept} of this library's own.", imported.styles.len())));
        } else {
            let count = imported.styles.len();
            self.library = imported;
            self.message = Some(Message::info(format!("Loaded {count} style(s).")));
        }
        self.reparse();
        self.reply(self.save_library())
    }

    // --- storage -------------------------------------------------------------------------------

    fn save_library(&self) -> Vec<Value> {
        let value = serde_json::to_string(&self.library.to_json()).unwrap_or_else(|_| "{}".to_string());
        vec![json!({"op": "storage.set", "key": STORAGE_LIBRARY, "value": value})]
    }

    fn save_settings(&self) -> Vec<Value> {
        let value = json!({
            "scale": self.scale,
            "page": self.page.id(),
            "markdown": self.markdown,
            "direction": self.direction,
            "internalPadding": self.padding,
            "resizeTextBoxOnCenter": self.resize_on_center,
            "textSizeIncrement": self.size_increment,
            "styleSizeStep": self.style_step,
            "showQuickStyleSize": self.quick_size,
        })
        .to_string();
        vec![json!({"op": "storage.set", "key": STORAGE_SETTINGS, "value": value})]
    }

    fn save_states(&self) -> Vec<Value> {
        let map: serde_json::Map<String, Value> = self.states.iter().map(|(name, state)| (name.clone(), state.to_json())).collect();
        let value = Value::Object(map).to_string();
        vec![json!({"op": "storage.set", "key": STORAGE_STATES, "value": value})]
    }

    fn save_script(&self) -> Vec<Value> {
        vec![json!({"op": "storage.set", "key": STORAGE_SCRIPT, "value": self.script})]
    }

    fn save_done(&self) -> Vec<Value> {
        let value = json!({
            "lines": self.done.iter().copied().collect::<Vec<usize>>(),
            "hide": self.hide_done,
        })
        .to_string();
        vec![json!({"op": "storage.set", "key": STORAGE_DONE, "value": value})]
    }
}

/// The full script after an edit made while the finished rows were hidden: the visible rows are
/// taken from the field and the hidden ones are kept as they were. `None` when the number of visible
/// rows no longer matches — an added or removed row has no place to go, and putting it somewhere
/// could move somebody's dialogue.
fn merge_hidden(full: &str, done: &BTreeSet<usize>, edited: &str) -> Option<String> {
    let rows: Vec<&str> = script::split(full).collect();
    let visible: Vec<usize> = (0..rows.len()).filter(|i| !done.contains(i)).collect();
    let edited: Vec<&str> = script::split(edited).collect();
    // An empty string is one empty row to `split`, but a field with nothing in it and nothing
    // visible in it is no rows at all.
    let edited: Vec<&str> = if edited == [""] { Vec::new() } else { edited };
    if visible.len() != edited.len() {
        return None;
    }
    let mut out: Vec<String> = rows.iter().map(|row| (*row).to_string()).collect();
    for (at, text) in visible.into_iter().zip(edited) {
        if let Some(row) = out.get_mut(at) {
            *row = text.to_string();
        }
    }
    Some(out.join("\n"))
}

// --- small readers -----------------------------------------------------------------------------

fn value(event: &Value, key: &str) -> Value {
    event.get(key).cloned().unwrap_or(Value::Null)
}

fn text(v: &Value, key: &str) -> String {
    v.get(key).and_then(Value::as_str).unwrap_or_default().to_string()
}

fn as_str(v: &Value) -> String {
    match v {
        Value::String(s) => s.clone(),
        Value::Null => String::new(),
        other => other.to_string(),
    }
}

fn number(v: &Value, key: &str, fallback: f64) -> f64 {
    v.get(key).and_then(Value::as_f64).filter(|n| n.is_finite()).unwrap_or(fallback)
}

fn now(event: &Value) -> u64 {
    event.get("now").and_then(Value::as_u64).unwrap_or(0)
}

fn on(v: &Value) -> bool {
    v.as_bool().unwrap_or(false)
}

/// Splits a settings field of tags the way TypeR's settings describe them — "separated with line
/// breaks or semicolons" (`settingsLinePrefixesDescr`, `settingsIgnoreTagsDescr`) — trimming each and
/// dropping blanks. Nothing is split on spaces: a tag may be `"## "` or `"[note]"`.
fn split_tags(text: &str) -> Vec<String> {
    text.split(['\n', ';']).map(str::trim).filter(|t| !t.is_empty()).map(str::to_string).collect()
}

/// An integer setting, the way TypeR's reducers take one: `parseInt`, then the reducer's clamp, with
/// rubbish falling back to the default rather than to zero.
fn integer(value: f64, min: f64, max: f64, fallback: f64) -> f64 {
    if !value.is_finite() {
        return fallback;
    }
    value.trunc().clamp(min, max)
}

/// A step that TypeR insists is finite and positive: anything else is its 0.1 default.
fn positive(value: f64, fallback: f64) -> f64 {
    if value.is_finite() && value > 0.0 { value } else { fallback }
}

/// TypeR's style-size normalisation (`stylesBlock.jsx`): snap to a multiple of the step, at the
/// step's own number of decimals, and never below 1.
fn normalize_step(value: f64, step: f64, decimals: usize) -> f64 {
    let rounded = (value / step).round() * step;
    let factor = 10f64.powi(i32::try_from(decimals).unwrap_or(3).min(12));
    (rounded * factor).round() / factor
}

/// Markdown's weighted runs as `type.edit`'s `runs`: character offsets, and only the weights the
/// run actually asked for (the style already carries the layer's other character properties).
fn markdown_runs(runs: &[script::Run]) -> Vec<Value> {
    let mut out = Vec::new();
    let mut at = 0usize;
    for run in runs {
        let start = at;
        at = at.saturating_add(run.chars);
        if !(run.bold || run.italic) {
            continue;
        }
        let mut spec = json!({"start": start, "end": at});
        if run.bold {
            spec["fauxBold"] = json!(true);
        }
        if run.italic {
            spec["fauxItalic"] = json!(true);
        }
        out.push(spec);
    }
    out
}

/// The number a widget sent, or `NaN` so the caller's fallback applies.
fn num(v: &Value) -> f64 {
    v.as_f64().filter(|n| n.is_finite()).unwrap_or(f64::NAN)
}

/// The index in `s3_apply`-shaped ids, for the library rows.
fn row_index(id: &str, prefix: &str, suffix: &str) -> Option<usize> {
    id.strip_prefix(prefix)?.strip_suffix(suffix)?.parse::<usize>().ok()
}

/// A style's prefixes guessed from a line of its own text, for copy-from-layer: the first word if it
/// looks like a tag. TyperTools asks the letterer instead, and so does this panel — the field is
/// right there — but starting from the line is a better guess than starting empty.
fn prefix_from_line(text: &str) -> String {
    let first = text.split_whitespace().next().unwrap_or_default();
    if first.len() <= 8 && first.chars().all(|c| c.is_ascii_alphanumeric() || c == '.' || c == ':') && first.chars().any(|c| !c.is_ascii_alphanumeric()) {
        first.to_string()
    } else {
        String::new()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A panel being driven the way the host drives it: an event in, ops out, and an answer that
    /// carries **the id the panel chose** for the operation it answers.
    ///
    /// Nothing here reaches around the protocol. A test that answered with an id of its own would be
    /// testing a panel that does not exist, and would miss the one thing the id is for: matching an
    /// answer to the operation it belongs to.
    struct H {
        p: Panel,
        out: Vec<Value>,
    }

    impl H {
        /// A panel that has been through `init` with nothing stored, and has not answered anything:
        /// the state a letterer's very first press of the button meets.
        fn fresh() -> H {
            let mut h = H { p: Panel::new(), out: Vec::new() };
            h.send(json!({"type": "init", "now": 10, "locale": "en", "storage": {},
                          "host": {"app": "PhotoCraft", "version": "0.1"}}));
            h
        }

        fn send(&mut self, event: Value) -> &mut H {
            self.out = self.p.handle(&event);
            self
        }

        fn click(&mut self, id: &str) -> &mut H {
            self.send(json!({"type": "ui.click", "now": 5, "id": id}))
        }

        /// A click on a `list` row: the host sends the row's index as the event's value.
        fn click_row(&mut self, id: &str, row: usize) -> &mut H {
            self.send(json!({"type": "ui.click", "now": 5, "id": id, "value": row}))
        }

        fn change(&mut self, id: &str, value: Value) -> &mut H {
            self.send(json!({"type": "ui.change", "now": 5, "id": id, "value": value}))
        }

        /// Answers the first operation of this kind the panel asked for, as the host does: the
        /// panel's own id echoed back, with `ok` and the operation's fields.
        fn answer(&mut self, op: &str, ok: bool, fields: Value) -> &mut H {
            let id = self.find(op).first().and_then(|o| o.get("id")).cloned().unwrap_or(Value::Null);
            assert!(!id.is_null(), "the panel asked for no {op} to answer: {:?}", self.out);
            let mut event = json!({"type": "result", "now": 5, "id": id, "ok": ok});
            if let (Some(to), Some(from)) = (event.as_object_mut(), fields.as_object()) {
                for (k, v) in from {
                    to.insert(k.clone(), v.clone());
                }
            }
            self.send(event)
        }

        /// Answers the document request the panel just made.
        fn doc(&mut self) -> &mut H {
            self.answer("doc.info", true, doc_fields())
        }

        /// Answers the document request with "there is no document open".
        fn no_doc(&mut self) -> &mut H {
            self.answer("doc.info", false, json!({"error": "no document is open"}))
        }

        fn ops(&self) -> Vec<&str> {
            self.out.iter().filter_map(|o| o.get("op").and_then(Value::as_str)).collect()
        }

        fn find(&self, op: &str) -> Vec<&Value> {
            self.out.iter().filter(|o| o.get("op").and_then(Value::as_str) == Some(op)).collect()
        }

        /// The `doc.command` for one command, when the panel asked for it.
        fn call(&self, command: &str) -> Option<&Value> {
            self.find("doc.command").into_iter().find(|o| o.get("command").and_then(Value::as_str) == Some(command))
        }

        /// The command of the first `doc.command` op, for "what did the run ask for next".
        fn first_call(&self) -> String {
            self.find("doc.command").first().and_then(|o| o.get("command")).and_then(Value::as_str).unwrap_or_default().to_string()
        }

        fn message(&self) -> String {
            self.p.message.as_ref().map(|m| m.text.clone()).unwrap_or_default()
        }

        fn failed(&self) -> bool {
            self.p.message.as_ref().is_some_and(|m| m.level == "error")
        }

        /// Answers whatever the run asked for next, until it stops asking — the loop the host drives,
        /// with a `type.info`-shaped measurement for every command and the created layer's id for a
        /// `type.create`. Returns with the run finished, or fails the test if it never finishes.
        fn drive(&mut self) {
            for _ in 0..12 {
                if self.p.run.is_none() {
                    return;
                }
                if !self.find("doc.info").is_empty() {
                    self.doc();
                    continue;
                }
                if self.first_call().is_empty() {
                    break;
                }
                self.answer("doc.command", true, type_fields());
            }
            assert!(self.p.run.is_none(), "the run never finished: {}", self.first_call());
        }
    }

    /// The document as `doc.info` answers it, without the envelope: 2000×3000 at 300 dpi, one type
    /// layer selected, and a marquee from (100, 50) to (300, 210).
    fn doc_fields() -> Value {
        json!({"width": 2000, "height": 3000, "resolution": 300,
               "activeLayer": 4, "selectedLayers": [4], "hasSelection": true,
               "selectionBounds": [100, 50, 200, 160], "revision": 12,
               "layers": [{"id": 4, "name": "Balloon", "kind": "Type", "selected": true}]})
    }

    /// A type layer as `type.info` answers it: a box layer whose type has landed at (110, 60).
    fn type_fields() -> Value {
        json!({"layer": 4, "text": "I'll be home by six.",
               "shape": {"Box": {"x": 0, "y": 0, "width": 162, "height": 2100}},
               "transform": [1, 0, 0, 1, 110, 60], "bounds": [110, 60, 272, 100],
               "antialias": "Smooth", "runs": [{"style": {"size_pt": 14.0}}]})
    }

    #[test]
    fn init_loads_what_was_stored_and_asks_for_the_document_and_the_fonts() {
        let lib = library::starter(Some("Inter"));
        let mut h = H { p: Panel::new(), out: Vec::new() };
        h.send(json!({"type": "init", "now": 10, "storage": {
            "library": lib.to_json().to_string(),
            "settings": json!({"scale": 150.0, "page": "styles"}).to_string(),
            "script": "d. hello",
        }}));
        assert_eq!(h.p.library.styles.len(), 5, "the stored library, not the starter one");
        assert_eq!(h.p.scale, 150.0);
        assert_eq!(h.p.page, Page::Styles);
        assert_eq!(h.p.script, "d. hello");
        assert_eq!(h.p.lines.len(), 1);
        assert_eq!(h.ops(), ["doc.info", "doc.command", "ui.view"]);
        assert_eq!(h.first_call(), "type.fonts");
        h.doc();
        assert_eq!(h.p.doc.as_ref().map(|d| d.dpi), Some(300.0));
        assert_eq!(h.p.target_layers(), vec![4]);
    }

    #[test]
    fn a_first_run_gets_a_starter_library_on_a_font_the_host_actually_has() {
        let mut h = H::fresh();
        assert_eq!(h.p.library.styles.len(), 5);
        assert!(h.ops().contains(&"storage.set"), "the starter library is stored");
        assert!(h.message().contains("starter"), "{}", h.message());
        // It is written during `init`, before any `type.fonts` answer, so it starts on TyperTools' own
        // default — Tahoma, which most Linux systems do not have.
        assert_eq!(props::family(&h.p.library.styles[0].text_props), props::DEFAULT_FONT);
        let out = h.answer("doc.command", true, json!({"families": ["Inter", "DejaVu Sans"]}));
        assert!(out.ops().contains(&"storage.set"), "and the re-fonted library is stored: {:?}", out.out);
        assert!(h.p.library.styles.iter().all(|s| props::family(&s.text_props) == "Inter"), "{:?}", h.p.library.styles[0].text_props);
        // A host with no fonts at all is not an error, and not a reason to lose the library.
        let mut h = H::fresh();
        h.answer("doc.command", true, json!({}));
        assert_eq!(h.p.library.styles.len(), 5);
    }

    #[test]
    fn a_script_is_parsed_against_the_librarys_own_prefixes() {
        let mut h = H::fresh();
        h.change("script", json!("d. One\n## door\nx. BANG\nplain"));
        assert_eq!(h.p.lines.len(), 4);
        assert_eq!(h.p.lines[0].style_id.as_deref(), Some("s1"), "“d.” is Dialogue's prefix");
        assert!(h.p.lines[1].ignored, "“##” is an ignore prefix");
        assert_eq!(h.p.lines[2].style_id.as_deref(), Some("s5"), "“x.” is the sound effect");
        assert_eq!(h.p.lines[3].style_id, None, "a line with no tag still gets the default style");
        assert_eq!(h.p.line_number(), 1, "an ignored line has no dialogue number");
        assert!(h.ops().contains(&"storage.set"), "the script is kept between sessions");
    }

    #[test]
    fn create_runs_the_whole_typertools_sequence_as_one_history_step() {
        let mut h = H::fresh();
        h.doc();
        h.change("script", json!("d. I'll be home by six.\nd. You said that yesterday."));

        h.click("create");
        assert_eq!(h.ops(), ["doc.info", "ui.view"], "a run starts by reading the document");
        h.answer("doc.info", true, doc_fields());

        // The box: the marquee pulled in by 10 px, 0.9 of its width less TypeR's default internal
        // padding of 10 px a side, and fifteen times its height — deliberately far too tall, so the
        // line lays out on one line before it is fitted.
        let call = h.call("type.create").cloned().expect("the layer is made");
        assert_eq!(call["params"]["text"], "I'll be home by six.");
        assert_eq!(call["params"]["box"], json!([110.0, 60.0, 142.0, 2100.0]));
        assert_eq!(call["params"]["direction"], "ltr", "TypeR sends its direction with every create");
        let coalesce = call["params"]["coalesce"].clone();
        assert!(coalesce.is_string(), "{coalesce}");

        h.answer("doc.command", true, json!({"layer": 9}));
        assert_eq!(h.first_call(), "type.info", "the new layer is measured");
        h.answer("doc.command", true, type_fields());

        let call = h.call("type.edit").cloned().expect("the layer is fitted");
        assert_eq!(call["params"]["coalesce"], coalesce, "one action is one undo step");
        assert_eq!(call["params"]["antialias"], "smooth", "a style's anti-aliasing is a layer property here");
        assert!(call["params"]["move"].is_array(), "and the type is moved to the balloon: {call}");

        h.answer("doc.command", true, type_fields());
        assert!(h.ops().contains(&"doc.info"), "after an action the panel re-reads the document");
        assert!(h.p.run.is_none());
        assert!(h.message().starts_with("Placed"), "{}", h.message());
        assert_eq!(h.p.cursor, 1, "and moves on to the next line, so a letterer can work down a page");
    }

    #[test]
    fn only_the_two_actions_that_letter_a_line_move_the_script_on() {
        // Align and the size buttons act on a layer, not on a line, and TyperTools does not advance
        // on them either (`previewBlock.jsx` sends `nextLine` from Create and Insert only).
        for (button, advances) in [("create", true), ("insert", true), ("align", false), ("sizeUp", false)] {
            let mut h = H::fresh();
            h.doc();
            h.change("script", json!("d. one\nd. two"));
            assert_eq!(h.p.cursor, 0);
            h.click(button);
            h.drive();
            assert_eq!(h.p.cursor == 1, advances, "{button}: {}", h.message());
        }
    }

    #[test]
    fn create_marks_the_line_finished_and_hiding_it_leaves_it_out_of_the_box() {
        let mut h = H::fresh();
        h.doc();
        h.change("script", json!("d. one\nd. two\nd. three"));
        h.click("create");
        h.drive();
        assert_eq!(h.p.cursor, 1, "the next line, as before");
        assert_eq!(h.p.done.iter().copied().collect::<Vec<_>>(), vec![0], "the row that was lettered");
        // The finished rows are stored under their own key, so the `script` key stays a plain string
        // and an older panel's storage keeps loading.
        let saved = h.find("storage.set").into_iter().find(|o| o.get("key").and_then(Value::as_str) == Some(STORAGE_DONE)).expect("the finished rows are stored");
        assert!(saved["value"].as_str().is_some_and(|s| s.contains("\"lines\":[0]")), "{saved}");

        h.change("hideDone", json!(true));
        assert!(h.p.hide_done);
        assert_eq!(h.p.visible_script(), "d. two\nd. three");
        // An edit made in the shortened box goes back around the finished row rather than moving it.
        h.change("script", json!("d. TWO\nd. THREE"));
        assert_eq!(h.p.script, "d. one\nd. TWO\nd. THREE");
        // A row added while rows are hidden has no place to go, so the edit is refused rather than
        // guessed at — and the box is sent back as it was.
        h.change("script", json!("d. TWO\nd. THREE\nd. FOUR"));
        assert!(h.message().contains("Hide finished lines"), "{}", h.message());
        assert_eq!(h.p.script, "d. one\nd. TWO\nd. THREE");
    }

    #[test]
    fn the_finished_rows_survive_a_restart_and_a_stale_one_is_dropped() {
        let mut h = H { p: Panel::new(), out: Vec::new() };
        h.send(json!({"type": "init", "now": 10, "storage": {
            "library": library::starter(None).to_json().to_string(),
            "script": "d. one\nd. two",
            "done": json!({"lines": [0, 9], "hide": true}).to_string(),
        }}));
        assert!(h.p.hide_done);
        assert_eq!(h.p.done.iter().copied().collect::<Vec<_>>(), vec![0], "an index the script no longer has is dropped");
        assert_eq!(h.p.visible_script(), "d. two");
    }

    #[test]
    fn an_ignored_or_empty_line_is_refused_with_a_reason() {
        let mut h = H::fresh();
        h.doc();
        h.change("script", json!("## a door slams\nd. hi"));
        h.click("create");
        assert!(h.p.run.is_none());
        assert!(h.failed(), "{}", h.message());
        assert!(h.message().contains("stage direction"), "{}", h.message());
        assert!(h.ops().contains(&"ui.view"), "and the panel is still drawn");
    }

    #[test]
    fn a_line_with_nothing_to_letter_is_refused_with_a_reason() {
        let mut h = H::fresh();
        h.doc();
        h.change("script", json!("d.   \nd. hi"));
        h.click("create");
        assert!(h.failed(), "{}", h.message());
        assert!(h.p.run.is_none());
    }

    #[test]
    fn an_action_with_nothing_to_act_on_says_so_rather_than_failing() {
        // No document at all.
        let mut h = H::fresh();
        h.no_doc();
        h.change("script", json!("d. hi"));
        h.click("insert");
        assert!(h.p.run.is_none());
        assert!(h.failed(), "{}", h.message());

        // A document, but a raster layer selected: there is nothing to letter.
        let mut raster = doc_fields();
        raster["selectedLayers"] = json!([2]);
        raster["activeLayer"] = json!(2);
        raster["layers"] = json!([{"id": 2, "name": "Ink", "kind": "Raster"}]);
        let mut h = H::fresh();
        h.answer("doc.info", true, raster);
        h.change("script", json!("d. hi"));
        h.click("align");
        assert!(h.p.run.is_none());
        assert!(h.failed() && h.message().contains("type layer"), "{}", h.message());
    }

    #[test]
    fn the_document_snapshot_follows_the_hosts_own_events() {
        let mut h = H::fresh();
        h.doc();
        assert!(h.p.doc.is_some());
        // `doc.changed` carries the whole document, so there is nothing to ask for.
        h.send(json!({"type": "doc.changed", "now": 20, "revision": 13, "doc": doc_fields()}));
        assert_eq!(h.p.doc.as_ref().map(|d| d.revision), Some(12));
        assert_eq!(h.ops(), ["ui.view"], "and it is a repaint, not another round trip");
        // A null document is a fact, not a stale snapshot to keep.
        h.send(json!({"type": "doc.changed", "now": 21, "doc": Value::Null}));
        assert!(h.p.doc.is_none());
        assert!(h.p.target_layers().is_empty());
    }

    #[test]
    fn the_editor_writes_back_into_the_library() {
        let mut h = H::fresh();
        h.click("s0_edit");
        assert_eq!(h.p.page, Page::Style);
        assert!(h.p.edit.is_some());
        h.answer("doc.command", true, json!({"faces": [{"family": "Inter", "style": "Regular"}]}));
        h.change("ed_name", json!("Balloon talk"));
        h.change("ed_prefixes", json!("b.\nB."));
        h.change("ed_size", json!(17.5));
        h.change("ed_fauxBold", json!(true));
        h.change("ed_underline", json!(true));
        h.change("ed_align", json!("right"));
        h.change("ed_colorHex", json!("#ff0000"));
        h.click("ed_save");
        let s = h.p.library.style("s1").expect("the style is still there");
        assert_eq!(s.name, "Balloon talk");
        assert_eq!(s.prefixes, vec!["b.".to_string(), "B.".to_string()]);
        assert_eq!(props::char(&s.text_props, "size").and_then(Value::as_f64), Some(17.5));
        assert_eq!(props::char(&s.text_props, "syntheticBold"), Some(&json!(true)));
        assert_eq!(props::char(&s.text_props, "underline"), Some(&json!("underlineOn")));
        assert_eq!(props::para(&s.text_props, "alignment"), Some(&json!("right")));
        assert_eq!(props::color_hex(&s.text_props), "#ff0000");
        assert_eq!(s.edited, 5, "the event's own clock, which is what a merge keeps");
        assert_eq!(h.p.page, Page::Styles);
        assert!(h.p.edit.is_none());
        assert!(h.ops().contains(&"storage.set"));
        // The new prefixes are what the parser matches from now on.
        h.change("script", json!("b. hello\nplain"));
        assert_eq!(h.p.lines[0].style_id.as_deref(), Some("s1"));
    }

    #[test]
    fn the_editor_asks_for_the_faces_of_the_family_it_is_showing() {
        let mut h = H::fresh();
        h.answer("doc.command", true, json!({"families": ["Inter", "DejaVu Sans"]}));
        h.click("newStyle");
        assert_eq!(h.first_call(), "type.fonts");
        assert_eq!(h.find("doc.command")[0]["params"]["family"], "Inter");
    }

    #[test]
    fn a_new_style_can_be_made_renamed_and_deleted() {
        let mut h = H::fresh();
        let before = h.p.library.styles.len();
        h.click("newStyle");
        assert_eq!(h.p.edit.as_ref().map(|e| e.id.clone()).unwrap_or_default(), "s6", "the next free id");
        h.change("ed_name", json!("Aside"));
        h.change("ed_prefixes", json!("a."));
        h.click("ed_save");
        assert_eq!(h.p.library.styles.len(), before + 1);
        assert!(h.p.library.style("s6").is_some());
        // A style with no name is refused rather than saved as a blank row. The row button carries
        // the style's place in the list, and the new style — which has no folder — sorts to the head
        // of it, so the row for Dialogue is looked up rather than assumed.
        let row = h.p.library.styles.iter().position(|s| s.id == "s1").expect("Dialogue is still listed");
        h.click(&format!("s{row}_edit"));
        h.change("ed_name", json!("   "));
        h.click("ed_save");
        assert!(h.p.edit.is_some(), "the editor stays open on a refusal");
        assert!(h.message().contains("needs a name"), "{}", h.message());
        h.change("ed_name", json!("Dialogue"));
        h.click("ed_delete");
        assert!(h.p.library.style("s1").is_none());
        assert_eq!(h.p.page, Page::Styles);
        assert_ne!(h.p.library.default_style_id.as_deref(), Some("s1"), "the default moves off a deleted style");
    }

    #[test]
    fn styles_move_in_the_order_that_decides_which_prefix_wins() {
        let mut h = H::fresh();
        h.click("s0_edit");
        let first = h.p.library.styles[0].id.clone();
        let second = h.p.library.styles[1].id.clone();
        h.click("ed_down");
        assert_eq!(h.p.library.styles[0].id, second);
        assert_eq!(h.p.library.styles[1].id, first);
        h.click("ed_up");
        assert_eq!(h.p.library.styles[0].id, first);
        // At the end of the list there is nowhere to go, and the panel says so.
        h.click("ed_up");
        assert!(h.message().contains("as far as it goes"), "{}", h.message());
    }

    #[test]
    fn folders_can_be_made_renamed_and_removed_without_orphaning_their_styles() {
        let mut h = H::fresh();
        // A new folder opens its editor with no name; naming it is the same edit as renaming one.
        h.click("f_new");
        assert_eq!(h.p.library.folders.len(), 3);
        h.change("f_name", json!("Thoughts"));
        h.click("f_save");
        assert_eq!(h.p.library.folders[2].name, "Thoughts");
        // Renaming an existing folder. A row's button carries the folder's *place* in the list
        // (`f0_edit` is the first folder named), not its id: an imported library's ids are whatever
        // Photoshop wrote, and a widget id may only hold letters, digits, `_`, `.` and `-`.
        h.click("f1_edit");
        assert_eq!(h.p.folder.as_deref(), Some("f2"), "the folder being renamed: the second row");
        assert_eq!(h.p.folder_name, "Sound effects", "and the name it is showing in the field");
        h.change("f_name", json!("Narration"));
        h.click("f_save");
        assert_eq!(h.p.library.folders[1].name, "Narration");
        // Removing one puts its styles back in the unsorted group rather than losing them.
        assert!(h.p.library.styles.iter().any(|s| s.folder.as_deref() == Some("f2")), "the sound effect is in it");
        h.click("f1_del");
        assert_eq!(h.p.library.folders.len(), 2);
        assert!(!h.p.library.styles.iter().any(|s| s.folder.as_deref() == Some("f2")));
        assert_eq!(h.p.library.styles.len(), 5, "and no style went with it");
        // They are still reachable: their prefixes still match a line.
        h.change("script", json!("d. hello"));
        assert_eq!(h.p.lines[0].style_id.as_deref(), Some("s1"));
        assert!(h.ops().contains(&"storage.set"), "and the library is stored");
    }

    #[test]
    fn the_folder_tree_can_be_nested_and_has_its_own_order() {
        let mut h = H::fresh();
        assert_eq!(h.p.library.folder_order(), ["f1", "f2"]);
        // A new folder goes inside the one the editor's parent dropdown names.
        h.click("f_new");
        h.change("f_parent", json!("f1"));
        h.change("f_name", json!("Narration"));
        h.click("f_save");
        let narration = h.p.library.folders.iter().find(|f| f.name == "Narration").cloned().expect("the folder was made");
        assert_eq!(narration.parent_id.as_deref(), Some("f1"));
        assert_eq!(h.p.library.folder_order(), ["f1", narration.id.as_str(), "f2"], "a child sits under its parent");
        // Roots move among roots; a folder's children follow it.
        h.click("f0_down");
        assert_eq!(h.p.library.folder_order(), ["f2", "f1", narration.id.as_str()]);
        // A folder cannot be put inside its own child, and the editor stays open on the refusal.
        h.click("f1_edit");
        h.change("f_parent", json!(narration.id));
        assert!(h.message().contains("cannot go inside itself"), "{}", h.message());
        h.click("f_save");
        let dialogue = h.p.library.folder("f1").cloned().expect("Dialogue is still there");
        assert_eq!(dialogue.parent_id, None);
        // Deleting a folder takes its subfolders, and the styles in either go back to unsorted.
        h.click("f1_del");
        assert!(h.p.library.folders.iter().all(|f| f.id != narration.id));
        assert_eq!(h.p.library.folders.len(), 1);
        assert!(h.p.library.styles.iter().all(|s| s.folder.as_deref() != Some("f1")));
        assert_eq!(h.p.library.styles.len(), 5, "no style is lost");
    }

    #[test]
    fn ignore_tags_are_removed_from_the_line_before_it_is_lettered() {
        let mut h = H::fresh();
        h.change("set_ignoreTags", json!("[note]\n{sfx}"));
        assert_eq!(h.p.library.ignore_tags, ["[note]", "{sfx}"]);
        h.change("script", json!("d. hello [note] there"));
        assert_eq!(h.p.lines[0].text, "hello  there", "the tag goes, the words around it stay");
        assert!(h.p.lines[0].style_id.is_some());
        assert!(h.ops().contains(&"storage.set"), "the tags are part of the library");
    }

    #[test]
    fn the_library_travels_as_the_json_typertools_writes() {
        let mut h = H::fresh();
        h.click("io_export");
        assert!(h.p.io.contains("\"styles\"") && h.p.io.contains("\"ignoreLinePrefixes\""), "{}", h.p.io);
        let exported = h.p.io.clone();
        // Clearing and loading it back gives the same library.
        h.click("set_reset");
        assert!(h.p.library.styles.is_empty());
        h.change("io_json", json!(exported));
        h.click("io_load");
        assert_eq!(h.p.library.styles.len(), 5);
        assert_eq!(h.p.library.style("s1").map(|s| s.name.clone()), Some("Dialogue".to_string()));
        // Rubbish in the box is a message, not a half-loaded library.
        h.change("io_json", json!("not json"));
        h.click("io_load");
        assert!(h.message().contains("not JSON"), "{}", h.message());
        h.change("io_json", json!("{}"));
        h.click("io_merge");
        assert!(h.message().contains("no styles"), "{}", h.message());
        assert_eq!(h.p.library.styles.len(), 5, "and the library is untouched");
    }

    #[test]
    fn the_number_field_and_the_line_buttons_move_through_the_script() {
        let mut h = H::fresh();
        // Rows: 0 dialogue, 1 and 3 stage directions, 2 and 4 dialogue.
        h.change("script", json!("d. one\n## door\nx. two\n## the end\nd. three"));
        h.click("next");
        assert_eq!(h.p.cursor, 2, "an ignored row is skipped");
        h.click("next");
        assert_eq!(h.p.cursor, 4, "and so is the next one");
        h.click("next");
        assert_eq!(h.p.cursor, 4, "the end of the script is the end");
        h.click("prev");
        assert_eq!(h.p.cursor, 2, "and backwards skips them too");
        h.click("last");
        assert_eq!(h.p.cursor, 4, "the last line, not the last row");
        h.click("first");
        assert_eq!(h.p.cursor, 0);
        // The field carries the dialogue number, which is not the row number when rows are ignored.
        h.change("lineNo", json!(2));
        assert_eq!(h.p.cursor, 2);
        h.change("lineNo", json!(3));
        assert_eq!(h.p.cursor, 4);
        h.change("lineNo", json!(99));
        assert_eq!(h.p.cursor, 4, "a line that is not there leaves the cursor alone");
        h.change("lineNo", json!(0));
        assert_eq!(h.p.cursor, 4, "and dialogue numbers start at one");
    }

    #[test]
    fn the_host_says_which_widgets_it_draws_and_the_view_uses_them() {
        let mut h = H { p: Panel::new(), out: Vec::new() };
        h.send(json!({"type": "init", "now": 10, "storage": {},
                      "host": {"name": "PhotoCraft", "widgets": ["list", "scroll", "scroll", "", 7, "label.color"]}}));
        assert!(h.p.has("list") && h.p.has("scroll") && h.p.has("label.color"));
        assert!(!h.p.has("button.icon"), "a feature the host did not name is absent");
        assert_eq!(h.p.widgets, ["list", "scroll", "label.color"], "each name once");
        let view = h.out.iter().find(|o| o["op"] == "ui.view").map(|o| o["view"].to_string()).unwrap_or_default();
        assert!(view.contains("\"type\":\"scroll\""), "the panes are drawn: {view}");
        assert!(!view.contains("\"icon\":"), "but not the buttons this host cannot draw: {view}");

        // A host that says nothing gets the plain page: an old module and an old host still agree.
        let old = H::fresh();
        assert!(old.p.widgets.is_empty());
        let view = old.out.iter().find(|o| o["op"] == "ui.view").map(|o| o["view"].to_string()).unwrap_or_default();
        assert!(!view.contains("\"type\":\"list\""), "{view}");
        assert!(!view.contains("\"type\":\"scroll\""), "{view}");
    }

    #[test]
    fn clicking_a_script_row_works_on_that_line() {
        let mut h = H::fresh();
        h.p.widgets = ["list", "scroll", "label.color", "label.font", "button.icon"].iter().map(|s| s.to_string()).collect();
        h.p.library = library::starter(None);
        h.p.script = "d. one\nd. two\n## a note\nd. three".into();
        h.p.reparse();
        h.click_row("scriptLine", 2);
        assert_eq!(h.p.cursor, 2, "the row index is the script row");
        assert!(h.out.iter().any(|o| o["op"] == "ui.view" && o["view"].to_string().contains("\"selected\":2")), "and the view says so");
        // A row that is not drawn leaves the line where it is, and a click with no value is a
        // press the host did not give an index for — neither is an error.
        h.click_row("scriptLine", 99);
        assert_eq!(h.p.cursor, 2);
        h.click("scriptLine");
        assert_eq!(h.p.cursor, 2);
    }

    #[test]
    fn a_hidden_finished_row_does_not_shift_the_row_a_click_names() {
        // TypeR hides the rows it has lettered. The list then draws the remaining rows, and the
        // click's index is among *those*: mapping it as a raw row would letter the wrong line.
        let mut h = H::fresh();
        h.p.library = library::starter(None);
        h.p.script = "d. one\nd. two\nd. three\nd. four".into();
        h.p.reparse();
        h.p.hide_done = true;
        h.p.done.insert(0);
        h.p.done.insert(2);
        h.click_row("scriptLine", 1);
        assert_eq!(h.p.cursor, 3, "the second drawn row is the fourth script row");
    }

    #[test]
    fn an_answer_to_something_the_panel_did_not_ask_is_dropped() {
        let mut h = H::fresh();
        h.doc();
        h.change("script", json!("d. hello"));
        // A result with no id at all, and one for an operation this panel never sent.
        assert_eq!(h.send(json!({"type": "result", "ok": true})).ops(), ["ui.view"]);
        assert_eq!(h.send(json!({"type": "result", "id": 9999, "ok": true, "layer": 9})).ops(), ["ui.view"]);
        // A cancel with no run, and an event this version of the panel has never heard of.
        assert_eq!(h.send(json!({"type": "cancel", "now": 5})).ops(), ["ui.view"]);
        h.send(json!({"type": "widget.melted"}));
        assert!(h.message().contains("does not know"), "{}", h.message());
        assert_eq!(h.ops(), ["ui.view"]);
        // A click on an id no one drew.
        assert_eq!(h.click("s99_apply").ops(), ["ui.view"]);
        assert!(h.p.run.is_none());
        // A hidden panel draws nothing at all…
        assert!(h.send(json!({"type": "panel.visible", "visible": false})).out.is_empty());
        // …and a panel that comes back asks what it missed.
        assert!(h.send(json!({"type": "panel.visible", "visible": true})).ops().contains(&"doc.info"));
        // Through all of it the panel is still usable.
        h.click("create");
        assert!(h.p.run.is_some());
    }

    #[test]
    fn a_run_that_the_host_refuses_leaves_a_message_and_no_run() {
        let mut h = H::fresh();
        h.doc();
        h.change("script", json!("d. hello"));
        h.click("create");
        h.answer("doc.info", true, doc_fields());
        assert_eq!(h.first_call(), "type.create");
        h.answer("doc.command", false, json!({"error": "the font “Tahoma” is not installed"}));
        assert!(h.p.run.is_none());
        assert!(h.failed(), "{}", h.message());
        assert!(h.message().contains("Tahoma"), "{}", h.message());
        assert!(h.ops().contains(&"ui.notice"), "a failure is shown where a letterer cannot miss it");
        // And the panel still works afterwards: the abandoned operation is not left outstanding.
        h.click("create");
        assert!(h.ops().contains(&"doc.info"));
    }

    #[test]
    fn a_cancel_discards_the_run_and_its_outstanding_step() {
        let mut h = H::fresh();
        h.doc();
        h.change("script", json!("d. hello"));
        h.click("create");
        h.answer("doc.info", true, doc_fields());
        assert!(h.p.run.is_some());
        h.send(json!({"type": "cancel", "now": 30}));
        assert!(h.p.run.is_none());
        assert!(h.message().contains("Cancelled"), "{}", h.message());
        // The answer to the step that was outstanding when the letterer pressed Cancel must not be
        // handed to whatever runs next.
        h.click("create");
        h.answer("doc.info", true, doc_fields());
        let stale = h.answer("doc.command", true, json!({"layer": 9}));
        assert_eq!(stale.first_call(), "type.info", "the answer went to the run that is actually running");
    }

    #[test]
    fn a_second_click_while_an_action_is_running_is_ignored() {
        let mut h = H::fresh();
        h.doc();
        h.change("script", json!("d. hello"));
        h.click("create");
        assert!(h.p.run.is_some());
        let out = h.click("create");
        assert_eq!(out.ops(), ["ui.view"], "no second run is started");
        // Including the buttons that would otherwise start one.
        assert_eq!(h.click("sizeUp").ops(), ["ui.view"]);
        assert_eq!(h.click("s4_apply").ops(), ["ui.view"]);
        assert_eq!(h.p.run.as_ref().map(|r| r.kind), Some(Kind::Create));
    }

    #[test]
    fn the_size_buttons_step_by_the_size_increment_setting() {
        let mut h = H::fresh();
        h.doc();
        h.click("sizeUp");
        assert_eq!(h.first_call(), "type.info", "a resize starts by measuring the layer");
        assert_eq!(h.p.run.as_ref().map(|r| r.kind), Some(Kind::Resize));
        let delta = h.p.run.as_ref().map(|r| r.delta).unwrap_or(0.0);
        assert!((delta - 1.0).abs() < 1e-9, "TypeR's default increment is one unit: {delta}");
        // The setting is TypeR's `textSizeIncrement`, an integer 1–99.
        let mut h = H::fresh();
        h.doc();
        h.change("set_sizeIncrement", json!(3));
        h.click("sizeDown");
        assert_eq!(h.p.run.as_ref().map(|r| r.delta), Some(-3.0));
        // With no type layer to work on, nothing starts.
        let mut h = H::fresh();
        h.answer("doc.info", true, json!({"width": 10, "height": 10, "resolution": 72, "layers": []}));
        h.click("sizeDown");
        assert!(h.p.run.is_none());
        assert!(h.message().contains("Select a type layer"), "{}", h.message());
    }

    #[test]
    fn a_library_row_applies_that_style_and_not_the_lines() {
        let mut h = H::fresh();
        h.doc();
        h.change("script", json!("d. hello"));
        h.click("s4_apply");
        assert_eq!(h.first_call(), "type.info");
        let run = h.p.run.as_ref().expect("a run");
        assert_eq!(run.kind, Kind::Apply);
        assert_eq!(run.style_name, "Sound effect", "the fifth style of the starter library");
        assert_eq!(run.text, "", "applying a style does not touch the text");
    }

    #[test]
    fn markdown_bold_and_italic_become_character_runs() {
        let mut h = H::fresh();
        h.doc();
        h.change("set_markdown", json!(true));
        h.change("script", json!("d. I **am** *here*"));
        h.click("create");
        h.answer("doc.info", true, doc_fields());
        let call = h.call("type.create").cloned().expect("the layer is made");
        assert_eq!(call["params"]["text"], "I am here", "the markers are gone from what the layer says");
        h.answer("doc.command", true, json!({"layer": 9}));
        let edit = h.call("type.edit").cloned().expect("the weights go on as runs");
        assert_eq!(edit["params"]["layer"], 9);
        assert_eq!(
            edit["params"]["runs"],
            json!([
                {"start": 2, "end": 4, "fauxBold": true},
                {"start": 5, "end": 9, "fauxItalic": true},
            ])
        );
        // The measurement still follows, and the run then finishes normally.
        h.answer("doc.command", true, type_fields());
        assert_eq!(h.first_call(), "type.info");
        h.drive();
        assert!(h.p.message.as_ref().is_some_and(|m| m.text.contains("I am here")), "{:?}", h.p.message);
    }

    #[test]
    fn the_markdown_markers_stay_when_the_setting_is_off() {
        let mut h = H::fresh();
        h.doc();
        h.change("script", json!("d. **loud**"));
        h.click("create");
        h.answer("doc.info", true, doc_fields());
        let call = h.call("type.create").cloned().expect("the layer is made");
        assert_eq!(call["params"]["text"], "**loud**", "TypeR's markdown is off by default");
        h.answer("doc.command", true, json!({"layer": 9}));
        assert_eq!(h.first_call(), "type.info", "and no run edit is inserted");
    }

    #[test]
    fn internal_padding_and_direction_reach_the_box_and_the_style() {
        let mut h = H::fresh();
        h.doc();
        h.change("set_padding", json!(20.0));
        h.change("set_direction", json!("rtl"));
        h.change("script", json!("d. hello"));
        h.click("create");
        h.answer("doc.info", true, doc_fields());
        let call = h.call("type.create").cloned().expect("the layer is made");
        // The 180 px-wide marquee: 0.9 of it is 162, less 20 px of padding a side.
        assert_eq!(call["params"]["box"], json!([110.0, 60.0, 122.0, 2100.0]));
        assert_eq!(call["params"]["direction"], "rtl");
        // A setting outside TypeR's range is brought inside it, never sent as it came.
        let mut h = H::fresh();
        h.change("set_padding", json!(500.0));
        assert_eq!(h.p.padding, 100.0);
        h.change("set_padding", json!(-20.0));
        assert_eq!(h.p.padding, 0.0);
        h.change("set_sizeIncrement", json!(0));
        assert_eq!(h.p.size_increment, 1.0);
        h.change("set_styleStep", json!(0));
        assert_eq!(h.p.style_step, 0.1);
    }

    #[test]
    fn align_only_reshapes_the_box_when_resize_on_centre_is_on() {
        let mut h = H::fresh();
        h.doc();
        h.click("align");
        h.answer("doc.info", true, doc_fields());
        h.answer("doc.command", true, type_fields());
        let edit = h.call("type.edit").cloned().expect("the layer is centred");
        assert!(edit["params"].get("box").is_none(), "the box is left alone: {edit}");
        assert!(edit["params"].get("move").is_some());
        // On: the balloon's box, less the default 10 px of padding a side.
        let mut h = H::fresh();
        h.doc();
        h.change("set_resizeOnCenter", json!(true));
        h.click("align");
        h.answer("doc.info", true, doc_fields());
        h.answer("doc.command", true, type_fields());
        let edit = h.call("type.edit").cloned().expect("the layer is reshaped");
        assert_eq!(edit["params"]["box"], json!([110.0, 60.0, 142.0, 2100.0]));
    }

    #[test]
    fn the_quick_style_size_edit_is_stored_in_the_styles_own_unit() {
        let mut h = H::fresh();
        let size = |h: &H| props::char(&h.p.library.styles[0].text_props, "size").and_then(Value::as_f64).unwrap_or(0.0);
        let before = size(&h);
        h.click("s0_sizeUp");
        assert!((size(&h) - (before + 0.1)).abs() < 1e-9, "the 0.1 default step: {}", size(&h));
        assert!(h.ops().contains(&"storage.set"), "the library is stored");
        h.change("set_styleStep", json!(0.5));
        h.click("s0_sizeDown");
        // The nudge snaps to a multiple of the new step (TypeR's `normalizeSizeStep`), so 14.1
        // less 0.5 is 13.5, not 13.6.
        assert!((size(&h) - 13.5).abs() < 1e-9, "{}", size(&h));
        h.change("s0_size", json!(20.0));
        assert!((size(&h) - 20.0).abs() < 1e-9);
        // Rubbish in the field changes nothing.
        h.change("s0_size", json!("nonsense"));
        assert!((size(&h) - 20.0).abs() < 1e-9);
    }

    #[test]
    fn a_state_saves_the_script_the_line_and_the_style_and_loads_them_back() {
        let mut h = H::fresh();
        h.change("script", json!("d. one\nd. two"));
        h.click("next");
        assert_eq!(h.p.cursor, 1);
        h.change("state_name", json!("Scene 3"));
        h.click("state_save");
        assert!(h.p.states.contains_key("Scene 3"));
        assert!(h.ops().contains(&"storage.set"), "the states are stored");
        h.change("script", json!("d. something else"));
        h.click("st0_load");
        assert_eq!(h.p.script, "d. one\nd. two");
        assert_eq!(h.p.cursor, 1, "the line it was on");
        assert_eq!(h.p.page, Page::Lettering, "loading a state goes back to the panes");
        h.click("st0_del");
        assert!(h.p.states.is_empty());
        // A state with no name is refused rather than saved as an unnamed key.
        h.click("state_save");
        assert!(h.message().contains("needs a name"), "{}", h.message());
    }

    #[test]
    fn the_type_r_settings_survive_a_restart() {
        let mut h = H { p: Panel::new(), out: Vec::new() };
        h.send(json!({"type": "init", "now": 10, "storage": {
            "settings": json!({
                "scale": 120, "page": "settings", "markdown": true, "direction": "rtl",
                "internalPadding": 25, "resizeTextBoxOnCenter": true,
                "textSizeIncrement": 5, "styleSizeStep": 0.5, "showQuickStyleSize": false,
            }).to_string(),
        }}));
        assert_eq!(h.p.scale, 120.0);
        assert_eq!(h.p.page, Page::Settings);
        assert!(h.p.markdown);
        assert_eq!(h.p.direction, "rtl");
        assert_eq!(h.p.padding, 25.0);
        assert!(h.p.resize_on_center);
        assert_eq!(h.p.size_increment, 5.0);
        assert_eq!(h.p.style_step, 0.5);
        assert!(!h.p.quick_size);
    }

    #[test]
    fn a_folder_exports_the_single_folder_shape_type_r_writes() {
        let mut h = H::fresh();
        let folder = h.p.library.folders[0].clone();
        let has_styles = !h.p.library.styles_in(Some(&folder.id)).is_empty();
        // Give the folder a style so the export has something in it.
        if !has_styles
            && let Some(s) = h.p.library.styles.first_mut()
        {
            s.folder = Some(folder.id.clone());
        }
        h.click("f0_export");
        let exported: Value = serde_json::from_str(&h.p.io).expect("the box holds JSON");
        assert_eq!(exported["name"], json!(folder.name));
        assert!(exported["exportedStyles"].is_array());
        assert!(exported.get("styles").is_none(), "the single-folder shape has exportedStyles: {}", h.p.io);
        assert!(exported.get("version").is_none());
        assert!(h.message().contains("Help page"), "{}", h.message());
    }

    #[test]
    fn the_text_scale_reaches_the_parameters_a_line_is_lettered_with() {
        let mut h = H::fresh();
        h.doc();
        h.change("script", json!("d. hello"));
        h.change("textScale", json!(50.0));
        assert_eq!(h.p.scale, 50.0);
        h.click("create");
        h.answer("doc.info", true, doc_fields());
        let call = h.call("type.create").cloned().expect("a create");
        assert_eq!(call["params"]["size"], json!(7.0), "the style's 14 pt at 50 %");
        assert_eq!(call["params"]["leading"], json!("auto"), "the starter styles are auto-led");
        // 100 % is the style's own size.
        let mut h = H::fresh();
        h.doc();
        h.change("script", json!("d. hello"));
        h.click("create");
        h.answer("doc.info", true, doc_fields());
        let call = h.call("type.create").cloned().expect("a create");
        assert_eq!(call["params"]["size"], json!(14.0));
    }

    #[test]
    fn the_settings_are_remembered_and_the_ignore_prefix_is_the_librarys_own() {
        let mut h = H::fresh();
        let out = h.change("set_ignore", json!("#\n--"));
        assert!(out.ops().contains(&"storage.set"));
        assert_eq!(h.p.library.ignore_prefixes, vec!["#", "--"]);
        h.change("script", json!("# note\nd. hi"));
        assert!(h.p.lines[0].ignored);
        let out = h.change("set_pointText", json!(true));
        assert!(out.ops().contains(&"storage.set"));
        assert!(h.p.library.point_text);
        h.change("page", json!("help"));
        assert_eq!(h.p.page, Page::Help);
        assert!(h.ops().contains(&"storage.set"));
        h.change("textScale", json!(10000.0));
        assert_eq!(h.p.scale, 999.0, "the field is clamped to what TyperTools allows");
        h.change("textScale", json!("nonsense"));
        assert_eq!(h.p.scale, 100.0, "and rubbish in it is the default, not a broken scale");
        h.click("set_scaleDefault");
        assert_eq!(h.p.scale, 100.0);
    }

    #[test]
    fn the_style_of_a_layer_can_be_copied_into_the_editor() {
        let mut h = H::fresh();
        h.doc();
        h.click("s0_edit");
        h.click("ed_copy");
        assert_eq!(h.first_call(), "type.info");
        assert_eq!(h.find("doc.command")[0]["params"]["layer"], 4);
        h.answer("doc.command", true, type_fields());
        let e = h.p.edit.as_ref().expect("the editor is still open");
        assert_eq!(props::size_pt(&e.props, 300.0), 14.0, "the size the layer reported comes back as a style size");
        assert!(h.message().contains("Copied"), "{}", h.message());
        // A layer whose type cannot be read leaves the editor as it was.
        let mut h = H::fresh();
        h.doc();
        h.click("s0_edit");
        let before = h.p.edit.as_ref().map(|e| e.props.clone());
        h.click("ed_copy");
        h.answer("doc.command", false, json!({"error": "layer 4 is not a type layer"}));
        assert!(h.failed(), "{}", h.message());
        assert_eq!(h.p.edit.as_ref().map(|e| e.props.clone()), before);
    }

    #[test]
    fn a_long_script_is_kept_within_what_a_widget_can_hold() {
        let mut h = H::fresh();
        let huge = "d. ".to_string() + &"x".repeat(view::SCRIPT_MAX + 100);
        h.change("script", json!(huge));
        assert_eq!(h.p.script.chars().count(), view::SCRIPT_MAX);
        assert_eq!(h.p.lines.len(), 1, "one very long line, still one line");
    }

    #[test]
    fn a_huge_script_does_not_take_the_panel_down() {
        // The parser is handed whatever the user pasted; this is the shape of "whatever".
        let mut h = H::fresh();
        for text in ["", "\n\n\n", &"\n".repeat(10_000), &"d.".repeat(20_000), "\u{0}\u{1}\u{feff}", &"x".repeat(view::SCRIPT_MAX)] {
            h.change("script", json!(text));
            assert!(h.ops().contains(&"ui.view"));
        }
        h.click("create");
        assert!(h.ops().contains(&"ui.view"));
    }

    #[test]
    fn the_reply_is_always_something_the_host_can_parse() {
        // Every event a panel can be sent, including nonsense, produces ops the host accepts. The
        // view's own rules are checked in `view.rs`; this is the envelope around it.
        let mut h = H::fresh();
        for event in [
            json!({"type": "panel.visible", "visible": true}),
            json!({"type": "panel.visible", "visible": false}),
            json!({"type": "result", "id": 1, "ok": true}),
            json!({"type": "result", "id": 1, "ok": false}),
            json!({"type": "result"}),
            json!({"type": "cancel"}),
            json!({"type": "ui.change", "id": "nonsense", "value": 1}),
            json!({"type": "ui.change", "value": Value::Null}),
            json!({"type": "ui.click"}),
            json!({"type": "ui.click", "id": "nonsense"}),
            json!({"type": "ui.click", "id": "s0_apply"}),
            json!({"type": "ui.click", "id": "s99_edit"}),
            json!({"type": "ui.click", "id": "f99_del"}),
            json!({"type": "ui.click", "id": "ed_save"}),
            json!({"type": "ui.click", "id": "ed_delete"}),
            json!({"type": "ui.click", "id": "ed_copy"}),
            json!({"type": "ui.click", "id": "io_export"}),
            json!({"type": "ui.click", "id": "io_load"}),
            json!({"type": "ui.click", "id": "newStyle"}),
            json!({"type": "ui.click", "id": "set_starter"}),
            json!({"type": "ui.click", "id": "set_reset"}),
            json!({"type": "doc.changed", "doc": doc_fields()}),
            json!({"type": "doc.changed"}),
            json!({}),
        ] {
            let out = h.send(event.clone());
            // Hiding the panel is the one event that is answered with nothing at all: nobody is
            // looking at it, so there is nothing to draw and nothing worth a round trip. Every other
            // event has to come back with at least the view.
            if event.get("visible").and_then(Value::as_bool) == Some(false) {
                assert!(out.out.is_empty(), "a hidden panel draws nothing: {event}");
                continue;
            }
            assert!(!out.out.is_empty(), "every event is answered: {event}");
            for op in &out.out {
                let name = op.get("op").and_then(Value::as_str).unwrap_or_default();
                assert!(
                    ["ui.view", "ui.set", "ui.notice", "ui.progress", "doc.info", "doc.command", "storage.set"].contains(&name),
                    "{name} is not an operation the ABI has: {op}"
                );
                // Whatever an op is, it is an object with an op and nothing unparseable in it.
                assert!(op.is_object());
                assert!(serde_json::to_string(op).is_ok());
            }
        }
    }

    #[test]
    fn a_prefix_is_guessed_from_a_line_only_when_it_looks_like_one() {
        assert_eq!(prefix_from_line("d. hello"), "d.");
        assert_eq!(prefix_from_line("x: BANG"), "x:");
        assert_eq!(prefix_from_line("hello there"), "", "a word with no tag is not a prefix");
        assert_eq!(prefix_from_line("an-extremely-long-first-word"), "");
    }
}
