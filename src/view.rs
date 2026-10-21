//! The widget tree the panel draws.
//!
//! A panel does not draw: it describes, and the host renders the description with the application's
//! own theme. So this file is a page of builders (`column`, `label`, `button`, …) and one function
//! per pane, and the only thing that matters is that what comes out is something the host will
//! accept — the host's own checks (PhotoCraft's `crates/plugins/src/view.rs`) are the contract, and a
//! tree that breaks one of them is refused *whole*, which stops the panel. Two rules follow from that
//! and are worth stating because they are not obvious from the code below:
//!
//! * **A `choice`'s current value must be one of its options.** A style naming a font this machine
//!   does not have is normal — a library travels between machines — so [`choice`] adds the current
//!   value when the list does not hold it, rather than leaving the panel to remember to.
//! * **Numbers and labels are clamped.** A hand-edited library can hold a leading of 0, a NaN
//!   tracking or a 300-character style name; [`number`] and [`label`] fit them to the range the
//!   widget set can express. A refusal here would take the whole panel down over one bad style.
//!
//! What the panel draws depends on what this host says it has (`init.host.widgets`). A host that
//! draws `list`, `scroll`, `label.color`, `label.font` and `button.icon` gets TypeR's own shape: a
//! line-numbered script list with the line it is working on highlighted, style names set in their
//! own font beside a colour swatch, glyph buttons and independently scrolling panes. A host without
//! them gets the plain page this module has always sent — and says so in Help, since a colour
//! swatch or a gutter that was never sent reads as a missing feature, not as a host that lacks one.

use serde_json::{Value, json};

use crate::library::{PREFIX_COLORS, Style};
use crate::panel::{Page, Panel};
use crate::props;
use crate::script;

/// The longest script the panel will hold, in characters. Equal to the host's `view::MAX_TEXT_LENGTH`:
/// a widget asking for more is refused, so the two move together.
pub const SCRIPT_MAX: usize = 65_536;

/// The longest label the host will draw (`view::MAX_LABEL`), and the longest text a `text` field on
/// this panel's pages holds — a style's name is a field's worth of characters, not a script's.
pub const LABEL_MAX: usize = 256;

/// How many library rows a page draws before it says "and more". The host allows 512 widgets in a
/// view; three per row plus the editor leaves room for fifty styles, which is more than a letterer
/// scrolls past. Everything beyond is reachable by prefix and by the JSON editor.
pub const MAX_ROWS: usize = 50;

/// The most rows a `list` widget holds (the host's `view::MAX_LIST_ROWS`). A longer script keeps
/// its rows in the script box; the list is a view of what it can draw.
pub const LIST_MAX: usize = 1000;

// --- builders ---------------------------------------------------------------------------------

pub fn column(children: Vec<Value>) -> Value {
    json!({"type": "column", "children": children})
}

pub fn row(children: Vec<Value>) -> Value {
    json!({"type": "row", "children": children})
}

pub fn separator() -> Value {
    json!({"type": "separator"})
}

/// A collapsible group. The host remembers whether it is open by its title, so a title is part of
/// the widget's identity: "Done (3)" becoming "Done (4)" would reopen a section the letterer had
/// closed. Callers keep the title stable for that reason.
pub fn section(title: &str, collapsed: bool, children: Vec<Value>) -> Value {
    json!({"type": "section", "title": cut(title), "collapsed": collapsed, "children": children})
}

/// Plain text. Anything over the host's label limit is cut: a style's name is whatever the library
/// says, and one long name must not cost the letterer their panel.
pub fn label(text: impl AsRef<str>) -> Value {
    json!({"type": "label", "text": cut(text.as_ref())})
}

pub fn muted(text: impl AsRef<str>) -> Value {
    json!({"type": "label", "text": cut(text.as_ref()), "level": "muted"})
}

pub fn warning(text: impl AsRef<str>) -> Value {
    json!({"type": "label", "text": cut(text.as_ref()), "level": "warning"})
}

pub fn error(text: impl AsRef<str>) -> Value {
    json!({"type": "label", "text": cut(text.as_ref()), "level": "error"})
}

/// A label the host may draw in a colour (`#rrggbb`) and a host font family. Both are left out when
/// they would fail the host's checks rather than sent and refused with the whole view: a library is
/// JSON a letterer can edit, and one odd colour must not cost them the panel.
fn styled(text: impl AsRef<str>, color: Option<&str>, font: Option<&str>) -> Value {
    let mut v = json!({"type": "label", "text": cut(text.as_ref())});
    if let Some(c) = color.filter(|c| is_hex(c)) {
        v["color"] = json!(c);
    }
    if let Some(f) = font.filter(|f| !f.is_empty()) {
        v["font"] = json!(cut_to(f, FONT_MAX));
    }
    v
}

/// The family a style letters in, for a label that sets the text in its own font — `None` when this
/// host does not draw label fonts, or the style names none yet (a starter style laid down before the
/// host listed its fonts).
fn family_font(p: &Panel, props: &Value) -> Option<String> {
    if !p.has("label.font") {
        return None;
    }
    let family = props::family(props);
    (!family.is_empty()).then_some(family)
}

/// A button. `enabled: false` greys it out and the host sends nothing when it is pressed — the panel
/// says what it cannot do rather than offering a button that refuses.
pub fn button(id: &str, text: impl AsRef<str>, primary: bool, enabled: bool) -> Value {
    json!({"type": "button", "id": id, "label": cut(text.as_ref()), "primary": primary, "enabled": enabled})
}

/// A button that draws a glyph, with `tooltip` as its hover hint (the host's `button.icon`). The
/// icon is cut to the eight characters the host accepts: a caller passing a word gets a glyph
/// button it can still press, not a refused view.
pub fn icon_button(id: &str, icon: &str, tooltip: &str, primary: bool, enabled: bool) -> Value {
    json!({
        "type": "button", "id": id, "label": cut(tooltip), "icon": cut_to(icon, 8),
        "primary": primary, "enabled": enabled,
    })
}

/// A list of single-line rows. `selected` is the row the host highlights, 0-based; `numbered` adds
/// the host's line-number gutter. A click sends `ui.click` with the row's index as its value.
pub fn list(id: &str, selected: Option<usize>, numbered: bool, rows: Vec<Value>) -> Value {
    let rows: Vec<Value> = rows.into_iter().take(LIST_MAX).collect();
    json!({"type": "list", "id": id, "selected": selected, "numbered": numbered, "rows": rows})
}

/// One row of a [`list`]: its text, and optionally the colour (`#rrggbb`) and font family the host
/// draws it in. Both are cut to what the host accepts rather than allowing one odd style to refuse
/// the whole view.
pub fn list_row(text: impl AsRef<str>, color: Option<&str>, font: Option<&str>) -> Value {
    let mut row = json!({"text": cut_to(text.as_ref(), LIST_ROW_CHARS)});
    if let Some(c) = color.filter(|c| is_hex(c)) {
        row["color"] = json!(c);
    }
    if let Some(f) = font.filter(|f| !f.is_empty()) {
        row["font"] = json!(cut_to(f, FONT_MAX));
    }
    row
}

/// A pane with its own scrollbar and a pinned footer. Height-less panes in a column share the free
/// height by `weight`, so the script and the library scroll independently and neither pushes the
/// other off the panel.
pub fn scroll(id: &str, weight: u8, children: Vec<Value>, footer: Vec<Value>) -> Value {
    json!({"type": "scroll", "id": id, "weight": weight.clamp(1, 16), "children": children, "footer": footer})
}

/// Whether `c` is a `#rrggbb` colour, which is the only shape the host's colour fields accept.
fn is_hex(c: &str) -> bool {
    c.strip_prefix('#').is_some_and(|hex| hex.len() == 6 && hex.bytes().all(|b| b.is_ascii_hexdigit()))
}

/// The longest family name the host resolves (its `view::MAX_FONT_NAME`).
pub const FONT_MAX: usize = 128;

/// The longest text one list row carries. A script row can be as long as the script; the list draws
/// one line and elides it, so carrying more than a wide panel can show is memory spent for nothing.
pub const LIST_ROW_CHARS: usize = 400;

/// A field. `max_length` is the host's cap on what can be typed into it — and on what it will carry,
/// so a value longer than its own field holds is cut here rather than refused by the host: a style
/// whose name was pasted in at length must not cost the letterer their panel.
pub fn text(id: &str, value: &str, placeholder: &str, multiline: bool, max_length: usize) -> Value {
    let max_length = max_length.min(SCRIPT_MAX);
    json!({
        "type": "text", "id": id, "value": cut_to(value, max_length), "placeholder": cut(placeholder),
        "multiline": multiline, "maxLength": max_length,
    })
}

/// A number field or slider, with the value fitted to the range the host insists on: finite, and
/// inside `min..=max`.
pub fn number(id: &str, value: f64, min: f64, max: f64, step: f64, slider: bool) -> Value {
    let value = finite(value, 0.0);
    // A bound that is not a number says nothing about the range, so the value itself stands in for
    // it: a style carrying an absurd minimum is shown as what it holds rather than clamped into a
    // range the library never claimed.
    let (min, max) = (finite(min, value), finite(max, value));
    let (min, max) = if min <= max { (min, max) } else { (max, min) };
    let step = if step.is_finite() && step > 0.0 { step } else { 1.0 };
    json!({"type": "number", "id": id, "value": value.clamp(min, max), "min": min, "max": max, "step": step, "slider": slider})
}

pub fn toggle(id: &str, value: bool, text: impl AsRef<str>) -> Value {
    json!({"type": "toggle", "id": id, "value": value, "label": cut(text.as_ref())})
}

/// A dropdown whose options are `(value, label)`. The current value is added when the list does not
/// already hold it, and the list is trimmed to the host's maximum — the two things that would
/// otherwise make the host refuse the whole view.
pub fn choice(id: &str, value: &str, options: Vec<(String, String)>) -> Value {
    let current = if value.is_empty() { "-" } else { value };
    let mut list: Vec<Value> = Vec::new();
    let mut seen: Vec<String> = Vec::new();
    if !options.iter().any(|(v, _)| v == current) {
        list.push(json!({"value": current, "label": format!("{current} (not installed)")}));
        seen.push(current.to_string());
    }
    for (v, l) in options {
        if v.is_empty() || v.len() > LABEL_MAX || seen.contains(&v) {
            continue;
        }
        if list.len() >= 256 {
            break;
        }
        seen.push(v.clone());
        list.push(json!({"value": v, "label": cut(&l)}));
    }
    if current.is_empty() && !seen.iter().any(|s| s == current) {
        list.insert(0, json!({"value": current, "label": "—"}));
    }
    json!({"type": "choice", "id": id, "value": current, "options": list})
}

pub fn progress() -> Value {
    json!({"type": "progress", "fraction": null})
}

/// Cuts text to the host's label limit, on a character boundary.
fn cut(text: &str) -> String {
    cut_to(text, LABEL_MAX)
}

/// Cuts text to `max` characters, marking the cut so it is visible rather than looking like the
/// value itself. One character of the budget goes to the mark.
fn cut_to(text: &str, max: usize) -> String {
    if max == 0 {
        return String::new();
    }
    if text.chars().count() <= max {
        return text.to_string();
    }
    text.chars().take(max - 1).collect::<String>() + "…"
}

fn finite(v: f64, fallback: f64) -> f64 {
    if v.is_finite() { v } else { fallback }
}

// --- reading a style's properties --------------------------------------------------------------
//
// The editor's fields are a view onto `textProps` (TyperTools' own shape, in `props.rs`), not a
// second copy of it: a field shows what the style holds and writes straight back into it, so an
// attribute the editor does not offer survives being edited.

fn n(v: Option<&Value>, fallback: f64) -> f64 {
    v.and_then(Value::as_f64).filter(|n| n.is_finite()).unwrap_or(fallback)
}

fn b(v: Option<&Value>) -> bool {
    v.and_then(Value::as_bool).unwrap_or(false)
}

fn s(v: Option<&Value>) -> String {
    v.and_then(Value::as_str).unwrap_or_default().to_string()
}

/// One character property of the style being edited.
fn c(p: &Panel, key: &str) -> Option<Value> {
    p.edit.as_ref().and_then(|e| props::char(&e.props, key)).cloned()
}

/// One paragraph property of the style being edited.
fn q(p: &Panel, key: &str) -> Option<Value> {
    p.edit.as_ref().and_then(|e| props::para(&e.props, key)).cloned()
}

/// A length property, in the style's own unit — which is what the editor shows and edits, so a
/// library written in millimetres is not silently rewritten into pixels.
fn length(p: &Panel, key: &str, char_prop: bool, fallback: f64) -> f64 {
    let v = if char_prop { c(p, key) } else { q(p, key) };
    n(v.as_ref(), fallback)
}

/// The unit suffix an editor field shows, from the style's `typeUnit`.
fn unit_suffix(p: &Panel) -> &'static str {
    match p.edit.as_ref().map(|e| props::type_unit(&e.props)) {
        Some("mmUnit") => " mm",
        Some("inchesUnit") => " in",
        Some("picasUnit") => " pc",
        Some("pointsUnit") => " pt",
        _ => " px",
    }
}

/// Cuts text to at most `max` UTF-8 bytes, on a character boundary. Widget option values and ids
/// are bounded in bytes, and a library is JSON that can name any string; cutting mid-character
/// would make the value invalid.
fn cut_bytes(text: &str, max: usize) -> String {
    if text.len() <= max {
        return text.to_string();
    }
    let mut end = max;
    while end > 0 && !text.is_char_boundary(end) {
        end -= 1;
    }
    text[..end].to_string()
}

/// One dropdown option, with its value and label cut to what a widget may carry.
fn option(name: &str) -> (String, String) {
    (cut_bytes(name, 256), cut_to(name, 256))
}

/// The family list for the dropdown: what the host has, then whatever the style names. A style whose
/// font is not installed here is offered anyway — a library travels between machines — and says so,
/// because "the font this style was written in is not on this machine" is something the letterer has
/// to be told, not something to hide behind a dropdown that silently shows a different font.
///
/// Returns the value the dropdown must show as well as its options: the host requires the value to be
/// one of them, so an over-long stored name is cut the same way as the option it is matched against.
fn families(p: &Panel) -> (String, Vec<(String, String)>) {
    let stored = p.edit.as_ref().map(|e| props::family(&e.props)).unwrap_or_default();
    let current = cut_bytes(&stored, 256);
    let mut out: Vec<(String, String)> = Vec::new();
    for f in p.fonts.families.iter().take(200) {
        let o = option(f);
        if !out.iter().any(|(v, _)| *v == o.0) {
            out.push(o);
        }
    }
    if !current.is_empty() && !out.iter().any(|(v, _)| *v == current) {
        out.insert(0, (current.clone(), cut_to(&format!("{stored} (not installed)"), 256)));
    }
    (current, out)
}

/// The face list for the family that is selected. Faces are only known for the family the host was
/// last asked about (`type.fonts {"family": …}`), and the current face is always offered.
fn faces(p: &Panel) -> (String, Vec<(String, String)>) {
    let stored = p.edit.as_ref().map(|e| props::font_style(&e.props)).unwrap_or_default();
    let current = cut_bytes(&stored, 256);
    let family = p.edit.as_ref().map(|e| props::family(&e.props)).unwrap_or_default();
    let mut out: Vec<(String, String)> = Vec::new();
    if p.fonts.face_family == family {
        for f in p.fonts.faces.iter().take(200) {
            let o = option(f);
            if !out.iter().any(|(v, _)| *v == o.0) {
                out.push(o);
            }
        }
    }
    if !current.is_empty() && !out.iter().any(|(v, _)| *v == current) {
        out.insert(0, (current.clone(), cut_to(&stored, 256)));
    }
    (current, out)
}

/// The font a style letters in, as one line: family, face and size in the style's own unit. The
/// library shows it under every style and the editor at its top, so the typeface behind each style
/// type — Dialogue, Whisper, Shout… — is visible without opening each style in turn. A library that
/// travelled from another machine can name anything, so both names are cut to a label's length.
pub fn font_summary(props: &Value) -> String {
    let family = cut_to(&props::family(props), 120);
    let face = cut_to(&props::font_style(props), 40);
    let size = props::char(props, "size").and_then(Value::as_f64).filter(|n| n.is_finite()).unwrap_or(props::DEFAULT_SIZE);
    let unit = match props::type_unit(props) {
        "mmUnit" => " mm",
        "inchesUnit" => " in",
        "picasUnit" => " pc",
        "pointsUnit" => " pt",
        _ => " px",
    };
    format!("{family} {face} · {size}{unit}")
}

// --- the panel ---------------------------------------------------------------------------------

/// The whole panel. TypeR's own shape: three stacked panes — preview, script, style library — and
/// a footer. Help, Settings and the style editor are the pages TypeR opens as modals; here they
/// replace the panes and carry a Back button, because a panel window has no modal.
pub fn render(p: &Panel) -> Value {
    let mut children = match p.page {
        // `Styles` is where the panel lands after a library edit; the library is a pane of the
        // lettering view now, so it draws the same three panes rather than a page of its own.
        Page::Lettering | Page::Styles => lettering(p),
        Page::Style => overlay("Style editor", style_editor(p)),
        Page::Folders => overlay("Folders", folders(p)),
        Page::Settings => overlay("Settings", settings(p)),
        Page::Help => overlay("Help", help(p)),
    };
    children.push(separator());
    children.push(status(p));
    if let Some(m) = &p.message {
        children.push(match m.level {
            "error" => error(&m.text),
            "warning" => warning(&m.text),
            _ => muted(&m.text),
        });
    }
    column(children)
}

/// A page that replaces the panes (one of TypeR's modals), with the way back.
fn overlay(title: &str, page: Vec<Value>) -> Vec<Value> {
    let mut out = vec![row(vec![button("back", "‹ Back", false, true), label(title)]), separator()];
    out.extend(page);
    out
}

/// The document and the run, in one line: what the actions will act on.
fn status(p: &Panel) -> Value {
    let mut parts: Vec<String> = Vec::new();
    match &p.doc {
        Some(d) => {
            parts.push(format!("{} × {} px at {} dpi", round(d.width), round(d.height), round(d.dpi)));
            if let Some(sel) = d.selection {
                parts.push(format!("selection {} × {}", round(sel[2]), round(sel[3])));
            } else {
                parts.push("no selection".to_string());
            }
        }
        None => parts.push("no document open".to_string()),
    }
    let targets = p.target_layers();
    parts.push(match targets.len() {
        0 => "no type layer selected".to_string(),
        1 => format!("1 type layer: {}", p.doc.as_ref().map(|d| d.name_of(targets[0])).unwrap_or_default()),
        n => format!("{n} type layers selected"),
    });
    muted(parts.join(" · "))
}

fn busy(p: &Panel) -> bool {
    p.run.is_some()
}

/// The top of the panel: the line the work is on, the buttons that move between lines, and the line
/// itself as it will be set. This is the first thing in the panel, so a letterer opens it and sees
/// where they are without scrolling.
fn top(p: &Panel) -> Vec<Value> {
    // The dialogue lines, not the rows: an ignored line is a stage direction, and the arrows skip
    // it, so counting them here would put a number on the panel that no button can reach.
    let rows = script::count(&p.lines);
    let counter = if p.lines.is_empty() {
        muted("Line 0 / 0")
    } else {
        label(format!("Line {} / {}", p.line_number(), rows))
    };
    let working = busy(p);
    // The counter *is* the line's number and the list is the other way to move, so the number field
    // that used to sit here said the same thing a third time — and pushed the row into two lines on
    // a narrow panel.
    let mut out = vec![row(vec![
        counter,
        nav(p, "prev", "◀", "Previous line", !working),
        nav(p, "next", "▶", "Next line", !working),
        nav(p, "first", "⏮", "First line", !working),
        nav(p, "last", "⏭", "Last line", !working),
    ])];
    out.push(line_preview(p));
    // The context window was this port's stand-in for TypeR's line list. A host that draws a
    // `list` gets the real one in the script pane, so the stand-in would only say it twice.
    if !p.has("list") {
        out.push(context(p));
    }
    out
}

/// Characters of dialogue a row in [`context`] shows. Six rows of this, a line number and a marker
/// each stay inside the host's 256-character label with room to spare.
pub const CONTEXT_CHARS: usize = 30;

/// The lines around the current one, as **one** multi-line label: two rows before it, the row itself
/// marked `▶`, and three after, each with its raw line number. A finished row is marked `✓`.
///
/// ABI v2 has no monospace level, so this is a proportional label: the numbers are right-aligned in
/// four columns, which keeps the rows readable in a proportional face, and each row's dialogue is cut
/// so the whole block fits one label (a label over 256 characters makes the host refuse the view).
fn context(p: &Panel) -> Value {
    if p.lines.is_empty() {
        return muted("The script is empty: paste one line of dialogue per row.");
    }
    let from = p.cursor.saturating_sub(2);
    let to = (p.cursor.saturating_add(4)).min(p.lines.len());
    let rows: Vec<String> = (from..to)
        .filter_map(|i| {
            let line = p.lines.get(i)?;
            let marker = if i == p.cursor {
                "▶"
            } else if p.done.contains(&i) {
                "✓"
            } else {
                " "
            };
            let text = if line.text.trim().is_empty() { "(no text)" } else { line.text.as_str() };
            let text = cut_to(text, CONTEXT_CHARS).replace('\n', " ");
            Some(format!("{marker}{:>4} {text}", i + 1))
        })
        .collect();
    label(rows.join("\n"))
}

// --- Lettering ---------------------------------------------------------------------------------

/// The three panes, in TypeR's order: the line, the script, and the style library.
///
/// A host that draws `scroll` gets TypeR's own layout: the line and the buttons above, then the
/// script and the library as panes with their own scrollbars and pinned footers, so a long script
/// does not push the style list out of reach and the actions do not scroll away. The script is the
/// pane the work is done in, so it takes the larger share of the height. The plain layout stacks
/// everything in one scroll area, in the same order.
fn lettering(p: &Panel) -> Vec<Value> {
    let mut out = top(p);
    out.push(separator());
    if p.has("scroll") {
        out.push(scroll("scriptPane", 3, script_pane(p), script_footer(p)));
        out.extend(actions(p));
        out.push(scroll("stylesPane", 2, styles(p), styles_footer(p)));
        return out;
    }
    out.extend(script_pane(p));
    out.extend(done_section(p));
    out.push(separator());
    out.extend(actions(p));
    out.push(separator());
    out.extend(styles(p));
    out.push(separator());
    out.extend(styles_footer(p));
    out
}

/// A navigation button: a glyph with a tooltip where the host draws icons, the glyph alone where it
/// does not (which is how this panel has always drawn them).
fn nav(p: &Panel, id: &str, icon: &str, tooltip: &str, enabled: bool) -> Value {
    if p.has("button.icon") {
        icon_button(id, icon, tooltip, false, enabled)
    } else {
        button(id, icon, false, enabled)
    }
}

/// What is done to the line being worked on, and the size it is set at: one row of presses, then the
/// scale. The paragraphs that explained the buttons are under `?`, in Help: a panel whose main view
/// is mostly explanation is a panel that is mostly scrolled past.
fn actions(p: &Panel) -> Vec<Value> {
    let working = busy(p);
    let mut out = vec![
        row(vec![
            button("create", "Create", true, !working),
            button("insert", "Insert", false, !working),
            button("align", "Align", false, !working),
            // Framed, unlike the arrows at the top of the panel: A−/A+ are pressed over and over
            // while a line is fitted, and a bare glyph between framed buttons reads as a label for
            // the row rather than something to press.
            button("sizeDown", "A−", false, !working),
            button("sizeUp", "A+", false, !working),
        ]),
        // The host's slider is a track and a knob with no readout of its own, so the value is
        // written beside it — a lone `%` with no number is all a letterer used to get.
        row(vec![label("Text size"), number("textScale", p.scale, 1.0, 999.0, 1.0, true), label(format!("{}%", round(p.scale)))]),
    ];
    if working {
        out.push(progress());
        out.push(muted("Working…"));
    }
    out
}

/// The script pane: the field, and — where the host draws a list — TypeR's line list above it, with
/// each row's number in the gutter and the line being worked on highlighted.
fn script_pane(p: &Panel) -> Vec<Value> {
    let hidden = p.hide_done && !p.done.is_empty();
    let hint = if hidden { format!("{} finished row(s) hidden", p.done.len()) } else { "one line of dialogue per row".to_string() };
    // The box is the importer: it is what a chapter is pasted into, and once a script is in, the pane
    // is the script itself and the box is in the way of it. The button beside the pane's title says
    // which way it goes — "Edit script" brings it back for a change to a line, "Done" puts it away.
    let box_button = if p.script_box { button("scriptBoxClose", "Done", false, true) } else { button("scriptBoxOpen", "Edit script", false, true) };
    let mut out = vec![row(vec![label("Script"), muted(hint), box_button])];
    if p.has("list") {
        out.push(script_list(p));
        if p.lines.len() > LIST_MAX {
            out.push(warning(format!(
                "The list shows the first {LIST_MAX} lines; the script itself holds all {}.",
                p.lines.len()
            )));
        }
    }
    if p.script_box {
        out.push(text(
            "script",
            &clip(&p.visible_script()),
            "One line of dialogue per row. Start a line with a style's prefix to set it in that style.",
            true,
            SCRIPT_MAX,
        ));
    } else if p.lines.is_empty() {
        out.push(muted("No script yet: Edit script opens the box to paste one in."));
    } else {
        out.push(muted(format!("{} line(s) read: Edit script opens the box to change one.", p.lines.len())));
    }
    out
}

/// The script as a `list`: the raw rows in order, the line being worked on selected, and a ✓ on the
/// rows already lettered. With finished rows hidden the list leaves them out, so the host's own
/// gutter would number 1..n where the script's numbers are different — the row then carries its own
/// number instead, and the gutter is only asked for when the numbers are the script's.
fn script_list(p: &Panel) -> Value {
    let raw = p.list_rows();
    let numbered = !(p.hide_done && !p.done.is_empty());
    let rows: Vec<Value> = raw
        .iter()
        .filter_map(|i| {
            let line = p.lines.get(*i)?;
            let marker = if p.done.contains(i) { "✓ " } else { "" };
            let body = if line.raw.trim().is_empty() { "(no text)" } else { line.raw.as_str() };
            let text = if numbered { format!("{marker}{body}") } else { format!("{marker}{:>4} {body}", i + 1) };
            Some(list_row(text, None, None))
        })
        .collect();
    let selected = raw.iter().position(|i| *i == p.cursor);
    list("scriptLine", selected, numbered, rows)
}

/// The script pane's pinned footer: the hide-finished switch and how many are hidden, visible however
/// far down the script the letterer has scrolled.
fn script_footer(p: &Panel) -> Vec<Value> {
    let count = p.done.len();
    let hint = if count == 0 { "No rows finished yet".to_string() } else { format!("{count} finished row(s), marked ✓") };
    vec![row(vec![toggle("hideDone", p.hide_done, "Hide finished lines"), muted(hint)])]
}

/// The library pane's pinned footer: what the library is for — putting a style on the selection —
/// and the three presses that are always wanted, whatever the library is scrolled to.
///
/// Apply lives here rather than in the row of buttons above because it acts with the library: the
/// style it puts on the layers is the one the list has in hand, and it stays within reach while the
/// list is scrolled.
fn styles_footer(p: &Panel) -> Vec<Value> {
    let working = busy(p);
    let mut line = vec![button("newStyle", "New style", false, true)];
    if p.has("button.icon") {
        line.push(icon_button("openSettings", "⚙", "Settings", false, true));
        line.push(icon_button("openHelp", "?", "Help", false, true));
    } else {
        line.push(button("openSettings", "Settings", false, true));
        line.push(button("openHelp", "Help", false, true));
    }
    vec![
        row(vec![button("apply", "Apply style", false, !working), button("applyKeep", "Apply, keep size", false, !working)]),
        row(line),
    ]
}

/// The finished rows: a collapsed section with the option to leave them out of the script box, and
/// what has been lettered so far. The title stays `Done` — the host remembers a section's open state
/// by its title, so a count in the title would reopen a section that was closed.
fn done_section(p: &Panel) -> Vec<Value> {
    let count = p.done.len();
    let mut children = vec![
        toggle("hideDone", p.hide_done, "Hide finished lines from the script box"),
        muted(if count == 0 {
            "No rows finished yet: Create and Insert mark the row they letter.".to_string()
        } else {
            format!("{count} finished row(s). Marked ✓ in the script above; clicking a row's line number or moving with ◀ ▶ works as before.")
        }),
    ];
    for i in p.done.iter().copied().take(MAX_ROWS) {
        let Some(line) = p.lines.get(i) else { continue };
        let text = if line.text.trim().is_empty() { "(no text)" } else { line.text.as_str() };
        children.push(muted(format!("{:>4}. {}", i + 1, cut_to(text, LABEL_MAX.saturating_sub(12)))));
    }
    if count > MAX_ROWS {
        children.push(muted(format!("{} more finished row(s) not listed here.", count - MAX_ROWS)));
    }
    vec![section("Done", true, children)]
}

/// The line the buttons will letter with: its style, and the text as the script has it.
fn line_preview(p: &Panel) -> Value {
    let Some(line) = p.current_line() else {
        return muted("No line here.");
    };
    let style = p.library.current_style(line.style_id.as_deref());
    let (name, colour) = match style {
        Some(s) => (s.name.clone(), s.prefix_color.clone()),
        None => ("(no style)".to_string(), "-".to_string()),
    };
    let text = if line.ignored { "(ignored)".to_string() } else { line.text.clone() };
    // The line preview is set in the style's own font where the host draws label fonts: the
    // typeface behind the line is visible in the preview, as TypeR's own preview shows it.
    let font = style.and_then(|s| family_font(p, &s.text_props));
    let mut out = vec![styled(format!("{name}: {text}"), None, font.as_deref())];
    if line.ignored {
        out.push(muted(match &line.ignore_prefix {
            Some(prefix) => format!("This line starts with the ignore prefix {prefix:?}, so the buttons skip it."),
            None => "This line has no text, so the buttons skip it.".to_string(),
        }));
    } else {
        out.push(muted(if colour == "-" {
            String::new()
        } else if p.has("label.color") {
            format!("Prefix colour {colour}, shown on the style's row")
        } else {
            format!("Prefix colour {colour} (this host has no colour widget to draw it with)")
        }));
    }
    column(out)
}

// --- Styles ------------------------------------------------------------------------------------

fn styles(p: &Panel) -> Vec<Value> {
    let mut out = Vec::new();
    if p.library.styles.is_empty() {
        out.push(muted("The library is empty. Make a style here, or load the starter library from Help."));
    }
    let mut drawn = 0usize;
    // TypeR lists the styles in no folder first, then each folder's styles under its name, in tree
    // order. The grouping is the library's own: it is what decides which prefix wins.
    let unsorted = p.library.styles_in(None);
    if !unsorted.is_empty() {
        out.push(muted("No folder"));
        draw_styles(&mut out, p, &unsorted, &mut drawn);
    }
    for id in p.library.folder_order() {
        let Some(f) = p.library.folder(&id) else { continue };
        let in_folder = p.library.styles_in(Some(f.id.as_str()));
        if in_folder.is_empty() {
            continue;
        }
        let indent = "  ".repeat(p.library.folder_depth(&f.id));
        out.push(muted(format!("{indent}{} ({} style(s))", f.name, in_folder.len())));
        draw_styles(&mut out, p, &in_folder, &mut drawn);
    }
    if p.library.styles.len() > drawn {
        out.push(warning(format!(
            "{} more styles than this page draws; use a prefix in the script, or edit the JSON in Help.",
            p.library.styles.len() - drawn
        )));
    }
    out.push(separator());
    if !p.library.styles.is_empty() {
        let options: Vec<(String, String)> = p.library.styles.iter().map(|s| (s.id.clone(), s.name.clone())).collect();
        let current = p.library.default_style_id.clone().unwrap_or_else(|| options.first().map(|(v, _)| v.clone()).unwrap_or_default());
        out.push(row(vec![muted("Style for lines with no prefix"), choice("defaultStyle", &current, options)]));
    }
    out
}

/// Draws styles until the page is full. Their ids carry their place in the library array, which is
/// the order the panel's handler indexes, not their place inside a folder.
fn draw_styles(out: &mut Vec<Value>, p: &Panel, styles: &[&Style], drawn: &mut usize) {
    for s in styles {
        if *drawn >= MAX_ROWS {
            break;
        }
        let Some(i) = p.library.styles.iter().position(|x| x.id == s.id) else { continue };
        *drawn += 1;
        out.push(style_row(p, i, s));
        // A host that sets each name in its own font has already answered "which face is this?".
        // One that cannot still gets the answer in words, as it always has.
        if !p.has("label.font") {
            out.push(muted(format!("Font: {}", font_summary(&s.text_props))));
        }
    }
}

/// One style: its text colour as a swatch, its name in its own font (where the host draws those),
/// the quick size editor when it is on, and the apply/edit buttons.
fn style_row(p: &Panel, i: usize, s: &Style) -> Value {
    let working = busy(p);
    let mut line = Vec::new();
    if p.has("label.color") {
        // TypeR's style item carries two swatches: the colour the style letters in, and the colour
        // of its prefix chip. A style with no prefixes has no chip, so it gets one square.
        let colour = props::color_hex(&s.text_props);
        line.push(styled("■", Some(&colour), None));
        if !s.prefixes.is_empty() {
            line.push(styled("■", Some(&s.prefix_color), None));
        }
    }
    let mut name = s.name.clone();
    if !s.prefixes.is_empty() {
        name.push_str(&format!(" [{}]", s.prefixes.join(" ")));
    }
    if s.prefixes_disabled {
        name.push_str(" (prefixes off)");
    }
    line.push(styled(name, None, family_font(p, &s.text_props).as_deref()));
    if p.quick_size {
        // TypeR's quick style size (`stylesBlock.jsx`): a step down, the size in the style's own
        // unit, and a step up, right on the row.
        let size = props::char(&s.text_props, "size").and_then(Value::as_f64);
        let step = if p.style_step.is_finite() && p.style_step > 0.0 { p.style_step } else { 0.1 };
        line.push(nav(p, &format!("s{i}_sizeDown"), "−", "Smaller", !working));
        line.push(number(&format!("s{i}_size"), size.unwrap_or(14.0), 1.0, 1296.0, step, false));
        line.push(nav(p, &format!("s{i}_sizeUp"), "+", "Larger", !working));
    }
    line.push(button(&format!("s{i}_apply"), "Apply", false, !working));
    line.push(button(&format!("s{i}_edit"), "Edit", false, !working));
    row(line)
}

// --- Style editor ------------------------------------------------------------------------------

fn style_editor(p: &Panel) -> Vec<Value> {
    let Some(e) = &p.edit else {
        return vec![muted("No style is open. Pick one from the Styles page, or make a new one."), row(vec![button("newStyle", "New style", false, true)])];
    };
    let (name, prefixes, color, folder, is_new) = (e.name.clone(), e.prefixes.clone(), e.color.clone(), e.folder.clone(), e.is_new);
    // "Font" is the one control a letterer goes looking for by name, so it is a label rather than
    // dimmed like the rest, and the row says how many families the host listed — an empty-looking
    // dropdown with no explanation reads as a broken panel.
    let (font_value, font_options) = families(p);
    let (face_value, face_options) = faces(p);
    let mut font_row = vec![label("Font"), choice("ed_font", &font_value, font_options)];
    if p.fonts.is_empty() {
        font_row.push(muted("(this host has listed no fonts)"));
    } else {
        font_row.push(muted(format!("{} from this host", p.fonts.families.len())));
    }
    let mut out = vec![
        row(vec![label("Name"), text("ed_name", &name, "Dialogue", false, LABEL_MAX)]),
        row(vec![muted("Prefixes"), text("ed_prefixes", &prefixes, "One prefix per line, or separated by semicolons", true, LABEL_MAX)]),
        row(vec![muted("Prefix colour"), choice("ed_color", &color, PREFIX_COLORS.iter().map(|c| (c.to_string(), c.to_string())).collect())]),
        row(vec![muted("Folder"), choice("ed_folder", folder.as_deref().unwrap_or("-"), folder_options(p))]),
        separator(),
        row(font_row),
        row(vec![muted("Face"), choice("ed_face", &face_value, face_options)]),
        row(vec![muted(format!("Size{}", unit_suffix(p))), number("ed_size", length(p, "size", true, 14.0), 0.1, 1296.0, 0.5, false)]),
        row(vec![
            toggle("ed_autoLeading", b(c(p, "autoLeading").as_ref()), "Auto leading"),
            muted("Leading"),
            number("ed_leading", length(p, "leading", true, 0.0), 0.0, 5000.0, 0.5, false),
        ]),
        row(vec![muted("Auto leading %"), number("ed_autoLeadingPct", n(q(p, "autoLeadingPercentage").as_ref(), 1.2) * 100.0, 10.0, 1000.0, 1.0, false)]),
        row(vec![
            muted("Tracking"),
            number("ed_tracking", n(c(p, "tracking").as_ref(), 0.0), -1000.0, 10_000.0, 1.0, false),
            muted("Kerning"),
            choice(
                "ed_kerning",
                &s(c(p, "autoKern").as_ref()),
                vec![("metricsKern".into(), "Metrics".into()), ("opticalKern".into(), "Optical".into()), ("manual".into(), "Manual".into())],
            ),
            number("ed_kernPair", n(c(p, "kerning").as_ref(), 0.0), -1000.0, 10_000.0, 1.0, false),
        ]),
        row(vec![
            muted("Horizontal %"),
            number("ed_hScale", n(c(p, "horizontalScale").as_ref(), 100.0), 1.0, 1000.0, 1.0, false),
            muted("Vertical %"),
            number("ed_vScale", n(c(p, "verticalScale").as_ref(), 100.0), 1.0, 1000.0, 1.0, false),
        ]),
        row(vec![
            muted(format!("Baseline{}", unit_suffix(p))),
            number("ed_baseline", length(p, "baselineShift", true, 0.0), -5000.0, 5000.0, 0.5, false),
            muted("Script"),
            choice(
                "ed_script",
                &baseline_choice(p),
                vec![("normal".into(), "Normal".into()), ("superScript".into(), "Superscript".into()), ("subScript".into(), "Subscript".into())],
            ),
        ]),
        muted(
            "Superscript and subscript are written as a baseline shift and a smaller size, which is how they render; the layer will not carry Adobe's own script attribute.",
        ),
        row(vec![
            muted("Caps"),
            choice(
                "ed_caps",
                &s(c(p, "fontCaps").as_ref()),
                vec![("normal".into(), "Normal".into()), ("smallCaps".into(), "Small caps".into()), ("allCaps".into(), "All caps".into())],
            ),
            muted("Align"),
            choice(
                "ed_align",
                &s(q(p, "alignment").as_ref()),
                vec![
                    ("left".into(), "Left".into()),
                    ("center".into(), "Centre".into()),
                    ("right".into(), "Right".into()),
                    ("justifyLeft".into(), "Justify left".into()),
                    ("justifyCenter".into(), "Justify centre".into()),
                    ("justifyRight".into(), "Justify right".into()),
                    ("justifyAll".into(), "Justify all".into()),
                ],
            ),
        ]),
        separator(),
        row(vec![
            toggle("ed_fauxBold", b(c(p, "syntheticBold").as_ref()), "Faux bold"),
            toggle("ed_fauxItalic", b(c(p, "syntheticItalic").as_ref()), "Faux italic"),
            toggle("ed_underline", props::is_on(c(p, "underline").as_ref(), "underlineOff"), "Underline"),
            toggle("ed_strike", props::is_on(c(p, "strikethrough").as_ref(), "strikethroughOff"), "Strikethrough"),
        ]),
        row(vec![
            toggle("ed_ligatures", b(c(p, "ligature").as_ref()) || b(c(p, "altligature").as_ref()), "Ligatures"),
            toggle("ed_discLigatures", b(c(p, "altligature").as_ref()), "Discretionary ligatures"),
            toggle("ed_hyphenate", b(q(p, "hyphenate").as_ref()), "Hyphenate"),
        ]),
        muted("PhotoCraft stores hyphenation but does not lay it out yet, so this saves a preference rather than changing the line."),
        row(vec![
            muted(format!("Indents{}", unit_suffix(p))),
            number("ed_firstIndent", length(p, "firstLineIndent", false, 0.0), -5000.0, 5000.0, 0.5, false),
            number("ed_startIndent", length(p, "leftIndent", false, 0.0), -5000.0, 5000.0, 0.5, false),
            number("ed_endIndent", length(p, "rightIndent", false, 0.0), -5000.0, 5000.0, 0.5, false),
            number("ed_spaceBefore", length(p, "spaceBefore", false, 0.0), 0.0, 5000.0, 0.5, false),
            number("ed_spaceAfter", length(p, "spaceAfter", false, 0.0), 0.0, 5000.0, 0.5, false),
        ]),
        row(vec![
            muted("Colour"),
            text("ed_colorHex", &props::color_hex(&e.props), "#000000", false, 16),
            muted("Anti-alias"),
            choice(
                "ed_antiAlias",
                props::anti_alias(&e.props),
                vec![
                    ("antiAliasNone".into(), "None".into()),
                    ("antiAliasSharp".into(), "Sharp".into()),
                    ("antiAliasCrisp".into(), "Crisp".into()),
                    ("antiAliasStrong".into(), "Strong".into()),
                    ("antiAliasSmooth".into(), "Smooth".into()),
                    ("antiAliasPlatformGray".into(), "Windows".into()),
                    ("antiAliasPlatformLCD".into(), "Windows LCD".into()),
                ],
            ),
        ]),
        separator(),
        row(vec![
            button("ed_up", "Move up", false, !busy(p)),
            button("ed_down", "Move down", false, !busy(p)),
            button("ed_copy", "Copy from a selected layer", false, !busy(p)),
        ]),
        muted(
            "Styles are matched to a script line by prefix, in library order, so the order is what decides when two styles share a prefix. Drag-reorder needs a drag widget this host does not have yet; these two buttons are the same thing.",
        ),
        row(vec![
            button("ed_save", if is_new { "Add style" } else { "Save" }, true, true),
            button("ed_delete", "Delete", false, !is_new && !busy(p)),
            button("ed_cancel", "Close", false, true),
        ]),
    ];
    // Adobe's paragraph options that this host cannot apply, when the style is asking for one of
    // them. Quiet about the inert defaults every style carries, loud about a real request: an option
    // that silently does nothing is worse than one that says it will do nothing.
    let unsupported = props::unsupported_paragraph_keys(&e.props);
    if !unsupported.is_empty() {
        out.push(warning(format!(
            "This style asks for {} — this host keeps it in the library but has nothing to apply it with, so the text will not use it.",
            unsupported.join(", ")
        )));
    }
    // The name and the font the style letters in, together at the top: the answer to "which font is
    // Dialogue in?" should not require scrolling to the font row and reading a dropdown.
    out.insert(0, label(format!("Editing “{name}”{}", if is_new { " (new)" } else { "" })));
    out.insert(1, muted(format!("Font: {}", font_summary(&e.props))));
    out
}

/// Adobe writes "not super- or subscript" as `normal`, an absent key or anything unrecognised.
fn baseline_choice(p: &Panel) -> String {
    match s(c(p, "baseline").as_ref()).as_str() {
        "superScript" => "superScript".to_string(),
        "subScript" => "subScript".to_string(),
        _ => "normal".to_string(),
    }
}

/// The folders, in the order the tree draws them, each name indented by its depth — TypeR indents
/// the folder list in its editors by `depth * 12 + 4` pixels (`stylesBlock.jsx:140`); a text label
/// has no padding of its own, so indentation here is two spaces a level.
fn folder_options(p: &Panel) -> Vec<(String, String)> {
    let mut out = vec![("-".to_string(), "No folder".to_string())];
    out.extend(p.library.folder_order().iter().filter_map(|id| p.library.folder(id)).map(|f| {
        let indent = "  ".repeat(p.library.folder_depth(&f.id));
        (f.id.clone(), format!("{indent}{}", f.name))
    }));
    out
}

/// The folders a folder can be moved into: everything except itself and its own descendants, which
/// is the list TypeR's folder editor offers (`editFolder.jsx:147-151`).
fn parent_options(p: &Panel) -> Vec<(String, String)> {
    let mut out = vec![("-".to_string(), "Top level".to_string())];
    let excluded = p.folder.as_deref().map(|id| p.library.descendants(id)).unwrap_or_default();
    out.extend(
        p.library
            .folder_order()
            .iter()
            .filter(|id| !excluded.contains(id))
            .filter_map(|id| p.library.folder(id))
            .map(|f| {
                let indent = "  ".repeat(p.library.folder_depth(&f.id));
                (f.id.clone(), format!("{indent}{}", f.name))
            }),
    );
    out
}

// --- Folders -----------------------------------------------------------------------------------

fn folders(p: &Panel) -> Vec<Value> {
    let editing = p.folder.is_some();
    let mut out = vec![
        row(vec![label("Folder"), text("f_name", &p.folder_name, "Folder name", false, LABEL_MAX), button("f_new", "New folder", editing, true)]),
        row(vec![muted("Inside"), choice("f_parent", p.folder_parent.as_deref().unwrap_or("-"), parent_options(p))]),
        muted("Folders nest, and a style belongs to one of them."),
        muted("With “current folder tag priority” on (Settings), a prefix in the folder of the style in use wins over the same prefix elsewhere."),
        muted("The arrows move a folder among its siblings. A folder's Export writes TypeR's folder file for it."),
        separator(),
    ];
    if p.library.folders.is_empty() {
        out.push(muted("No folders yet."));
    }
    for (i, id) in p.library.folder_order().iter().take(MAX_ROWS).enumerate() {
        let Some(f) = p.library.folder(id) else { continue };
        let count = p.library.styles_in(Some(f.id.as_str())).len();
        let renaming_this = p.folder.as_deref() == Some(f.id.as_str());
        let indent = "  ".repeat(p.library.folder_depth(&f.id));
        out.push(row(vec![
            label(format!("{indent}{} ({count} style(s))", f.name)),
            button(&format!("f{i}_up"), "↑", false, true),
            button(&format!("f{i}_down"), "↓", false, true),
            button(&format!("f{i}_edit"), if renaming_this { "Editing" } else { "Rename" }, false, !renaming_this),
            button(&format!("f{i}_export"), "Export", false, true),
            button(&format!("f{i}_del"), "Delete", false, true),
        ]));
    }
    if editing {
        out.push(row(vec![button("f_save", "Save", true, true), button("f_cancel", "Cancel", false, true)]));
        out.push(muted("Saving changes the folder's name and where it sits. Its styles go with it, and deleting a folder leaves them in the unsorted list."));
    }
    if p.library.folders.len() > MAX_ROWS {
        out.push(warning(format!("{} more folders than this page draws.", p.library.folders.len() - MAX_ROWS)));
    }
    out
}

// --- Settings ----------------------------------------------------------------------------------

fn settings(p: &Panel) -> Vec<Value> {
    let library = &p.library;
    let mut out = vec![
        toggle("set_pointText", library.point_text, "Create point text instead of a box"),
        muted(
            "Point text is one line from a click, which is what a sound effect usually is; box text wraps inside the balloon's shape.",
        ),
        separator(),
        row(vec![muted("Ignore prefixes"), text("set_ignore", &library.ignore_prefixes.join("\n"), "##", true, LABEL_MAX)]),
        muted("A script line starting with one of these is skipped by the buttons: it is a stage direction, not dialogue. Separate them with line breaks or semicolons."),
        row(vec![muted("Ignore tags"), text("set_ignoreTags", &library.ignore_tags.join("\n"), "[note]", true, LABEL_MAX)]),
        muted("These are removed from a line wherever they appear, before the line is lettered. TypeR's default list is empty."),
        toggle("set_folderPriority", library.folder_priority, "Prefer the prefixes of the style's own folder"),
        muted("With this on, a line is matched against the prefixes of the folder the style in use belongs to before the library's other folders — TypeR's `currentFolderTagPriority`."),
        separator(),
        toggle("set_markdown", p.markdown, "Interpret markdown"),
        muted("`**bold**`, `*italic*` and `***both***` are stripped from the line and set as faux bold or italic runs. Unclosed markers stay as text."),
        row(vec![
            muted("Direction"),
            choice("set_direction", &p.direction, vec![("ltr".into(), "Left to right".into()), ("rtl".into(), "Right to left".into())]),
        ]),
        muted("The direction every line is lettered with, as TypeR's direction setting."),
        separator(),
        row(vec![muted("Internal padding"), number("set_padding", p.padding, 0.0, 100.0, 1.0, false), muted("px each side of the box")]),
        muted("TypeR's `internalPadding`: taken off both sides of the balloon box's width so the type does not touch its edge (0–100)."),
        toggle("set_resizeOnCenter", p.resize_on_center, "Resize the text box when Align centres"),
        muted("Off, Align only moves the layer. On, it first gives the layer the balloon's box and refits it, as TypeR's `resizeTextBoxOnCenter` does; point text is never reshaped."),
        toggle("set_deselect", p.deselect, "Deselect after placing a line"),
        muted("Create, Insert and Align drop the marquee once the line is in, so the next balloon can be drawn without clearing it by hand. The clearing goes in the same undo step as the line: one undo brings the line and the marquee back together."),
        separator(),
        row(vec![muted("Size step"), number("set_sizeIncrement", p.size_increment, 1.0, 99.0, 1.0, false), muted("for A−/A+")]),
        row(vec![muted("Style size step"), number("set_styleStep", p.style_step, 0.01, 1000.0, 0.01, false)]),
        toggle("set_quickStyleSize", p.quick_size, "Show the quick style size editor on library rows"),
        separator(),
        label("Shortcuts"),
        muted("The panel's own presses are its buttons: a panel cannot be given keys. ABI v2 has no command or shortcut contribution, and nothing typed reaches a panel's sandbox, so a key set here would be a promise the host cannot keep."),
        muted("TypeR had no shortcut settings either — its buttons were the whole interface."),
        muted("These are PhotoCraft's own keys, written the way this platform writes them. They work while this window is up: the panel is a window inside PhotoCraft rather than a separate application."),
        row(vec![label(keys(p, "Cmd+Shift+,")), muted("Decrease Type Size — the selected type layer a step down")]),
        row(vec![label(keys(p, "Cmd+Shift+.")), muted("Increase Type Size")]),
        row(vec![label(keys(p, "Cmd+Alt+Shift+,")), muted("Decrease Type Size by 10")]),
        row(vec![label(keys(p, "Cmd+Alt+Shift+.")), muted("Increase Type Size by 10")]),
        muted(format!(
            "The host's own step is 2 pt. A−/A+ step by the Size step above — {} pt — and keep the layer where it is, as TypeR did.",
            round(p.size_increment)
        )),
        muted("Two more worth a key, and neither has one to begin with: Layer ▸ New ▸ New Type Layer, which is what Create does, and this panel's own row under Window ▸ Plug-ins ▸ Typerust, which shows and hides the window."),
        muted("All of them are listed in Edit ▸ Keyboard Shortcuts, which is where a key is given, changed or removed."),
        separator(),
        row(vec![muted("State name"), text("state_name", &p.state_name, "Scene 3", false, LABEL_MAX), button("state_save", "Save state", false, true)]),
        muted("A state is the script, the line and the style in use — TypeR's saved states. Page images and the last opened PSD are not part of it: a panel can neither open nor see a file."),
    ];
    if p.states.is_empty() {
        out.push(muted("No states saved yet."));
    }
    for (i, name) in p.states.keys().take(MAX_ROWS).enumerate() {
        out.push(row(vec![
            label(name),
            button(&format!("st{i}_load"), "Load", false, true),
            button(&format!("st{i}_del"), "Delete", false, true),
        ]));
    }
    out.push(separator());
    out.push(row(vec![button("set_scaleDefault", "Text size back to 100 %", false, true), button("set_starter", "Load the starter library", false, true)]));
    out.push(row(vec![button("set_reset", "Reset everything", false, true)]));
    out
}

// --- Help --------------------------------------------------------------------------------------

fn help(p: &Panel) -> Vec<Value> {
    let mut out = vec![
        label("Typerust"),
        muted("A lettering panel: paste a script, one line of dialogue per row, tag lines with a style's prefix, and put them into the page."),
        muted("It is a port of TypeR 2.5.0 for Adobe Photoshop and of TyperTools 1.4.9 before it, both MIT, © 2017 Swirt — their strings, their style library and their layout, against PhotoCraft's own type engine."),
        muted("Not affiliated with the authors of either: TypeR and TyperTools are Adobe Photoshop panels by other people, and this is a separate project that ports their behaviour. See ATTRIBUTION.md."),
        separator(),
        label("Text size and the buttons"),
        // Two labels, not one: a label is cut at the host's 256 characters, and half an
        // explanation is worse than none.
        muted("Text size scales the style's size (and its leading) when a line is created or inserted."),
        muted(format!(
            "A−/A+ step the type by {} pt and keep the layer where it is. Apply puts the library's style on the selected layers without touching what they say; Apply, keep size gives them that look but leaves the size they have.",
            p.size_increment
        )),
        separator(),
        label("What is here"),
        muted("TypeR's three panes: the line with Create, Insert, Align and the size steps, the script, and the style library with Apply and Apply keeping the layer's size."),
        muted("Under them: nested folders with a folder-scoped prefix priority, `//` inheritance, ignore tags, markdown bold and italic, RTL direction, internal padding and resize-on-centre."),
        muted("Both size steps, the quick style size, saved states, the style editor, and TypeR's full and single-folder JSON exports."),
        label("What is not, and why"),
        muted("Keyboard shortcuts: a panel cannot add any — ABI v2 has no command or shortcut contribution, and keystrokes never cross the sandbox — so the actions are these buttons."),
        muted("The host's own keys, spelled for this platform and with the two host rows worth giving a key to, are on the Settings page."),
    ];
    // The gaps are the host's, and a host that has grown the widget is no longer missing it. Say
    // which, rather than either claiming a gap that is gone or pretending the plain layout is it.
    if p.has("label.color") && p.has("label.font") {
        out.push(muted("A line is previewed in its style's own font, and each style's row carries its text colour and prefix colour as swatches."));
    } else {
        out.push(muted("A coloured preview of a line, and the prefix colour itself: this host has no colour widget, so a prefix's colour is written out as text."));
    }
    if p.has("list") && p.has("scroll") {
        out.push(muted("TypeR's draggable pane divider: the script and the library keep a fixed share of the height instead. The script list is line-numbered, the line being worked on is highlighted, and a row can be clicked to work on it."));
    } else {
        out.push(muted("TypeR's draggable pane divider and script gutter: the widget set has neither a divider nor a gutter. The panes keep TypeR's order at their own content height, and the line number and style are named in the preview pane."));
    }
    out.extend([
        muted("Importing or exporting the library as a file: the ABI has no file dialogs, so the JSON is shown in the box below to copy out or paste in."),
        muted("The full export is TypeR's own `TypeR_Export.json` shape; a folder's Export button writes its single-folder shape, which this port can also load."),
        muted(
            "Hanging punctuation (burasagari) is refused with a message rather than stored silently: PhotoCraft's type engine has no kinsoku line breaking yet.",
        ),
        muted("The marquee is not touched. TypeR expands and contracts the selection in twelve steps before using it; this port reads the selection, insets it by 10 px, and leaves it as the letterer drew it, because ABI v2 grants no selection command."),
        muted("TypeR's Middle East marks option is absent for the same reason: PhotoCraft's type engine has no diacritic positioning to set."),
        separator(),
        label("The library as JSON"),
        text("io_json", &clip(&p.io), "Paste a TypeR library or exported folder here, or press Show JSON to see this one.", true, SCRIPT_MAX),
        row(vec![
            button("io_export", "Show JSON", false, true),
            button("io_load", "Load", false, true),
            button("io_merge", "Merge", false, true),
            button("io_reset", "Clear", false, true),
        ]),
        muted(format!(
            "The panel remembers {} style(s), {} folder(s) and its settings between sessions, in PhotoCraft's own plug-in storage.",
            p.library.styles.len(),
            p.library.folders.len()
        )),
    ]);
    out.push(separator());
    out.push(muted("Starter library: dialogue, whisper, shout and thought styles, a sound-effect style, and the ## ignore prefix."));
    out
}

// --- helpers -----------------------------------------------------------------------------------

/// Cuts to the longest text the host will accept in a `text` widget, so a script pasted in from a
/// novel cannot make the view invalid (the host checks the declared value against `maxLength`).
fn clip(text: &str) -> String {
    if text.chars().count() <= SCRIPT_MAX {
        return text.to_string();
    }
    text.chars().take(SCRIPT_MAX).collect()
}

/// A host shortcut in the platform's own notation, as the host writes it in its menus: `Cmd+Shift+.`
/// is `⌘⇧.` on macOS and `Ctrl+Shift+.` elsewhere. The panel is told the platform at `init`; a host
/// that says nothing gets the non-Mac spelling rather than a guess at a Mac one.
fn keys(p: &Panel, spec: &str) -> String {
    let mac = p.platform == "macos";
    spec.split('+')
        .map(|part| match (part, mac) {
            ("Cmd", true) => "⌘",
            ("Cmd", false) => "Ctrl+",
            ("Shift", true) => "⇧",
            ("Shift", false) => "Shift+",
            ("Alt", true) => "⌥",
            ("Alt", false) => "Alt+",
            (other, _) => other,
        })
        .collect()
}

fn round(v: f64) -> String {
    if !v.is_finite() {
        return "?".to_string();
    }
    if (v - v.round()).abs() < 0.05 { format!("{}", v.round() as i64) } else { format!("{v:.1}") }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::actions::{Kind, Run};
    use crate::doc::Doc;
    use crate::library::{Style, starter};
    use crate::panel::Panel;
    use std::collections::BTreeSet;

    /// The widget features the host advertises when it draws everything this module can use.
    fn all_widgets() -> Vec<String> {
        ["list", "scroll", "label.color", "label.font", "button.icon"].iter().map(|s| s.to_string()).collect()
    }

    /// A panel in each state a page has to survive: one that has never been used, one with a real
    /// document, a script and a library, and one with a style open in the editor. The used one
    /// advertises every new widget, so the pages are checked in both the plain and the TypeR-shaped
    /// layout.
    fn panels() -> Vec<(&'static str, Panel)> {
        let mut fresh = Panel::new();
        fresh.page = Page::Styles;

        let mut used = Panel::new();
        used.widgets = all_widgets();
        used.doc = Doc::from_json(&json!({
            "width": 2000, "height": 3000, "resolution": 300,
            "activeLayer": 4, "selectedLayers": [4], "hasSelection": true,
            "selectionBounds": [100, 50, 200, 160],
            "layers": [{"id": 4, "name": "Balloon", "kind": "Type", "selected": true}],
        }));
        used.script = "d. I'll be home by six.\n## sound of a door\nx. SIX!".into();
        used.library = starter(Some("Inter"));
        used.fonts.families = vec!["Inter".into(), "DejaVu Sans".into()];
        used.fonts.face_family = "Inter".into();
        used.fonts.faces = vec!["Regular".into(), "Bold".into(), "Italic".into()];
        used.reparse();
        used.io = "{\"styles\": []}".into();

        let mut editing = Panel::new();
        editing.library = starter(None);
        editing.start_edit("s1", false);
        editing.page = Page::Style;

        let mut naming = Panel::new();
        naming.library = starter(None);
        naming.folder = Some("f2".into());
        naming.folder_name = "Sound effects".into();

        vec![("fresh", fresh), ("used", used), ("editing", editing), ("naming", naming)]
    }

    /// The host's own rules (PhotoCraft's `crates/plugins/src/view.rs`), which is what this panel has
    /// to satisfy: ids, counts, depth, and a `choice` whose value is one of its options. A tree that
    /// breaks one is refused whole and the panel stops, so this is the test that matters most here.
    fn check(root: &Value) {
        let mut seen = BTreeSet::new();
        let mut count = 0;
        walk(root, 1, &mut count, &mut seen);
    }

    fn walk(w: &Value, depth: usize, count: &mut usize, seen: &mut BTreeSet<String>) {
        *count += 1;
        assert!(*count <= 512, "more than 512 widgets");
        assert!(depth <= 16, "nested deeper than 16");
        let kind = w.get("type").and_then(Value::as_str).unwrap_or_default();
        assert!(!kind.is_empty(), "a node with no type: {w}");
        if let Some(id) = w.get("id").and_then(Value::as_str) {
            assert!((1..=64).contains(&id.len()), "{id:?} is not 1-64 bytes");
            assert!(id.bytes().all(|b| b.is_ascii_alphanumeric() || matches!(b, b'_' | b'.' | b'-')), "{id:?} has a character a widget id may not");
            assert!(seen.insert(id.to_string()), "{id:?} is used twice");
        }
        match kind {
            "label" | "button" | "section" | "toggle" => {
                let text = w.get("text").or_else(|| w.get("label")).or_else(|| w.get("title")).and_then(Value::as_str).unwrap_or_default();
                assert!(text.chars().count() <= 256, "a label is over 256 characters: {text}");
                if let Some(icon) = w.get("icon").and_then(Value::as_str) {
                    assert!(!icon.is_empty() && icon.chars().count() <= 8, "an icon over 8 characters: {icon:?}");
                }
                if let Some(c) = w.get("color").and_then(Value::as_str) {
                    assert!(is_hex(c), "{c:?} is not a #rrggbb colour: {w}");
                }
                if let Some(f) = w.get("font").and_then(Value::as_str) {
                    assert!(!f.is_empty() && f.chars().count() <= FONT_MAX, "bad label font: {w}");
                }
            }
            "list" => {
                let rows = w["rows"].as_array().cloned().unwrap_or_default();
                assert!(rows.len() <= 1000, "a list over 1000 rows: {}", rows.len());
                if let Some(selected) = w.get("selected").and_then(Value::as_u64) {
                    assert!((selected as usize) < rows.len(), "a list's selected row is not drawn: {w}");
                }
                for row in &rows {
                    let text = row["text"].as_str().unwrap_or_default();
                    assert!(!text.is_empty(), "a row with no text: {row}");
                    if let Some(c) = row.get("color").and_then(Value::as_str) {
                        assert!(is_hex(c), "{c:?} is not a #rrggbb colour: {row}");
                    }
                    if let Some(f) = row.get("font").and_then(Value::as_str) {
                        assert!(!f.is_empty() && f.chars().count() <= FONT_MAX, "bad row font: {row}");
                    }
                }
            }
            "scroll" => {
                let weight = w["weight"].as_u64().unwrap_or(1);
                assert!((1..=16).contains(&weight), "a scroll weight outside 1..=16: {w}");
                assert!(!w["children"].is_null(), "a scroll with no children field: {w}");
            }
            "number" => {
                let (v, lo, hi, step) = (
                    w["value"].as_f64().unwrap_or(f64::NAN),
                    w["min"].as_f64().unwrap_or(f64::NAN),
                    w["max"].as_f64().unwrap_or(f64::NAN),
                    w["step"].as_f64().unwrap_or(f64::NAN),
                );
                assert!(v.is_finite() && lo.is_finite() && hi.is_finite() && lo <= hi && (lo..=hi).contains(&v), "bad number: {w}");
                assert!(step.is_finite() && step > 0.0, "a number needs a positive step: {w}");
            }
            "choice" => {
                let value = w["value"].as_str().unwrap_or_default();
                let options = w["options"].as_array().cloned().unwrap_or_default();
                assert!(!options.is_empty() && options.len() <= 256, "bad choice: {w}");
                assert!(options.iter().any(|o| o["value"].as_str() == Some(value)), "a choice value is not offered: {w}");
                assert!(options.iter().all(|o| o["value"].as_str().is_some_and(|v| !v.is_empty() && v.len() <= 256)), "bad option: {w}");
            }
            "text" => {
                let value = w["value"].as_str().unwrap_or_default();
                let cap = w["maxLength"].as_u64().unwrap_or(SCRIPT_MAX as u64);
                assert!(cap <= SCRIPT_MAX as u64 && value.chars().count() as u64 <= cap, "a text value is longer than its cap: {}", value.chars().count());
            }
            "progress" => assert!(w["fraction"].is_null() || w["fraction"].as_f64().is_some_and(|f| (0.0..=1.0).contains(&f)), "bad progress: {w}"),
            "column" | "row" | "separator" => {}
            other => panic!("{other} is not a widget the host has: {w}"),
        }
        if let Some(children) = w.get("children").and_then(Value::as_array) {
            for child in children {
                walk(child, depth + 1, count, seen);
            }
        }
        // A pane's pinned footer is part of the tree the host checks, so it is part of the tree this
        // check walks.
        if let Some(footer) = w.get("footer").and_then(Value::as_array) {
            for child in footer {
                walk(child, depth + 1, count, seen);
            }
        }
    }

    #[test]
    fn every_page_is_a_view_the_host_will_accept() {
        for (what, mut p) in panels() {
            for page in [Page::Lettering, Page::Styles, Page::Style, Page::Folders, Page::Settings, Page::Help] {
                p.page = page;
                for busy in [false, true] {
                    // A run in flight: every action button goes grey, the progress bar appears.
                    p.run = busy.then(|| Run::begin(Kind::Create, "test".into(), vec![]));
                    check(&render(&p));
                }
                p.run = None;
            }
            assert!(!what.is_empty());
        }
    }

    /// The `list` widget with this id, if the view has one.
    fn find_list<'a>(v: &'a Value, id: &str) -> Option<&'a Value> {
        if v.get("type").and_then(Value::as_str) == Some("list") && v.get("id").and_then(Value::as_str) == Some(id) {
            return Some(v);
        }
        for key in ["children", "footer"] {
            if let Some(list) = v.get(key).and_then(Value::as_array)
                && let Some(found) = list.iter().find_map(|c| find_list(c, id))
            {
                return Some(found);
            }
        }
        None
    }

    #[test]
    fn a_host_that_draws_the_new_widgets_gets_type_rs_shape() {
        // The whole point of `init.host.widgets`: this host can draw TypeR's own layout — a
        // line-numbered script list with the current line highlighted, style names in their own
        // fonts beside colour swatches, glyph buttons, and panes that scroll independently.
        let mut p = Panel::new();
        p.widgets = all_widgets();
        p.library = starter(Some("Inter"));
        p.script = "d. one\nx. BANG\nd. three".into();
        p.reparse();
        p.cursor = 1;
        p.done.insert(0);
        let view = render(&p);
        check(&view);
        let dumped = view.to_string();
        assert!(dumped.contains("\"type\":\"scroll\""), "the panes scroll independently: {dumped}");
        assert!(dumped.contains("\"id\":\"scriptLine\""), "the script is a list: {dumped}");
        let list = find_list(&view, "scriptLine").expect("the script list is drawn");
        assert_eq!(list["numbered"], json!(true), "the host draws the gutter: {list}");
        assert_eq!(list["selected"], json!(1), "and marks the line being worked on: {list}");
        assert_eq!(list["rows"].as_array().map(Vec::len), Some(3), "every script row is a row: {list}");
        assert!(list["rows"][0]["text"].as_str().unwrap_or_default().starts_with("✓ "), "a finished row is marked: {list}");
        assert!(dumped.contains("\"font\":\"Inter\""), "style names in their own font: {dumped}");
        assert!(dumped.contains("\"color\":\"#"), "and a colour swatch: {dumped}");
        assert!(dumped.contains("\"icon\":\"◀\""), "glyph buttons: {dumped}");
        // The five-row context label was the stand-in for this list; it is not drawn twice.
        assert!(multi_line_label(&view).is_none());
    }

    #[test]
    fn a_host_without_the_new_widgets_gets_the_plain_page() {
        // One module, two hosts: nothing new may be sent to a host that never advertised it.
        let mut p = Panel::new();
        p.library = starter(Some("Inter"));
        p.script = "d. one\nd. two".into();
        p.reparse();
        let view = render(&p);
        check(&view);
        let dumped = view.to_string();
        for absent in ["\"type\":\"scroll\"", "\"type\":\"list\"", "\"icon\":", "\"font\":", "\"color\":"] {
            assert!(!dumped.contains(absent), "{absent} was sent to a host that lacks it: {dumped}");
        }
        // The stand-ins are still there: the context window and the written-out font of each style.
        assert!(multi_line_label(&view).is_some());
        assert!(dumped.contains("Font: Inter"), "{dumped}");
    }

    #[test]
    fn a_script_longer_than_the_list_is_cut_to_what_the_host_draws() {
        let mut p = Panel::new();
        p.widgets = all_widgets();
        p.library = starter(None);
        p.script = (0..LIST_MAX + 5).map(|i| format!("d. line {i}")).collect::<Vec<_>>().join("\n");
        p.reparse();
        let view = render(&p);
        check(&view);
        let list = find_list(&view, "scriptLine").expect("the script list is drawn");
        assert_eq!(list["rows"].as_array().map(Vec::len), Some(LIST_MAX), "the list stops at the host's cap");
        assert!(view.to_string().contains("shows the first 1000 lines"), "and says the rest is in the script box");
    }

    #[test]
    fn a_hidden_finished_row_keeps_the_numbers_it_has_in_the_script() {
        // With rows hidden the host's gutter would count 1..n over the rows that are left, which
        // are not the script's numbers. The row carries its own number instead.
        let mut p = Panel::new();
        p.widgets = all_widgets();
        p.library = starter(None);
        p.script = "d. one\nd. two\nd. three".into();
        p.reparse();
        p.hide_done = true;
        p.done.insert(0);
        let view = render(&p);
        check(&view);
        let list = find_list(&view, "scriptLine").expect("the script list is drawn");
        assert_eq!(list["numbered"], json!(false), "{list}");
        let rows = list["rows"].as_array().cloned().unwrap_or_default();
        assert_eq!(rows.len(), 2);
        assert!(rows[0]["text"].as_str().unwrap_or_default().contains("   2 "), "{list}");
        assert!(rows[1]["text"].as_str().unwrap_or_default().contains("   3 "), "{list}");
    }

    #[test]
    fn every_style_row_says_which_font_it_letters_in() {        // TypeR's library is a row per style type — Dialogue, Whisper, Shout… — and the typeface
        // behind each one used to be visible only by opening the style and reading a dropdown.
        let mut p = Panel::new();
        p.library = starter(Some("Inter"));
        for (id, family) in [("s1", "Comic Sans MS"), ("s2", "Inter"), ("s3", "Impact")] {
            if let Some(s) = p.library.styles.iter_mut().find(|s| s.id == id) {
                let face = props::font_style(&s.text_props);
                props::set_font(&mut s.text_props, family, &face, None);
            }
        }
        p.page = Page::Styles;
        let view = render(&p);
        check(&view);
        let dumped = view.to_string();
        assert!(dumped.contains("Font: Comic Sans MS Regular · 14 pt"), "Dialogue's font is on its row: {dumped}");
        assert!(dumped.contains("Font: Inter Italic · 10 pt"), "Whisper's font is on its row: {dumped}");
        assert!(dumped.contains("Font: Impact Bold · 22 pt"), "Shout's font is on its row: {dumped}");
    }

    #[test]
    fn the_editor_names_the_font_at_its_top_and_says_what_the_host_listed() {
        let mut p = Panel::new();
        p.library = starter(Some("Comic Sans MS"));
        p.start_edit("s1", false);
        p.page = Page::Style;
        p.fonts.families = vec!["Comic Sans MS".into(), "Inter".into()];
        let view = render(&p);
        check(&view);
        let dumped = view.to_string();
        assert!(dumped.contains("Editing “Dialogue”"), "{dumped}");
        assert!(dumped.contains("Font: Comic Sans MS Regular · 14 pt"), "the font is stated above the controls: {dumped}");
        assert!(dumped.contains("2 from this host"), "and where the families came from: {dumped}");
        assert!(!dumped.contains("this host has listed no fonts"), "{dumped}");

        // A host that listed nothing says so, rather than leaving an unexplained dropdown.
        p.fonts.families.clear();
        let empty = render(&p).to_string();
        assert!(empty.contains("this host has listed no fonts"), "{empty}");
        assert!(!empty.contains("from this host"), "{empty}");
    }

    #[test]
    fn a_style_naming_an_absurd_font_still_draws() {
        // A library is JSON the letterer can edit in Help: a family or face name can be any length,
        // and the host refuses the whole tree when one option is over 256 bytes. Cut, don't stop.
        let mut p = Panel::new();
        p.library = starter(None);
        if let Some(s) = p.library.styles.first_mut() {
            props::set_font(&mut s.text_props, &"F".repeat(500), &format!("{}é", "R".repeat(400)), None);
        }
        p.start_edit("s1", false);
        for page in [Page::Styles, Page::Style] {
            p.page = page;
            check(&render(&p));
        }
    }

    #[test]
    fn a_style_the_host_has_no_font_for_is_still_offered() {
        // A library travels between machines: the font is not installed here, and the panel must
        // still be drawable, with the current value in the dropdown.
        let mut p = Panel::new();
        p.library = starter(None);
        p.start_edit("s1", false);
        p.page = Page::Style;
        if let Some(e) = p.edit.as_mut() {
            props::set_font(&mut e.props, "Nonexistent Sans", "Black Italic", None);
        }
        p.fonts.families = vec!["Inter".into(), "DejaVu Sans".into()];
        let view = render(&p);
        check(&view);
        let dumped = view.to_string();
        assert!(dumped.contains("Nonexistent Sans"), "the style's own font is offered");
        assert!(dumped.contains("not installed"), "and says what it is");
    }

    #[test]
    fn the_editor_says_when_a_style_asks_for_something_this_host_cannot_apply() {
        // Adobe's paragraph shape carries hanging punctuation and the justification methods, and every
        // style has them at their inert defaults. A style that *departs* from those is asking for
        // something the editor has no control for, and an option that silently does nothing is worse
        // than one that says so.
        let mut p = Panel::new();
        p.library = starter(None);
        p.start_edit("s1", false);
        p.page = Page::Style;
        let quiet = render(&p).to_string();
        assert!(!quiet.contains("nothing to apply it with"), "the inert defaults are not a request: {quiet}");

        if let Some(e) = p.edit.as_mut() {
            props::set_para(&mut e.props, "hangingRoman", json!(false));
            props::set_para(&mut e.props, "burasagari", json!("burasagariStandard"));
        }
        let view = render(&p);
        check(&view);
        let loud = view.to_string();
        assert!(loud.contains("hangingRoman"), "{loud}");
        assert!(loud.contains("burasagari"), "{loud}");
        assert!(loud.contains("nothing to apply it with"), "{loud}");
    }

    #[test]
    fn the_face_dropdown_says_when_the_host_has_no_fonts_at_all() {
        // A build with no font directory, or a first run before the enumeration finishes: an empty
        // dropdown with nothing said about it looks like the panel is broken.
        let mut p = Panel::new();
        p.library = starter(None);
        p.start_edit("s1", false);
        p.page = Page::Style;
        p.fonts = Default::default();
        let view = render(&p);
        check(&view);
        assert!(view.to_string().contains("this host has listed no fonts"), "{view}");
    }

    #[test]
    fn an_absurd_style_does_not_make_a_view_the_host_would_refuse() {
        let mut p = Panel::new();
        p.page = Page::Style;
        p.library.styles.push(Style::new("odd", "x".repeat(400)));
        p.start_edit("odd", false);
        if let Some(e) = p.edit.as_mut() {
            props::set_char(&mut e.props, "tracking", json!(f64::INFINITY));
            props::set_char(&mut e.props, "horizontalScale", json!(-40.0));
            props::set_char(&mut e.props, "size", Value::Null);
            props::set_para(&mut e.props, "autoLeadingPercentage", json!(5000.0));
            props::set_char(&mut e.props, "underline", json!({"weird": true}));
            props::set_para(&mut e.props, "hyphenate", json!("yes"));
            props::set_char(&mut e.props, "fontName", json!(""));
            props::set_para(&mut e.props, "alignment", json!("sideways"));
        }
        let view = render(&p);
        check(&view);
        // A 400-character name is cut to what a label can hold, and the page is still a page.
        assert!(view.to_string().contains('…'));
    }

    #[test]
    fn every_widget_id_the_panel_draws_is_one_the_panel_answers() {
        // The panel's `click` and `change` are the other half of the contract: an id it draws but
        // does not handle is a control that does nothing, which is worse than one that is missing.
        let known = |id: &str| {
            let prefix = |p: &str, s: &str| id.strip_prefix(p).and_then(|r| r.strip_suffix(s)).is_some_and(|n| n.parse::<usize>().is_ok());
            matches!(
                id,
                "page"
                    | "script"
                    | "hideDone"
                    | "lineNo"
                    | "textScale"
                    | "defaultStyle"
                    | "set_pointText"
                    | "set_ignore"
                    | "set_ignoreTags"
                    | "set_folderPriority"
                    | "set_markdown"
                    | "set_direction"
                    | "set_padding"
                    | "set_resizeOnCenter"
                    | "set_sizeIncrement"
                    | "set_styleStep"
                    | "set_quickStyleSize"
                    | "set_deselect"
                    | "state_name"
                    | "state_save"
                    | "back"
                    | "openSettings"
                    | "openHelp"
                    | "openFolders"
                    | "set_scaleDefault"
                    | "set_starter"
                    | "set_reset"
                    | "prev"
                    | "next"
                    | "first"
                    | "last"
                    | "create"
                    | "insert"
                    | "align"
                    | "sizeDown"
                    | "sizeUp"
                    | "apply"
                    | "applyKeep"
                    | "newStyle"
                    | "ed_save"
                    | "ed_delete"
                    | "ed_cancel"
                    | "ed_up"
                    | "ed_down"
                    | "ed_copy"
                    | "f_name"
                    | "f_parent"
                    | "f_new"
                    | "f_save"
                    | "f_cancel"
                    | "io_json"
                    | "io_export"
                    | "io_load"
                    | "io_merge"
                    | "io_reset"
                    | "ed_name"
                    | "ed_prefixes"
                    | "ed_color"
                    | "ed_folder"
                    | "ed_font"
                    | "ed_face"
                    | "ed_size"
                    | "ed_leading"
                    | "ed_autoLeading"
                    | "ed_autoLeadingPct"
                    | "ed_tracking"
                    | "ed_kerning"
                    | "ed_kernPair"
                    | "ed_hScale"
                    | "ed_vScale"
                    | "ed_baseline"
                    | "ed_script"
                    | "ed_caps"
                    | "ed_align"
                    | "ed_fauxBold"
                    | "ed_fauxItalic"
                    | "ed_underline"
                    | "ed_strike"
                    | "ed_ligatures"
                    | "ed_discLigatures"
                    | "ed_hyphenate"
                    | "ed_firstIndent"
                    | "ed_startIndent"
                    | "ed_endIndent"
                    | "ed_spaceBefore"
                    | "ed_spaceAfter"
                    | "ed_colorHex"
                    | "ed_antiAlias"
                    | "scriptLine"
                    | "scriptBoxOpen"
                    | "scriptBoxClose"
                    // The panes' ids name the host's scroll state, not a control: nothing is sent
                    // when one is scrolled.
                    | "scriptPane"
                    | "stylesPane"
            ) || prefix("s", "_apply")
                || prefix("s", "_edit")
                || prefix("f", "_edit")
                || prefix("f", "_del")
                || prefix("f", "_up")
                || prefix("f", "_down")
                || prefix("f", "_export")
                || prefix("s", "_size")
                || prefix("s", "_sizeDown")
                || prefix("s", "_sizeUp")
                || prefix("st", "_load")
                || prefix("st", "_del")
        };
        fn ids(v: &Value, out: &mut Vec<String>) {
            if let Some(id) = v.get("id").and_then(Value::as_str) {
                out.push(id.to_string());
            }
            for key in ["children", "footer"] {
                if let Some(list) = v.get(key).and_then(Value::as_array) {
                    for c in list {
                        ids(c, out);
                    }
                }
            }
        }
        for (what, mut p) in panels() {
            for page in [Page::Lettering, Page::Styles, Page::Style, Page::Folders, Page::Settings, Page::Help] {
                p.page = page;
                for busy in [false, true] {
                    p.run = busy.then(|| Run::begin(Kind::Create, "test".into(), vec![]));
                    let mut found = Vec::new();
                    ids(&render(&p), &mut found);
                    for id in found {
                        assert!(known(&id), "{what}/{:?} draws {id}, which the panel does not answer", page.id());
                    }
                }
                p.run = None;
            }
        }
    }

    #[test]
    fn the_panel_is_three_panes_and_a_footer_with_no_page_tabs() {
        // TypeR's layout: preview, then the script, then the style library. Help and Settings are
        // buttons in the footer, and every page that replaces the panes has the way back.
        let p = Panel::new();
        let dumped = render(&p).to_string();
        assert!(dumped.contains("\"id\":\"script\""), "the script pane is drawn");
        assert!(dumped.contains("\"id\":\"create\""), "the preview pane is drawn");
        assert!(dumped.contains("\"id\":\"newStyle\""), "the library pane is drawn");
        assert!(dumped.contains("\"id\":\"openSettings\"") && dumped.contains("\"id\":\"openHelp\""));
        assert!(!dumped.contains("\"id\":\"page\""), "no page switcher: the panes are the panel");
        for page in [Page::Style, Page::Folders, Page::Settings, Page::Help] {
            let mut p = Panel::new();
            p.page = page;
            let dumped = render(&p).to_string();
            assert!(dumped.contains("\"id\":\"back\""), "{:?} has a Back button: {dumped}", page);
        }
    }

    #[test]
    fn the_footer_says_what_is_selected_and_what_the_line_says() {
        let mut p = Panel::new();
        assert!(render(&p).to_string().contains("no document open"));
        p.doc = Doc::from_json(&json!({"width": 10, "height": 10, "resolution": 72, "hasSelection": false, "layers": []}));
        let dumped = render(&p).to_string();
        assert!(dumped.contains("no selection"), "{dumped}");
        assert!(dumped.contains("no type layer selected"), "{dumped}");
        assert!(dumped.contains("The script is empty"), "{dumped}");
        // The line preview names the style the line's prefix chose, and its text.
        p.library = starter(None);
        p.script = "x. BANG".into();
        p.reparse();
        let dumped = render(&p).to_string();
        assert!(dumped.contains("Sound effect"), "{dumped}");
        assert!(dumped.contains("BANG"), "{dumped}");
        assert!(dumped.contains("Line 1 / 1"), "{dumped}");
        // An ignored line says why the buttons skip it.
        p.script = "## a door".into();
        p.reparse();
        let dumped = render(&p).to_string();
        assert!(dumped.contains("ignore prefix"), "{dumped}");
    }

    /// The text of the first label with more than one line in it: the panel's context window is the
    /// only widget that draws several rows in one label.
    fn multi_line_label(v: &Value) -> Option<String> {
        if v.get("type").and_then(Value::as_str) == Some("label")
            && let Some(t) = v.get("text").and_then(Value::as_str)
            && t.contains('\n')
        {
            return Some(t.to_string());
        }
        v.get("children").and_then(Value::as_array).into_iter().flatten().find_map(multi_line_label)
    }

    /// Every label in the tree whose text was cut to fit the host's limit.
    fn cut_labels(v: &Value, out: &mut Vec<String>) {
        if v.get("type").and_then(Value::as_str) == Some("label")
            && let Some(text) = v.get("text").and_then(Value::as_str)
            && text.chars().count() >= LABEL_MAX
            && text.ends_with('…')
        {
            out.push(text.to_string());
        }
        for key in ["children", "footer"] {
            for c in v.get(key).and_then(Value::as_array).into_iter().flatten() {
                cut_labels(c, out);
            }
        }
    }

    /// The value of the `text` widget with this id.
    fn text_value(v: &Value, wanted: &str) -> Option<String> {
        if v.get("type").and_then(Value::as_str) == Some("text") && v.get("id").and_then(Value::as_str) == Some(wanted) {
            return v.get("value").and_then(Value::as_str).map(str::to_string);
        }
        v.get("children").and_then(Value::as_array).into_iter().flatten().find_map(|c| text_value(c, wanted))
    }

    /// What the host draws, in the order it draws it: each widget's id where it has one, its text
    /// where it has none, so a test can say what comes before what without reading the JSON.
    fn drawn_in_order(v: &Value) -> Vec<String> {
        fn walk(v: &Value, out: &mut Vec<String>) {
            if let Some(s) = ["id", "text"].iter().find_map(|k| v.get(*k).and_then(Value::as_str)) {
                out.push(s.to_string());
            }
            for key in ["children", "footer"] {
                for c in v.get(key).and_then(Value::as_array).into_iter().flatten() {
                    walk(c, out);
                }
            }
        }
        let mut out = Vec::new();
        walk(v, &mut out);
        out
    }

    /// The widget with this id, anywhere in the tree.
    fn widget_with_id(v: &Value, wanted: &str) -> Option<Value> {
        if v.get("id").and_then(Value::as_str) == Some(wanted) {
            return Some(v.clone());
        }
        v.get("children")
            .and_then(Value::as_array)
            .into_iter()
            .flatten()
            .chain(v.get("footer").and_then(Value::as_array).into_iter().flatten())
            .find_map(|c| widget_with_id(c, wanted))
    }

    #[test]
    fn the_main_view_is_the_line_the_script_the_buttons_and_the_size() {
        // The order a letterer meets the panel in: where they are, the script (the pane that gets
        // the most height), the one row of buttons, the size, and the library.
        let mut p = Panel::new();
        p.widgets = all_widgets();
        p.library = starter(Some("Inter"));
        p.script = "d. one\nd. two".into();
        p.reparse();
        let view = render(&p);
        check(&view);
        let order = drawn_in_order(&view);
        let pos = |want: &str| order.iter().position(|s| s == want).unwrap_or_else(|| panic!("{want} is drawn: {order:?}"));
        assert!(pos("Line 1 / 2") < pos("scriptLine"), "the line comes first: {order:?}");
        assert!(pos("scriptLine") < pos("create"), "then the script: {order:?}");
        assert!(pos("create") < pos("textScale"), "the buttons are one row, the size under them: {order:?}");
        assert!(pos("textScale") < pos("s0_apply"), "and the library under that: {order:?}");
        let script = widget_with_id(&view, "scriptPane").expect("the script is a pane");
        let styles = widget_with_id(&view, "stylesPane").expect("the library is a pane");
        assert!(
            script["weight"].as_u64() > styles["weight"].as_u64(),
            "the script is the main pane: {} against {}",
            script["weight"],
            styles["weight"]
        );
    }

    #[test]
    fn the_size_slider_says_what_the_size_is() {
        // The host's slider is a track and a knob: without a label beside it the row reads `%`.
        let mut p = Panel::new();
        p.scale = 137.0;
        let dumped = render(&p).to_string();
        assert!(dumped.contains("137%"), "the value is written out: {dumped}");
        assert!(!dumped.contains("\"text\":\"%\""), "and not a bare percent sign: {dumped}");
    }

    #[test]
    fn the_size_steps_are_framed_buttons_and_the_arrows_are_not() {
        // A−/A+ are pressed over and over while a line is fitted, so they wear a frame; the arrows
        // that walk the script stay glyph buttons where the host draws those.
        let mut p = Panel::new();
        p.widgets = all_widgets();
        let view = render(&p);
        for id in ["create", "insert", "align", "sizeDown", "sizeUp"] {
            let w = widget_with_id(&view, id).unwrap_or_else(|| panic!("{id} is drawn"));
            assert!(w.get("icon").is_none(), "{id} is drawn with a frame, not as a bare glyph");
        }
        assert_eq!(widget_with_id(&view, "sizeDown").expect("A− is drawn")["label"], "A−");
        assert_eq!(widget_with_id(&view, "sizeUp").expect("A+ is drawn")["label"], "A+");
        assert_eq!(widget_with_id(&view, "prev").expect("the arrows are drawn")["icon"], "◀");
    }

    #[test]
    fn the_explanations_are_under_the_question_mark() {
        // The main view keeps one line of guidance — how a script is written — and the paragraphs
        // that explain the size and the buttons are a press away, in Help.
        let main = render(&Panel::new()).to_string();
        assert!(!main.contains("Text size scales the style's size"), "the main view is not a manual: {main}");
        assert!(!main.contains("A−/A+ step the type by"), "{main}");
        assert!(main.contains("one line of dialogue per row"), "how a line is written stays: {main}");
        let mut p = Panel::new();
        p.page = Page::Help;
        let help = render(&p).to_string();
        assert!(help.contains("Text size scales the style's size"), "and the explanation is under `?`: {help}");
        assert!(help.contains("A−/A+ step the type by"), "{help}");
        assert!(help.contains("Apply, keep size gives them that look"), "{help}");
    }

    #[test]
    fn the_apply_buttons_are_with_the_library_they_apply() {
        // Apply acts with the library — the style it puts on the layers is the one the list has in
        // hand — so it lives in the library's pinned footer, not in the row of buttons above.
        let mut p = Panel::new();
        p.widgets = all_widgets();
        p.library = starter(Some("Inter"));
        let view = render(&p);
        check(&view);
        let order = drawn_in_order(&view);
        let pos = |want: &str| order.iter().position(|s| s == want).unwrap_or_else(|| panic!("{want} is drawn: {order:?}"));
        assert!(pos("apply") > pos("create"), "Apply is not in the row of buttons: {order:?}");
        assert!(pos("apply") > pos("s0_apply"), "and it is drawn with the library: {order:?}");
        let footer = widget_with_id(&view, "stylesPane").expect("the library is a pane");
        let footer = footer["footer"].to_string();
        assert!(footer.contains("apply") && footer.contains("applyKeep"), "pinned, so it stays in reach: {footer}");
    }

    #[test]
    fn no_explanation_the_panel_writes_is_cut_short() {
        // A label is cut at the host's 256 characters with a `…`. That is right for a script line or
        // a style's name, which are data, and wrong for the panel's own prose: half an explanation
        // reads as a complete one. With a fresh panel every label is the panel's own.
        let mut p = Panel::new();
        for page in [Page::Lettering, Page::Styles, Page::Style, Page::Folders, Page::Settings, Page::Help] {
            p.page = page;
            let view = render(&p);
            let mut cut_short = Vec::new();
            cut_labels(&view, &mut cut_short);
            assert!(cut_short.is_empty(), "{page:?} cuts its own text: {cut_short:?}");
        }
    }

    #[test]
    fn the_script_pane_carries_the_button_that_puts_the_box_away_and_brings_it_back() {
        // The box is the importer: with a script in, the pane is the script itself and the box is in
        // the way of it, so the header carries the press that brings it back.
        let mut p = Panel::new();
        p.widgets = all_widgets();
        p.script = "d. one\nd. two".into();
        p.reparse();
        let open = render(&p);
        assert!(widget_with_id(&open, "script").is_some(), "the box is drawn while it is open");
        assert!(widget_with_id(&open, "scriptBoxClose").is_some());
        p.script_box = false;
        let view = render(&p);
        check(&view);
        assert!(widget_with_id(&view, "script").is_none(), "and not once it is put away");
        assert!(widget_with_id(&view, "scriptBoxOpen").is_some(), "the press that brings it back");
        assert!(widget_with_id(&view, "scriptLine").is_some(), "the lines are still there to work through");
        let dumped = view.to_string();
        assert!(dumped.contains("Edit script"), "{dumped}");
        assert!(dumped.contains("2 line(s) read"), "and says where the script went: {dumped}");
        // An empty panel says what to do instead of naming a count of nothing.
        let mut fresh = Panel::new();
        fresh.widgets = all_widgets();
        fresh.script_box = false;
        assert!(render(&fresh).to_string().contains("No script yet"));
    }

    #[test]
    fn the_shortcuts_page_names_the_host_keys_in_this_platforms_notation() {
        // The panel owns no keys — Help says why — so the Settings page names the host's own, says
        // where they are changed, and spells them the way the platform the host reported at `init`
        // spells them: ⌘⇧. against Ctrl+Shift+. A Linux letterer sent looking for "Cmd" is being sent
        // after a key their keyboard does not have.
        let mut p = Panel::new();
        p.page = Page::Settings;
        p.platform = "macos".into();
        let mac = render(&p);
        check(&mac);
        let mac = mac.to_string();
        assert!(mac.contains("⌘⇧.") && mac.contains("⌘⇧,"), "the size steps: {mac}");
        assert!(mac.contains("⌘⌥⇧"), "and the by-10 steps carry Alt: {mac}");
        p.platform = "linux".into();
        let linux = render(&p).to_string();
        assert!(linux.contains("Ctrl+Shift+.") && linux.contains("Ctrl+Alt+Shift+,"), "{linux}");
        assert!(!linux.contains('⌘') && !linux.contains('⇧'), "and no Mac glyphs off a Mac: {linux}");
        assert!(linux.contains("Edit ▸ Keyboard Shortcuts"), "where a key is changed: {linux}");
        assert!(linux.contains("New Type Layer"), "and the host rows worth a key: {linux}");
        // A host that reports nothing gets the non-Mac spelling rather than a guess at a Mac one.
        p.platform = String::new();
        assert!(render(&p).to_string().contains("Ctrl+Shift+."));
    }

    #[test]
    fn the_top_of_the_panel_is_the_line_and_a_window_of_the_lines_around_it() {
        let mut p = Panel::new();
        p.library = starter(None);
        p.script = "d. one\nd. two\nd. three\nd. four\nd. five\nd. six\nd. seven".into();
        p.reparse();
        p.cursor = 3;
        p.done.insert(2); // a finished row inside the window
        let view = render(&p);
        check(&view);
        let dumped = view.to_string();
        assert!(dumped.contains("Line 4 / 7"), "the counter is where the work is: {dumped}");
        let block = multi_line_label(&view).expect("the context window is one multi-line label");
        assert!(block.contains("▶   4 four"), "the current row is marked: {block}");
        assert!(block.contains("✓   3 three"), "a finished row is marked: {block}");
        assert!(block.contains("two") && block.contains("six"), "two rows before and three after: {block}");
        assert!(!block.contains("one"), "and no further back: {block}");
        assert!(block.contains("seven"), "the third row after the current one: {block}");
        assert_eq!(block.lines().count(), 6);
    }

    #[test]
    fn the_context_label_fits_what_the_host_will_draw() {
        // A label over 256 characters makes the host refuse the whole view, and a script row can be
        // as long as the script itself.
        let long = format!("d. {}", "x".repeat(SCRIPT_MAX - 3));
        let mut p = Panel::new();
        p.library = starter(None);
        p.script = [long.as_str(); 8].join("\n");
        p.reparse();
        p.cursor = 4;
        let view = render(&p);
        check(&view);
        let block = multi_line_label(&view).expect("the context window is one multi-line label");
        assert!(block.chars().count() <= LABEL_MAX, "{} characters", block.chars().count());
    }

    #[test]
    fn finished_rows_can_be_left_out_of_the_script_box_and_are_listed_under_done() {
        let mut p = Panel::new();
        p.library = starter(None);
        p.script = "d. one\nd. two\nd. three".into();
        p.reparse();
        p.done.insert(0);
        let view = render(&p);
        check(&view);
        assert_eq!(text_value(&view, "script").as_deref(), Some("d. one\nd. two\nd. three"), "visible until the letterer asks");

        p.hide_done = true;
        let view = render(&p);
        check(&view);
        assert_eq!(text_value(&view, "script").as_deref(), Some("d. two\nd. three"), "the finished row is left out");
        let dumped = view.to_string();
        assert!(dumped.contains("\"title\":\"Done\""), "{dumped}");
        assert!(dumped.contains("1 finished row(s)"), "{dumped}");
        // The title is stable: the host remembers whether a section is open by its title, so a count
        // in the title would reopen it every time a row was lettered.
        p.done.insert(1);
        let dumped = render(&p).to_string();
        assert!(dumped.contains("\"title\":\"Done\""), "{dumped}");
        assert!(!dumped.contains("\"title\":\"Done (2)\""), "{dumped}");
    }


    #[test]
    fn a_huge_script_is_clipped_to_what_a_widget_may_hold() {
        let big = "x".repeat(SCRIPT_MAX + 500);
        assert_eq!(clip(&big).chars().count(), SCRIPT_MAX);
        let mut p = Panel::new();
        p.page = Page::Lettering;
        p.script = big.clone();
        check(&render(&p));
        p.page = Page::Help;
        p.io = big;
        check(&render(&p));
        // And a library with more styles than a page draws says so instead of overflowing.
        p.page = Page::Styles;
        p.library = starter(None);
        for i in 0..60 {
            p.library.styles.push(Style::new(format!("z{i}"), "x".repeat(300)));
        }
        let view = render(&p);
        check(&view);
        assert!(view.to_string().contains("more styles than this page draws"));
    }

    #[test]
    fn the_helpers_fit_what_they_are_given() {
        assert_eq!(cut("short"), "short");
        assert_eq!(cut(&"x".repeat(300)).chars().count(), LABEL_MAX);
        assert_eq!(finite(f64::NAN, 7.0), 7.0);
        assert_eq!(finite(f64::INFINITY, 7.0), 7.0);
        // A number is clamped into its own range, and a swapped range is put the right way round.
        assert_eq!(number("n", 50.0, 0.0, 10.0, 1.0, false)["value"], json!(10.0));
        assert_eq!(number("n", f64::NAN, 5.0, 1.0, 1.0, false)["min"], json!(1.0));
        assert_eq!(number("n", 3.0, f64::NAN, 1.0, 1.0, false)["value"], json!(3.0));
        // A choice always offers its own value, and an unknown one is labelled as such.
        let c = choice("c", "Gone", vec![("a".into(), "A".into())]);
        assert_eq!(c["options"][0]["value"], json!("Gone"));
        assert_eq!(c["options"][1]["value"], json!("a"));
        // …and options the host would refuse are dropped rather than passed on.
        let c =
            choice("c", "a", vec![("a".into(), "A".into()), (String::new(), "empty".into()), ("x".repeat(300), "long".into()), ("a".into(), "again".into())]);
        assert_eq!(c["options"].as_array().map(Vec::len), Some(1));
        // A choice that has nothing at all to offer still has one option, because the host refuses
        // a dropdown with none.
        let c = choice("c", "", Vec::new());
        assert_eq!(c["options"].as_array().map(Vec::len), Some(1));
        check(&c);
    }

    #[test]
    fn a_library_of_fifty_styles_is_still_a_view_the_host_accepts() {
        // The host allows 512 widgets; this is the shape that gets closest to it.
        let mut p = Panel::new();
        p.page = Page::Styles;
        p.library = starter(None);
        while p.library.styles.len() < MAX_ROWS {
            p.library.styles.push(Style::new(format!("x{}", p.library.styles.len()), "A style"));
        }
        for page in [Page::Styles, Page::Style] {
            p.page = page;
            check(&render(&p));
        }
    }

    #[test]
    fn the_help_page_is_honest_about_what_the_abi_cannot_do() {
        // The gaps are part of the product: a letterer has to be told that a panel cannot add a
        // shortcut, rather than left pressing one that never fires.
        let mut p = Panel::new();
        p.page = Page::Help;
        let dumped = render(&p).to_string();
        assert!(dumped.contains("cannot add any"), "{dumped}");
        assert!(dumped.contains("no colour widget"), "{dumped}");
        assert!(dumped.contains("burasagari"), "{dumped}");
        assert!(dumped.contains("TypeR 2.5.0"), "the port says what it is a port of");
        assert!(dumped.contains("2017 Swirt"), "and credits it");
    }
}
