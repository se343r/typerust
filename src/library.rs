//! The style library: the styles, the folders they live in, and the settings beside them.
//!
//! The shape is the one **TypeR 2.5.0** keeps in its state (`app_src/context.jsx`), so a library can
//! move between Photoshop TypeR and this port in either direction: folders are
//! `{id, name, parentId, order}` and nested, styles are
//! `{id, name, folder, prefixes, prefixColor, textProps, edited, prefixesDisabled?}`, and `textProps`
//! is kept verbatim in the `layerText` shape TypeR writes — the port never rewrites a property it
//! does not understand, so a style edited here and taken back to Photoshop keeps everything.
//!
//! What this file ports from TypeR, with its source:
//!
//! * [`Folder`] nesting and ordering, and the normalisation `normalizeFolders` does
//!   (`context.jsx:54-87`): an empty, missing or dangling `parentId` becomes root, a missing `order`
//!   is 0, and each sibling group is stably sorted and renumbered `0..n`.
//! * [`Library::resort`], TypeR's flattening (`context.jsx:677-688`): unsorted styles first, then
//!   root folders in order, and within each folder its own styles before its child folders'. This is
//!   the order prefixes match in, so it is behaviour, not presentation.
//! * [`Library::prefixes`], the four prefix lists and the folder-priority rule
//!   (`context.jsx:690-729`).
//! * `ignoreTags`, `currentFolderTagPriority` and `prefixesDisabled`, all of which change what a
//!   pasted script matches.
//! * Prefix strings split the way TypeR's `saveStyle` splits them (`context.jsx:433-441`): on a line
//!   break or a `;`, trimmed, blanks dropped. The editor shows them one per line, as TypeR's does.
//!
//! Deliberate differences, all of them ways this port refuses to lose a letterer's work:
//!
//! * **A folder cycle cannot hide styles.** TypeR's flattening walks only from folder roots, so a
//!   style inside a cycle (`A → B → A`) is silently dropped from the library it hands to matching.
//!   Here any style the walk did not reach is appended after the walk; nothing disappears.
//! * **Duplicate folder ids are dropped, keeping the first**, the same rule [`Library::from_json`]
//!   already applies to styles: a second folder with an id everything above already refers to can
//!   only shadow it.
//! * **The import shape is this port's own storage shape**, not TypeR's export. TypeR's export
//!   (`TypeR_Export.json`, `{folders, styles, version, exported, …settings}`) and its single-folder
//!   export (`{name, exportedStyles}`) are written by [`export_json`] and [`export_folder_json`],
//!   which is where their filters and omissions live; this storage format is a superset that
//!   round-trips.
//! * **`textItemKind` is still read back.** TypeR exports it from a field that does not exist
//!   (`state.setTextItemKind`, `export.jsx:108`), so its own exports omit it; this port's files carry
//!   the real setting, and a legacy TyperTools file that names it is accepted too.

use serde_json::{Map, Value, json};

use crate::props;
use crate::script::Prefixes;

/// TypeR's default prefix colour (`config.defaultPrefixColor`).
pub const DEFAULT_PREFIX_COLOR: &str = "#FFF3B0";

/// The prefix colours TypeR offers as presets, in its order.
pub const PREFIX_COLORS: [&str; 8] = ["#FFA6A4", "#FDD4BB", "#FFF3B0", "#CDFDD3", "#BEDADC", "#C2F2FF", "#C3C9FF", "#F6D5FF"];

/// The version this storage format writes. TypeR 2.5.0 is the reference.
pub const FORMAT_VERSION: &str = "2.5.0";

/// The deepest folder nesting [`Library::resort`] follows. TypeR has no limit (and no depth guard in
/// its own tree walk); a library file is input, and this port must not be driven into a stack
/// overflow by one. Folders past this depth keep their styles — the leftovers rule below covers them.
pub const MAX_FOLDER_DEPTH: usize = 64;

/// A folder of styles. Folders nest: `parent_id` is the folder this one is inside.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Folder {
    pub id: String,
    pub name: String,
    /// `None` for a folder at the root of the library.
    pub parent_id: Option<String>,
    /// Position among its siblings. Renumbered `0..n` by [`Library::normalize_folders`], so a saved
    /// order that skips numbers still comes back in the same order.
    pub order: u32,
}

impl Folder {
    /// A folder at the root, at the end of its siblings.
    pub fn new(id: impl Into<String>, name: impl Into<String>) -> Folder {
        Folder { id: id.into(), name: name.into(), parent_id: None, order: 0 }
    }
}

/// One saved text style: the name, where it lives, the script prefixes that pick it, and the text
/// properties it applies (the jam `textProps` object, verbatim).
#[derive(Clone, Debug, PartialEq)]
pub struct Style {
    pub id: String,
    pub name: String,
    /// The folder's id, or `None` for the unsorted group TypeR shows first.
    pub folder: Option<String>,
    pub prefixes: Vec<String>,
    /// TypeR's `prefixesDisabled`: the style stays in the library and is chosen by hand, but its
    /// prefixes never claim a line.
    pub prefixes_disabled: bool,
    pub prefix_color: String,
    /// Milliseconds since the epoch, as TypeR stamps it (`Date.now()`); 0 when unknown. Used only to
    /// settle an import that collides with a style edited locally.
    pub edited: u64,
    pub text_props: Value,
}

impl Style {
    /// A new style with TypeR's starting properties: the default family at 14, centred.
    pub fn new(id: impl Into<String>, name: impl Into<String>) -> Style {
        Style {
            id: id.into(),
            name: name.into(),
            folder: None,
            prefixes: Vec::new(),
            prefixes_disabled: false,
            prefix_color: DEFAULT_PREFIX_COLOR.to_string(),
            edited: 0,
            text_props: props::default_props(),
        }
    }

    /// The prefixes as one multi-line string, the way the editor shows them.
    pub fn prefixes_text(&self) -> String {
        self.prefixes.join("\n")
    }

    /// Splits a prefix list the way TypeR's `saveStyle` does
    /// (`prefixes.split(/(?:\r?\n|;)/).map(p => p.trim()).filter(Boolean)`): one per line or
    /// semicolon, trimmed, blanks dropped.
    pub fn set_prefixes_text(&mut self, text: &str) {
        self.prefixes = text
            .split(['\n', ';'])
            .map(str::trim)
            .filter(|p| !p.is_empty())
            .map(str::to_string)
            .collect();
    }

    fn to_json(&self) -> Value {
        let mut v = json!({
            "id": self.id,
            "name": self.name,
            "folder": self.folder,
            "prefixes": self.prefixes,
            "prefixColor": self.prefix_color,
            "textProps": self.text_props,
            "edited": self.edited,
        });
        // TypeR leaves the key off until it is toggled; writing it only when true keeps the file
        // shaped like TypeR's own state objects.
        if self.prefixes_disabled {
            v["prefixesDisabled"] = json!(true);
        }
        v
    }

    fn from_json(v: &Value) -> Option<Style> {
        let id = v.get("id").and_then(Value::as_str)?.to_string();
        Some(Style::from_json_with_id(v, id))
    }

    /// The style's fields, with an id supplied by the caller. TypeR's single-folder export carries
    /// no ids (`stylesBlock.jsx:104-125`), so its importer makes them.
    fn from_json_with_id(v: &Value, id: String) -> Style {
        Style {
            id,
            name: v.get("name").and_then(Value::as_str).unwrap_or("Style").to_string(),
            folder: v.get("folder").and_then(Value::as_str).filter(|f| !f.is_empty()).map(str::to_string),
            prefixes: strings(v.get("prefixes")),
            prefixes_disabled: v.get("prefixesDisabled").and_then(Value::as_bool).unwrap_or(false),
            prefix_color: v.get("prefixColor").and_then(Value::as_str).unwrap_or(DEFAULT_PREFIX_COLOR).to_string(),
            edited: v.get("edited").and_then(Value::as_u64).unwrap_or(0),
            text_props: props::normalise(v.get("textProps").cloned().unwrap_or(Value::Null)),
        }
    }
}

/// A list of strings: an array, or TypeR's string form. A string is split on newlines or semicolons
/// — the same rule [`Style::set_prefixes_text`] uses — rather than on whitespace, because a prefix
/// may contain a space (`"## "`, which a script can use to mark a note explicitly).
fn strings(v: Option<&Value>) -> Vec<String> {
    match v {
        Some(Value::Array(a)) => a.iter().filter_map(Value::as_str).map(str::to_string).collect(),
        Some(Value::String(s)) => s
            .split(['\n', ';'])
            .map(str::trim)
            .filter(|p| !p.is_empty())
            .map(str::to_string)
            .collect(),
        _ => Vec::new(),
    }
}

/// Everything the panel keeps: the styles, the folders, and the settings around them.
#[derive(Clone, Debug, PartialEq)]
pub struct Library {
    /// In **matching order**: the unsorted styles first, then each folder's, as TypeR sorts them.
    /// The first style whose prefix a line starts with wins, so this order is behaviour, not
    /// presentation ([`Library::resort`] is what maintains it).
    pub styles: Vec<Style>,
    /// The folders, in the order TypeR's state holds them; display and matching order is by
    /// `order` among siblings ([`Library::child_folders`]).
    pub folders: Vec<Folder>,
    /// TypeR's `ignoreLinePrefixes` (default `["##"]`): a line starting with one is not dialogue.
    pub ignore_prefixes: Vec<String>,
    /// TypeR's `ignoreTags`: tokens removed from the dialogue wherever they appear.
    pub ignore_tags: Vec<String>,
    pub default_style_id: Option<String>,
    /// `true`: Create and Align work on point text. `false` (TypeR's default): on a text box.
    pub point_text: bool,
    /// TypeR's `currentFolderTagPriority` (default `true`): a prefix in the current style's folder
    /// beats every other prefix for the same line.
    pub folder_priority: bool,
}

impl Default for Library {
    fn default() -> Self {
        Library {
            styles: Vec::new(),
            folders: Vec::new(),
            ignore_prefixes: vec!["##".to_string()],
            ignore_tags: Vec::new(),
            default_style_id: None,
            point_text: false,
            folder_priority: true,
        }
    }
}

impl Library {
    pub fn style(&self, id: &str) -> Option<&Style> {
        self.styles.iter().find(|s| s.id == id)
    }

    pub fn style_mut(&mut self, id: &str) -> Option<&mut Style> {
        self.styles.iter_mut().find(|s| s.id == id)
    }

    pub fn folder(&self, id: &str) -> Option<&Folder> {
        self.folders.iter().find(|f| f.id == id)
    }

    /// The styles of one folder in order — or, with `None`, the unsorted ones.
    pub fn styles_in(&self, folder: Option<&str>) -> Vec<&Style> {
        self.styles.iter().filter(|s| s.folder.as_deref() == folder).collect()
    }

    /// The folders directly inside `parent`, in their order — TypeR's `getFolderChildren`
    /// (`context.jsx:104-113`), plus the stable tie-break the usual sort gives.
    pub fn child_folders(&self, parent: Option<&str>) -> Vec<&Folder> {
        let mut children: Vec<&Folder> = self.folders.iter().filter(|f| f.parent_id.as_deref() == parent).collect();
        children.sort_by_key(|f| f.order);
        children
    }

    /// How deep a folder is, for indenting it. A folder that is its own ancestor stops at the first
    /// repeat, so a cyclic library renders instead of hanging.
    pub fn folder_depth(&self, id: &str) -> usize {
        let mut depth = 0;
        let mut at = self.folder(id).and_then(|f| f.parent_id.as_deref());
        let mut seen: Vec<&str> = vec![id];
        while let Some(parent) = at {
            if seen.contains(&parent) || depth >= MAX_FOLDER_DEPTH {
                break;
            }
            seen.push(parent);
            depth += 1;
            at = self.folder(parent).and_then(|f| f.parent_id.as_deref());
        }
        depth
    }

    /// The folder ids in display order: roots by `order`, each followed by its children. Bounded by
    /// [`MAX_FOLDER_DEPTH`] and a seen-set, so a cycle cannot loop.
    pub fn folder_order(&self) -> Vec<String> {
        let mut out: Vec<String> = Vec::new();
        let mut seen: Vec<String> = Vec::new();
        let roots: Vec<String> = self.child_folders(None).iter().map(|f| f.id.clone()).collect();
        // An explicit stack, children pushed in reverse so the first sibling is visited first.
        let mut stack: Vec<(String, usize)> = roots.into_iter().rev().map(|id| (id, 1)).collect();
        while let Some((id, depth)) = stack.pop() {
            if depth > MAX_FOLDER_DEPTH || seen.contains(&id) {
                continue;
            }
            seen.push(id.clone());
            out.push(id.clone());
            let children: Vec<String> = self.child_folders(Some(&id)).iter().map(|f| f.id.clone()).collect();
            for child in children.into_iter().rev() {
                stack.push((child, depth + 1));
            }
        }
        out
    }

    /// TypeR's `normalizeFolders` (`context.jsx:54-87`): settle every parent reference, default
    /// `order`, and renumber each sibling group `0..n` in a stable sort.
    pub fn normalize_folders(&mut self) {
        // A duplicate id is kept once: the first is the folder everything above it refers to.
        let mut seen: Vec<String> = Vec::new();
        self.folders.retain(|f| {
            if seen.contains(&f.id) {
                false
            } else {
                seen.push(f.id.clone());
                true
            }
        });
        let ids: Vec<String> = self.folders.iter().map(|f| f.id.clone()).collect();
        for f in &mut self.folders {
            if let Some(parent) = f.parent_id.clone()
                && (parent == f.id || !ids.contains(&parent))
            {
                f.parent_id = None;
            }
        }
        // Group by parent, in the order each group first appears, and renumber each group.
        let mut groups: Vec<(Option<String>, Vec<usize>)> = Vec::new();
        for (i, f) in self.folders.iter().enumerate() {
            match groups.iter_mut().find(|(parent, _)| *parent == f.parent_id) {
                Some((_, members)) => members.push(i),
                None => groups.push((f.parent_id.clone(), vec![i])),
            }
        }
        for (_, members) in groups {
            let mut members = members;
            members.sort_by_key(|&i| self.folders.get(i).map(|f| f.order).unwrap_or(0));
            for (order, i) in members.into_iter().enumerate() {
                if let Some(f) = self.folders.get_mut(i) {
                    f.order = u32::try_from(order).unwrap_or(u32::MAX);
                }
            }
        }
    }

    /// Puts the unsorted styles first and each folder's after it, keeping the order within each
    /// group: TypeR's flattening, and the order prefixes match in.
    pub fn resort(&mut self) {
        self.normalize_folders();
        // A style naming a folder that is gone would be unreachable: it goes back to the unsorted
        // group, which is where TypeR's post-reducer pass puts it (`context.jsx:655-659`).
        let known: Vec<String> = self.folders.iter().map(|f| f.id.clone()).collect();
        for s in &mut self.styles {
            if s.folder.as_ref().is_some_and(|f| !known.contains(f)) {
                s.folder = None;
            }
        }
        let mut out: Vec<Style> = Vec::with_capacity(self.styles.len());
        let mut taken = vec![false; self.styles.len()];
        for (i, s) in self.styles.iter().enumerate() {
            if s.folder.is_none() {
                out.push(s.clone());
                taken[i] = true;
            }
        }
        for id in self.folder_order() {
            for (i, s) in self.styles.iter().enumerate() {
                if !taken[i] && s.folder.as_deref() == Some(id.as_str()) {
                    out.push(s.clone());
                    taken[i] = true;
                }
            }
        }
        // TypeR's walk starts at the roots, so a style inside a folder cycle never reaches the list.
        // Losing it would lose a letterer's style silently: it goes at the end instead.
        for (i, s) in self.styles.iter().enumerate() {
            if !taken[i] {
                out.push(s.clone());
            }
        }
        self.styles = out;
    }

    /// Every style prefix, in matching order: `(style id, prefix)`. The tests use this as the
    /// readable form of a match order; nothing in the panel calls it, so it is test-only.
    #[cfg(test)]
    fn prefix_order(&self) -> Vec<(String, String)> {
        self.styles
            .iter()
            .filter(|s| !s.prefixes_disabled)
            .flat_map(|s| s.prefixes.iter().map(move |p| (s.id.clone(), p.clone())))
            .collect()
    }

    /// The four prefix lists TypeR builds before matching (`context.jsx:690-705`), ready for
    /// [`script::parse`].
    ///
    /// `current_folder` is the folder of the style in use — TypeR reads it from
    /// `state.currentStyle.folder`, so `None` (no style, or an unsorted one) makes the unsorted
    /// prefixes the ones with priority. That is the original's behaviour, not an approximation: its
    /// test is `folder === currentFolder`, which is true for `null === null` too.
    pub fn prefixes(&self, current_folder: Option<&str>, folder_priority: bool) -> Prefixes {
        let mut p = Prefixes { folder_priority, ..Prefixes::default() };
        for style in self.styles.iter().filter(|s| !s.prefixes_disabled) {
            let folder = style.folder.as_deref();
            for prefix in &style.prefixes {
                let entry = (style.id.clone(), prefix.clone());
                p.all.push(entry.clone());
                if folder.is_some() {
                    p.foldered.push(entry.clone());
                } else {
                    p.unsorted.push(entry.clone());
                }
                if folder == current_folder {
                    p.in_current_folder.push(entry);
                }
            }
        }
        p
    }

    /// The style a line's prefix picked, or the default style, or the first one: TypeR's
    /// `currentStyle` fallback chain.
    pub fn current_style(&self, line_style: Option<&str>) -> Option<&Style> {
        line_style
            .and_then(|id| self.style(id))
            .or_else(|| self.default_style_id.as_deref().and_then(|id| self.style(id)))
            .or_else(|| self.styles.first())
    }

    /// A new id no style or folder has: `s1`, `s2`, … TypeR makes a random id; a sequence is the
    /// same thing without a source of randomness, and it is easier to read in an export.
    pub fn new_style_id(&self) -> String {
        let mut n = self.styles.len() + 1;
        loop {
            let id = format!("s{n}");
            if self.style(&id).is_none() {
                return id;
            }
            n += 1;
        }
    }

    pub fn new_folder_id(&self) -> String {
        let mut n = self.folders.len() + 1;
        loop {
            let id = format!("f{n}");
            if self.folder(&id).is_none() {
                return id;
            }
            n += 1;
        }
    }

    /// The folder and every folder inside it — TypeR's `deleteFolder` takes the descendants with it.
    pub fn descendants(&self, id: &str) -> Vec<String> {
        let mut out = vec![id.to_string()];
        let mut seen = vec![id.to_string()];
        let mut i = 0;
        while i < out.len() && out.len() <= MAX_FOLDER_DEPTH.saturating_mul(MAX_FOLDER_DEPTH) {
            let parent = out[i].clone();
            i += 1;
            for child in self.child_folders(Some(&parent)) {
                if !seen.contains(&child.id) {
                    seen.push(child.id.clone());
                    out.push(child.id.clone());
                }
            }
        }
        out
    }

    /// Removes a folder and its subfolders. Its styles — and theirs — move to the unsorted group
    /// rather than going with it, which is TypeR's `deleteFolder` without `permanent`.
    pub fn remove_folder(&mut self, id: &str) {
        let gone = self.descendants(id);
        self.folders.retain(|f| !gone.contains(&f.id));
        for s in &mut self.styles {
            if s.folder.as_deref().is_some_and(|f| gone.iter().any(|g| g == f)) {
                s.folder = None;
            }
        }
        self.resort();
    }

    /// Removes a folder and its subfolders **and the styles in them**: TypeR's `deleteFolder` with
    /// `permanent`, which is the one action that deletes styles.
    #[cfg(test)]
    pub fn remove_folder_permanently(&mut self, id: &str) {
        let gone = self.descendants(id);
        self.folders.retain(|f| !gone.contains(&f.id));
        self.styles.retain(|s| !s.folder.as_deref().is_some_and(|f| gone.iter().any(|g| g == f)));
        self.resort();
    }

    /// Moves a folder one place among its siblings. TypeR reorders folders by dragging; the host has
    /// no drag widget, so the Folders page has two buttons and this is what they do. A folder at
    /// either end stays where it is.
    pub fn move_folder(&mut self, id: &str, up: bool) {
        let Some(folder) = self.folder(id) else { return };
        let parent = folder.parent_id.clone();
        let siblings: Vec<String> = self.child_folders(parent.as_deref()).iter().map(|f| f.id.clone()).collect();
        let Some(at) = siblings.iter().position(|s| s == id) else { return };
        let to = if up { at.checked_sub(1) } else { siblings.get(at + 1).map(|_| at + 1) };
        let Some(to) = to else { return };
        // Both orders are swapped rather than one changed: `normalize_folders` renumbers the group
        // from the values it finds, and a duplicate would make the result depend on array order.
        let (a, b) = (siblings[at].clone(), siblings[to].clone());
        let (oa, ob) = (self.folder(&a).map(|f| f.order), self.folder(&b).map(|f| f.order));
        if let (Some(oa), Some(ob)) = (oa, ob) {
            if let Some(f) = self.folders.iter_mut().find(|f| f.id == a) {
                f.order = ob;
            }
            if let Some(f) = self.folders.iter_mut().find(|f| f.id == b) {
                f.order = oa;
            }
        }
        self.normalize_folders();
    }

    /// Whether moving `id` inside `parent` would make a loop: the parent picker hides the folder and
    /// its descendants for this reason (`editFolder.jsx:147-151`).
    pub fn can_move_into(&self, id: &str, parent: Option<&str>) -> bool {
        match parent {
            None => true,
            Some(p) => p != id && !self.descendants(id).iter().any(|d| d == p),
        }
    }

    /// The library as this port's storage keeps it. Not TypeR's export shape — see
    /// [`export_json`] for that.
    pub fn to_json(&self) -> Value {
        json!({
            "ignoreLinePrefixes": self.ignore_prefixes,
            "ignoreTags": self.ignore_tags,
            "defaultStyleId": self.default_style_id,
            "textItemKind": if self.point_text { "point" } else { "box" },
            "currentFolderTagPriority": self.folder_priority,
            "folders": self.folders.iter().map(|f| json!({
                "id": f.id,
                "name": f.name,
                "parentId": f.parent_id,
                "order": f.order,
            })).collect::<Vec<_>>(),
            "styles": self.styles.iter().map(Style::to_json).collect::<Vec<_>>(),
            "version": FORMAT_VERSION,
        })
    }

    /// A library from this port's storage, a TypeR export, or a TyperTools export. Never fails: a
    /// field that is missing or the wrong shape falls back to the default, because the file comes
    /// from outside and half a library is better than none — but a style without an id is dropped,
    /// since nothing could refer to it.
    pub fn from_json(v: &Value) -> Library {
        let mut lib = Library::default();
        if let Some(p) = v.get("ignoreLinePrefixes") {
            lib.ignore_prefixes = strings(Some(p));
        }
        if let Some(t) = v.get("ignoreTags") {
            lib.ignore_tags = strings(Some(t));
        }
        lib.default_style_id = v.get("defaultStyleId").and_then(Value::as_str).map(str::to_string);
        // TypeR defaults this on, and only a literal `false` turns it off (`!== false`).
        lib.folder_priority = v.get("currentFolderTagPriority").and_then(Value::as_bool).unwrap_or(true);
        // Either name, whichever the file carries (see the module note).
        lib.point_text = match v.get("textItemKind").and_then(Value::as_str) {
            Some(kind) => kind.eq_ignore_ascii_case("point"),
            None => v.get("pastePointText").and_then(Value::as_bool).unwrap_or(false),
        };
        // TypeR's single-folder export: the file carries no ids, so new ones are made here, as its
        // importer does (`settings.jsx:335-352`).
        if let Some(styles) = v.get("exportedStyles").and_then(Value::as_array) {
            let mut folder = Folder::new(lib.new_folder_id(), v.get("name").and_then(Value::as_str).unwrap_or("Folder"));
            folder.order = u32::try_from(lib.folders.len()).unwrap_or(u32::MAX);
            let folder_id = folder.id.clone();
            lib.folders.push(folder);
            for s in styles {
                let id = lib.new_style_id();
                let mut style = Style::from_json_with_id(s, id);
                style.folder = Some(folder_id.clone());
                lib.styles.push(style);
            }
            lib.resort();
            return lib;
        }
        if let Some(folders) = v.get("folders").and_then(Value::as_array) {
            lib.folders = folders
                .iter()
                .filter_map(|f| {
                    let id = f.get("id").and_then(Value::as_str)?;
                    Some(Folder {
                        id: id.to_string(),
                        name: f.get("name").and_then(Value::as_str).unwrap_or("Folder").to_string(),
                        parent_id: f
                            .get("parentId")
                            .and_then(Value::as_str)
                            .filter(|p| !p.is_empty())
                            .map(str::to_string),
                        order: f
                            .get("order")
                            .and_then(Value::as_f64)
                            .filter(|n| n.is_finite() && *n >= 0.0)
                            .map(|n| u32::try_from(n as u64).unwrap_or(u32::MAX))
                            .unwrap_or(0),
                    })
                })
                .collect();
        }
        if let Some(styles) = v.get("styles").and_then(Value::as_array) {
            let mut seen: Vec<String> = Vec::new();
            for s in styles {
                if let Some(style) = Style::from_json(s)
                    && !seen.contains(&style.id)
                {
                    seen.push(style.id.clone());
                    lib.styles.push(style);
                }
            }
        }
        lib.resort();
        // A default style that is not there would silently letter with the first style instead.
        if let Some(id) = lib.default_style_id.clone()
            && lib.style(&id).is_none()
        {
            lib.default_style_id = None;
        }
        lib
    }

    /// Merges an imported library into this one the way TypeR's `import` reducer does: styles this
    /// library has edited more recently are kept, and everything else comes from the file. Settings
    /// are always taken from the file. Returns how many of this library's own styles were kept.
    pub fn merge(&mut self, imported: &Library) -> usize {
        let mut kept = 0;
        for mine in &self.styles {
            let theirs = imported.style(&mine.id);
            let keep = match theirs {
                None => true,
                Some(t) => mine.edited > 0 && mine.edited > t.edited,
            };
            if keep {
                kept += 1;
            }
        }
        let mut styles: Vec<Style> = Vec::new();
        for mine in &self.styles {
            match imported.style(&mine.id) {
                None => styles.push(mine.clone()),
                Some(t) => {
                    if mine.edited > 0 && mine.edited > t.edited {
                        styles.push(mine.clone());
                    } else {
                        styles.push(t.clone());
                    }
                }
            }
        }
        for t in &imported.styles {
            if !styles.iter().any(|s| s.id == t.id) {
                styles.push(t.clone());
            }
        }
        self.styles = styles;
        self.folders = imported.folders.clone();
        self.ignore_prefixes = imported.ignore_prefixes.clone();
        self.ignore_tags = imported.ignore_tags.clone();
        self.default_style_id = imported.default_style_id.clone();
        self.point_text = imported.point_text;
        self.folder_priority = imported.folder_priority;
        self.resort();
        kept
    }
}

/// TypeR's full export (`export.jsx:82-112`): the selected folders and **only the styles directly
/// inside them** — an unsorted style is never exported by this dialog — plus the settings when the
/// letterer asks for them.
///
/// Two fields differ from TypeR's own file, for reasons the sandbox forces:
///
/// * `exported` is omitted. TypeR writes `new Date()`, which serialises to an ISO 8601 string; the
///   plug-in has no clock, only the monotonic `now` of each event, and a made-up timestamp would be
///   worse than none. Nothing reads the field.
/// * `textItemKind` is written from the real setting. TypeR tries to write it from a state field
///   that does not exist, so its own exports omit it; a reader that understands it (this port, and
///   TyperTools) gets the right value.
/// * TypeR's `language`, `autoClosePSD` and `autoScrollStyle` are not written: this panel has no
///   equivalent settings yet, and inventing values for them would claim otherwise.
///
/// A style's `stroke` is not carried yet — the port has no stroke editor, so the styles it writes
/// have none. That arrives with the stroke feature rather than being faked here.
pub fn export_json(lib: &Library, folder_ids: &[String], with_settings: bool) -> Value {
    let folders: Vec<Value> = lib
        .folders
        .iter()
        .filter(|f| folder_ids.contains(&f.id))
        .map(|f| json!({"id": f.id, "name": f.name, "parentId": f.parent_id, "order": f.order}))
        .collect();
    let styles: Vec<Value> = lib
        .styles
        .iter()
        .filter(|s| s.folder.as_deref().is_some_and(|f| folder_ids.iter().any(|id| id == f)))
        .map(Style::to_json)
        .collect();
    let mut v = json!({
        "folders": folders,
        "styles": styles,
        "version": FORMAT_VERSION,
    });
    if with_settings {
        v["ignoreLinePrefixes"] = json!(lib.ignore_prefixes);
        v["ignoreTags"] = json!(lib.ignore_tags);
        v["defaultStyleId"] = json!(lib.default_style_id);
        v["currentFolderTagPriority"] = json!(lib.folder_priority);
        v["textItemKind"] = json!(if lib.point_text { "point" } else { "box" });
    }
    v
}

/// TypeR's single-folder export (`stylesBlock.jsx:104-125`): the folder's name and the styles
/// directly in it, with no ids, edit dates or disabled flags. Subfolders are not included.
pub fn export_folder_json(lib: &Library, folder_id: &str) -> Value {
    let name = lib.folder(folder_id).map(|f| f.name.clone()).unwrap_or_default();
    let styles: Vec<Value> = lib
        .styles_in(Some(folder_id))
        .iter()
        .map(|s| {
            json!({
                "name": s.name,
                "textProps": s.text_props,
                "prefixes": s.prefixes,
                "prefixColor": s.prefix_color,
            })
        })
        .collect();
    json!({ "name": name, "exportedStyles": styles })
}

/// The layer-level keys of a style that are not text properties: TypeR strips exactly these when a
/// style is copied off a layer (`editStyle.jsx` › `copyLayerStyle`), and they mean nothing in a
/// library.
pub const LAYER_KEYS: [&str; 7] = ["textKey", "textShape", "textClickPoint", "transform", "boundingBox", "bounds", "warp"];

/// Drops the layer-level keys from a `layerText` object, in place. The style editor calls this after
/// copying a style off the active layer, as TypeR does.
pub fn strip_layer_keys(layer_text: &mut Map<String, Value>) {
    for k in LAYER_KEYS {
        layer_text.remove(k);
    }
}

/// One line of the starter table below: id, name, prefixes, folder, size in points, face, caps,
/// alignment, colour.
type Recipe = (&'static str, &'static str, &'static str, &'static str, f64, &'static str, &'static str, &'static str, &'static str);

/// The library the panel starts from when there is none: the five kinds of line a letterer reaches
/// for on a first page, in two folders, with the default style and TypeR's `##` ignore prefix.
///
/// This is **this port's own**, not a copy of TypeR's shipped library — a letterer's real library is
/// their own file anyway (Help ▸ Load). The point is that the panel is usable the minute it opens,
/// and that a prefix is what selects a style.
///
/// `font` is the first family the host has, so the starter styles letter in something that is
/// actually installed; with `None` they keep TypeR's own default, Tahoma.
///
/// The sizes below are **points**, and the styles say so (`typeUnit`), which is the one place this
/// library departs from TypeR's `getDefaultStyle()`. Its own style is 14 *pixels*, and the letterer
/// scales it with the Text Size field; a starter library is meant to be usable as it opens, and
/// 14 px is 3.4 pt on the 300 dpi page a comic is drawn at — a tenth of the size the balloon needs.
/// Points mean the same thing on every document, which is what a starting point should do.
pub fn starter(font: Option<&str>) -> Library {
    let mut lib = Library {
        folders: vec![Folder::new("f1", "Dialogue"), Folder::new("f2", "Sound effects")],
        ..Library::default()
    };

    // id, name, prefixes, folder, size in points, face, caps, alignment, colour
    let recipes: [Recipe; 5] = [
        ("s1", "Dialogue", "d.\nD.", "f1", 14.0, "Regular", "normal", "center", "#000000"),
        ("s2", "Whisper", "w.\nW.", "f1", 10.0, "Italic", "normal", "center", "#000000"),
        ("s3", "Shout", "s.\nS.", "f1", 22.0, "Bold", "allCaps", "center", "#000000"),
        ("s4", "Thought", "t.\nT.", "f1", 12.0, "Italic", "normal", "center", "#000000"),
        ("s5", "Sound effect", "x.\nX.", "f2", 30.0, "Bold", "allCaps", "center", "#ffffff"),
    ];
    for (i, (id, name, prefixes, folder, size, face, caps, align, colour)) in recipes.iter().enumerate() {
        let mut s = Style::new(*id, *name);
        s.text_props["typeUnit"] = json!("pointsUnit");
        s.folder = Some((*folder).to_string());
        s.set_prefixes_text(prefixes);
        s.prefix_color = PREFIX_COLORS[i % PREFIX_COLORS.len()].to_string();
        if let Some(f) = font {
            props::set_font(&mut s.text_props, f, face, None);
        } else {
            // A starter style is laid down before the host has listed its fonts, so it names no
            // family yet — but the face is this style type's own (Whisper is italic, Shout bold) and
            // must survive until `starter_font` gives it a family. Without this, the fallback face
            // below applied to every style and the five starter styles lettered alike.
            props::set_char(&mut s.text_props, "fontStyleName", json!(face));
        }
        props::set_char(&mut s.text_props, "size", json!(size));
        props::set_char(&mut s.text_props, "impliedFontSize", json!(size));
        props::set_char(&mut s.text_props, "fontCaps", json!(caps));
        props::set_char(&mut s.text_props, "color", props::hex_color(colour));
        props::set_para(&mut s.text_props, "alignment", json!(align));
        s.edited = 1;
        lib.styles.push(s);
    }
    lib.default_style_id = Some("s1".into());
    lib.point_text = false;
    lib.resort();
    lib
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::script;

    fn style(id: &str, name: &str, prefix: &str) -> Style {
        let mut s = Style::new(id, name);
        s.set_prefixes_text(prefix);
        s
    }

    #[test]
    fn the_order_styles_match_in_is_unsorted_first_then_folders() {
        let mut lib = Library { folders: vec![Folder::new("f1", "Shouts")], ..Library::default() };
        let mut a = style("s1", "A", "a:");
        a.folder = Some("f1".into());
        lib.styles = vec![a, style("s2", "B", "b:"), style("s3", "C", "c:")];
        lib.resort();
        assert_eq!(
            lib.styles.iter().map(|s| s.id.as_str()).collect::<Vec<_>>(),
            ["s2", "s3", "s1"],
            "the unsorted ones first, in their own order, then the folder's"
        );
        assert_eq!(lib.prefix_order(), [("s2".into(), "b:".into()), ("s3".into(), "c:".into()), ("s1".into(), "a:".into())]);
        assert_eq!(lib.styles_in(None).len(), 2);
        assert_eq!(lib.styles_in(Some("f1")).len(), 1);
    }

    #[test]
    fn nested_folders_put_each_folder_s_styles_before_its_children() {
        let mut lib = Library {
            folders: vec![
                Folder { id: "f2".into(), name: "Child".into(), parent_id: Some("f1".into()), order: 0 },
                Folder { id: "f1".into(), name: "Root".into(), parent_id: None, order: 0 },
                Folder { id: "f0".into(), name: "Root two".into(), parent_id: None, order: 1 },
            ],
            ..Library::default()
        };
        let mut root = style("s1", "Root style", "r:");
        root.folder = Some("f1".into());
        let mut child = style("s2", "Child style", "c:");
        child.folder = Some("f2".into());
        let mut second = style("s3", "Second root", "q:");
        second.folder = Some("f0".into());
        lib.styles = vec![second, child, root, style("s4", "Unsorted", "u:")];
        lib.resort();
        assert_eq!(lib.styles.iter().map(|s| s.id.as_str()).collect::<Vec<_>>(), ["s4", "s1", "s2", "s3"]);
    }

    #[test]
    fn folder_orders_are_renumbered_and_the_arrays_are_left_alone() {
        let mut lib = Library {
            folders: vec![
                Folder { id: "b".into(), name: "B".into(), parent_id: None, order: 9 },
                Folder { id: "a".into(), name: "A".into(), parent_id: None, order: 3 },
                Folder { id: "c".into(), name: "C".into(), parent_id: None, order: 7 },
            ],
            ..Library::default()
        };
        lib.normalize_folders();
        // State order is the file's order; display order is by the renumbered value.
        assert_eq!(lib.folders.iter().map(|f| f.id.as_str()).collect::<Vec<_>>(), ["b", "a", "c"]);
        assert_eq!(lib.folders.iter().map(|f| f.order).collect::<Vec<_>>(), [2, 0, 1]);
        assert_eq!(lib.folder_order(), ["a", "c", "b"]);
    }

    #[test]
    fn a_dangling_or_self_parent_becomes_a_root() {
        let mut lib = Library {
            folders: vec![
                Folder { id: "a".into(), name: "A".into(), parent_id: Some("a".into()), order: 0 },
                Folder { id: "b".into(), name: "B".into(), parent_id: Some("gone".into()), order: 0 },
            ],
            ..Library::default()
        };
        lib.normalize_folders();
        assert!(lib.folders.iter().all(|f| f.parent_id.is_none()));
        assert_eq!(lib.folder_order(), ["a", "b"]);
        assert_eq!(lib.folder_depth("a"), 0);
    }

    #[test]
    fn folder_depth_is_bounded_even_when_the_file_has_a_loop() {
        // A hand-written file can make A inside B and B inside A; normalizeFolders does not catch
        // it, and the display walk must still terminate.
        let mut lib = Library {
            folders: vec![
                Folder { id: "a".into(), name: "A".into(), parent_id: Some("b".into()), order: 0 },
                Folder { id: "b".into(), name: "B".into(), parent_id: Some("a".into()), order: 0 },
            ],
            ..Library::default()
        };
        lib.normalize_folders();
        let _ = lib.folder_order();
        assert!(lib.folder_depth("a") <= MAX_FOLDER_DEPTH);
        assert!(lib.folder_depth("b") <= MAX_FOLDER_DEPTH);
    }

    #[test]
    fn a_style_inside_a_folder_loop_is_not_lost() {
        // TypeR's flattening starts at the roots, so these two folders — which are each other's
        // parent — are never reached and their styles never make it into the matching list. Here
        // they are appended instead.
        let mut lib = Library {
            folders: vec![
                Folder { id: "a".into(), name: "A".into(), parent_id: Some("b".into()), order: 0 },
                Folder { id: "b".into(), name: "B".into(), parent_id: Some("a".into()), order: 0 },
            ],
            ..Library::default()
        };
        let mut s = style("s1", "Loop", "p:");
        s.folder = Some("a".into());
        lib.styles = vec![s];
        lib.resort();
        assert_eq!(lib.styles.len(), 1, "the style is still matchable");
        assert_eq!(lib.prefix_order(), [("s1".into(), "p:".into())]);
    }

    #[test]
    fn a_style_whose_folder_is_gone_falls_back_to_the_unsorted_group() {
        let mut lib = Library::default();
        let mut a = style("s1", "A", "a:");
        a.folder = Some("gone".into());
        lib.styles = vec![a];
        lib.resort();
        assert_eq!(lib.styles[0].folder, None, "an unreachable style would never match a line");
    }

    #[test]
    fn removing_a_folder_takes_its_subfolders_and_keeps_their_styles() {
        let mut lib = Library {
            folders: vec![
                Folder { id: "f1".into(), name: "Root".into(), parent_id: None, order: 0 },
                Folder { id: "f2".into(), name: "Child".into(), parent_id: Some("f1".into()), order: 0 },
                Folder { id: "f3".into(), name: "Elsewhere".into(), parent_id: None, order: 1 },
            ],
            ..Library::default()
        };
        let mut a = style("s1", "A", "a:");
        a.folder = Some("f2".into());
        let mut b = style("s2", "B", "b:");
        b.folder = Some("f3".into());
        lib.styles = vec![a, b];
        lib.remove_folder("f1");
        assert_eq!(lib.folders.iter().map(|f| f.id.as_str()).collect::<Vec<_>>(), ["f3"]);
        assert_eq!(lib.styles.len(), 2, "deleting a folder does not delete its styles");
        assert_eq!(lib.style("s1").map(|s| s.folder.clone()), Some(None));
        assert_eq!(lib.style("s2").map(|s| s.folder.clone()), Some(Some("f3".into())));
    }

    #[test]
    fn a_permanent_delete_takes_the_styles_with_it() {
        let mut lib = Library {
            folders: vec![
                Folder { id: "f1".into(), name: "Root".into(), parent_id: None, order: 0 },
                Folder { id: "f2".into(), name: "Child".into(), parent_id: Some("f1".into()), order: 0 },
            ],
            ..Library::default()
        };
        let mut a = style("s1", "A", "a:");
        a.folder = Some("f2".into());
        lib.styles = vec![a, style("s2", "B", "b:")];
        lib.remove_folder_permanently("f1");
        assert!(lib.folders.is_empty());
        assert_eq!(lib.styles.len(), 1);
        assert_eq!(lib.styles[0].id, "s2");
    }

    #[test]
    fn folders_move_one_place_among_their_siblings() {
        let mut lib = Library {
            folders: vec![
                Folder { id: "a".into(), name: "A".into(), parent_id: None, order: 0 },
                Folder { id: "b".into(), name: "B".into(), parent_id: None, order: 1 },
                Folder { id: "c".into(), name: "C".into(), parent_id: Some("b".into()), order: 0 },
            ],
            ..Library::default()
        };
        lib.move_folder("b", true);
        assert_eq!(lib.folder_order(), ["b", "c", "a"], "b and a swapped; c came with b");
        lib.move_folder("a", true);
        assert_eq!(lib.folder_order(), ["a", "b", "c"], "a moved back up");
        lib.move_folder("a", true);
        assert_eq!(lib.folder_order(), ["a", "b", "c"], "the first folder stays first");
        lib.move_folder("b", false);
        assert_eq!(lib.folder_order(), ["a", "b", "c"], "a root folder's child does not join this group");
    }

    #[test]
    fn a_folder_cannot_be_moved_inside_itself_or_its_children() {
        let mut lib = Library {
            folders: vec![
                Folder { id: "f1".into(), name: "Root".into(), parent_id: None, order: 0 },
                Folder { id: "f2".into(), name: "Child".into(), parent_id: Some("f1".into()), order: 0 },
                Folder { id: "f3".into(), name: "Other".into(), parent_id: None, order: 1 },
            ],
            ..Library::default()
        };
        assert!(!lib.can_move_into("f1", Some("f1")));
        assert!(!lib.can_move_into("f1", Some("f2")), "moving a folder into its own child makes a loop");
        assert!(lib.can_move_into("f2", Some("f3")));
        assert!(lib.can_move_into("f1", None));
        lib.normalize_folders();
        assert!(lib.can_move_into("f2", None));
    }

    #[test]
    fn prefix_lists_carry_type_r_s_folder_priority() {
        let mut lib = Library {
            folders: vec![
                Folder { id: "f1".into(), name: "Balloons".into(), parent_id: None, order: 0 },
                Folder { id: "f2".into(), name: "Effects".into(), parent_id: None, order: 1 },
            ],
            ..Library::default()
        };
        let mut balloons = style("s1", "Balloon", "#\nb:");
        balloons.folder = Some("f1".into());
        let mut effects = style("s2", "Effect", "#\ne:");
        effects.folder = Some("f2".into());
        let mut unsorted = style("s3", "Unsorted", "#\nu:");
        unsorted.prefixes_disabled = false;
        let mut off = style("s4", "Off", "z:");
        off.prefixes_disabled = true;
        lib.styles = vec![balloons, effects, unsorted, off];
        lib.resort();

        let p = lib.prefixes(Some("f1"), true);
        assert_eq!(p.all.len(), 6, "two prefixes each, the disabled style's none");
        assert_eq!(p.in_current_folder, [("s1".into(), "#".into()), ("s1".into(), "b:".into())]);
        assert_eq!(p.foldered.len(), 4);
        assert_eq!(p.unsorted.len(), 2);
        assert!(p.folder_priority);
        assert!(!p.all.iter().any(|(id, _)| id == "s4"), "a disabled style claims nothing");

        // With no style in use the current folder is `None`, and the unsorted prefixes are the ones
        // with priority — TypeR compares `folder === currentFolder`, and `null === null` is true.
        let p = lib.prefixes(None, true);
        assert_eq!(p.in_current_folder, [("s3".into(), "#".into()), ("s3".into(), "u:".into())]);
        // And priority off is just a flag for the parser; the lists are the same.
        let q = lib.prefixes(Some("f1"), false);
        assert_eq!(q.all, p.all);
        assert!(!q.folder_priority);
    }

    #[test]
    fn prefixes_are_split_on_lines_and_semicolons() {
        let mut s = Style::new("s1", "A");
        s.set_prefixes_text(" d: \n; D: ;;\n\nx:");
        assert_eq!(s.prefixes, ["d:", "D:", "x:"]);
        assert_eq!(s.prefixes_text(), "d:\nD:\nx:");
        // The string form of an imported list follows the same rule.
        let lib = Library::from_json(&json!({"styles": [{"id": "s1", "prefixes": "a:;b:\nc:"}]}));
        assert_eq!(lib.style("s1").map(|s| s.prefixes.clone()), Some(vec!["a:".into(), "b:".into(), "c:".into()]));
    }

    #[test]
    fn an_export_comes_back_the_same() {
        let mut lib = Library {
            folders: vec![
                Folder { id: "f1".into(), name: "Shouts".into(), parent_id: None, order: 0 },
                Folder { id: "f2".into(), name: "Nested".into(), parent_id: Some("f1".into()), order: 0 },
            ],
            ..Library::default()
        };
        lib.ignore_prefixes = vec!["##".into(), "//".into()];
        lib.ignore_tags = vec!["{b}".into()];
        lib.default_style_id = Some("s1".into());
        lib.point_text = true;
        lib.folder_priority = false;
        let mut a = style("s1", "Dialogue", "d: D:");
        a.folder = Some("f1".into());
        a.prefix_color = PREFIX_COLORS[3].into();
        a.edited = 1_700_000_000_000;
        a.prefixes_disabled = true;
        // A property the port knows nothing about survives the round trip: that is the whole point
        // of keeping `textProps` verbatim.
        a.text_props["layerText"]["textStyleRange"][0]["textStyle"]["fontTechnology"] = json!(1);
        lib.styles = vec![a];
        let json = lib.to_json();
        assert_eq!(json["textItemKind"], "point");
        assert_eq!(json["version"], FORMAT_VERSION);
        assert_eq!(json["folders"][1]["parentId"], "f1");
        assert_eq!(json["styles"][0]["prefixesDisabled"], true);
        let back = Library::from_json(&json);
        assert_eq!(back, lib, "an export re-imports as the library it was");
        assert_eq!(back.styles[0].text_props["layerText"]["textStyleRange"][0]["textStyle"]["fontTechnology"], 1);
    }

    #[test]
    fn a_hostile_file_imports_as_far_as_it_can() {
        // Every field the wrong shape, a style with no id, a folder with no name, a duplicate id.
        let v = json!({
            "ignoreLinePrefixes": "## //",
            "ignoreTags": [1, "##"],
            "defaultStyleId": 7,
            "textItemKind": "sideways",
            "currentFolderTagPriority": "yes",
            "folders": [{"name": "no id"}, {"id": "f1"}, {"id": "f1", "name": "twice"}],
            "styles": [{"name": "no id"}, {"id": "s1", "prefixes": 3, "textProps": "not an object"}, {"id": "s1", "name": "twice"}],
        });
        let lib = Library::from_json(&v);
        assert_eq!(lib.ignore_prefixes, ["## //"], "a string list is split on lines, not spaces");
        assert_eq!(lib.ignore_tags, ["##"]);
        assert_eq!(lib.default_style_id, None);
        assert!(!lib.point_text);
        assert!(lib.folder_priority, "a value that is not `false` leaves priority on");
        assert_eq!(lib.folders.len(), 1, "a folder with no id is dropped, a duplicate id once");
        assert_eq!(lib.styles.len(), 1, "a style with no id is dropped, a duplicate id once");
        // The *first* entry for an id is the one that counts: a second one later in the file cannot
        // shadow the style everything above it already refers to.
        assert_eq!(lib.styles[0].name, "Style", "the first of the two `s1`s, with a name it never had");
        assert!(lib.styles[0].prefixes.is_empty());
        assert!(lib.styles[0].text_props.is_object(), "bad properties become the defaults");
        // And the whole thing is still a library that can be exported: nothing here panics.
        assert!(Library::from_json(&json!({"styles": null, "folders": 5})).styles.is_empty());
        assert!(Library::from_json(&Value::Null).styles.is_empty());
    }

    #[test]
    fn a_merge_keeps_the_styles_edited_here_and_takes_the_rest() {
        let mut mine = Library::default();
        let mut local = style("s1", "Local", "d:");
        local.edited = 2_000;
        mine.styles = vec![local, style("s2", "Only here", "x:")];
        let mut theirs = Library::default();
        let mut theirs_s1 = style("s1", "Theirs", "d:");
        theirs_s1.edited = 1_000;
        theirs.ignore_tags = vec!["{i}".into()];
        theirs.folder_priority = false;
        theirs.styles = vec![theirs_s1, style("s3", "Only there", "y:")];
        let kept = mine.merge(&theirs);
        assert_eq!(kept, 2, "s1 was edited here later, s2 is not in the file");
        assert_eq!(mine.styles.len(), 3);
        assert_eq!(mine.style("s1").map(|s| s.name.as_str()), Some("Local"));
        assert_eq!(mine.style("s3").map(|s| s.name.as_str()), Some("Only there"));
        assert_eq!(mine.ignore_tags, ["{i}"]);
        assert!(!mine.folder_priority);
    }

    #[test]
    fn ids_are_sequence_numbers_that_are_not_taken() {
        let mut lib = Library { styles: vec![style("s1", "A", "a:"), style("s2", "B", "b:")], ..Library::default() };
        assert_eq!(lib.new_style_id(), "s3");
        lib.folders = vec![Folder::new("f1", "F")];
        assert_eq!(lib.new_folder_id(), "f2");
        lib.folders.push(Folder::new("f2", "G"));
        assert_eq!(lib.new_folder_id(), "f3");
    }

    #[test]
    fn the_current_style_falls_back_the_way_type_r_does() {
        let mut lib = Library::default();
        let mut a = style("s1", "A", "a:");
        a.edited = 1;
        lib.styles = vec![a, style("s2", "B", "b:")];
        lib.default_style_id = Some("s2".into());
        assert_eq!(lib.current_style(Some("s1")).map(|s| s.id.as_str()), Some("s1"), "the line's own style");
        assert_eq!(lib.current_style(None).map(|s| s.id.as_str()), Some("s2"), "else the default");
        lib.default_style_id = None;
        assert_eq!(lib.current_style(Some("gone")).map(|s| s.id.as_str()), Some("s1"), "else the first style");
        lib.styles.clear();
        assert!(lib.current_style(None).is_none(), "and nothing at all in an empty library");
    }

    #[test]
    fn the_full_export_takes_the_chosen_folders_and_the_styles_directly_in_them() {
        let mut lib = Library {
            folders: vec![
                Folder { id: "f1".into(), name: "One".into(), parent_id: None, order: 0 },
                Folder { id: "f2".into(), name: "Two".into(), parent_id: None, order: 1 },
            ],
            ..Library::default()
        };
        let mut in_one = style("s1", "In one", "a:");
        in_one.folder = Some("f1".into());
        let mut in_two = style("s2", "In two", "b:");
        in_two.folder = Some("f2".into());
        lib.styles = vec![in_one, in_two, style("s3", "Unsorted", "c:")];
        lib.resort();

        let v = export_json(&lib, &["f1".into()], false);
        assert_eq!(v["folders"].as_array().map(Vec::len), Some(1));
        assert_eq!(v["styles"].as_array().map(Vec::len), Some(1), "only styles directly in the chosen folder");
        assert_eq!(v["styles"][0]["id"], "s1");
        assert_eq!(v["version"], FORMAT_VERSION);
        assert!(v.get("ignoreLinePrefixes").is_none(), "settings only when asked for");
        assert!(v.get("exported").is_none(), "the panel has no clock");

        let v = export_json(&lib, &["f1".into(), "f2".into()], true);
        assert_eq!(v["ignoreLinePrefixes"], json!(["##"]));
        assert_eq!(v["ignoreTags"], json!([]));
        assert_eq!(v["currentFolderTagPriority"], true);
        assert_eq!(v["textItemKind"], "box");
        assert_eq!(v["styles"].as_array().map(Vec::len), Some(2), "still not the unsorted style");
    }

    #[test]
    fn a_single_folder_export_is_just_the_name_and_its_styles() {
        let mut lib = Library {
            folders: vec![Folder { id: "f1".into(), name: "Shouts".into(), parent_id: None, order: 0 }],
            ..Library::default()
        };
        let mut a = style("s1", "Bold", "s:");
        a.folder = Some("f1".into());
        a.prefixes_disabled = true;
        lib.styles = vec![a];
        let v = export_folder_json(&lib, "f1");
        assert_eq!(v["name"], "Shouts");
        let styles = v["exportedStyles"].as_array().expect("an array");
        assert_eq!(styles.len(), 1);
        assert_eq!(styles[0]["name"], "Bold");
        for absent in ["id", "folder", "edited", "prefixesDisabled"] {
            assert!(styles[0].get(absent).is_none(), "TypeR's folder export carries no {absent}");
        }
        // And it imports as a new folder, as TypeR's importer does.
        let back = Library::from_json(&v);
        assert_eq!(back.folders.len(), 1);
        assert_eq!(back.styles.len(), 1);
        assert_eq!(back.styles[0].folder.as_deref(), Some(back.folders[0].id.as_str()));
    }

    #[test]
    fn the_starter_library_is_usable_and_names_the_font_it_was_given() {
        let lib = starter(Some("Inter"));
        assert_eq!(lib.styles.len(), 5);
        assert_eq!(lib.folders.len(), 2);
        assert_eq!(lib.ignore_prefixes, vec!["##".to_string()]);
        assert_eq!(lib.default_style_id.as_deref(), Some("s1"));
        assert!(lib.styles.iter().all(|s| s.folder.is_some()), "every starter style is in a folder");
        assert!(lib.styles.iter().all(|s| props::family(&s.text_props) == "Inter"));
        assert!(lib.styles.iter().all(|s| !s.prefixes.is_empty()));
        // Every prefix matches a line, and the style it matches is the one that carries it.
        let prefixes = lib.prefixes(None, true);
        let lines = script::parse(
            "d. hello\nx. BANG\nplain\n## a note",
            &script::Rules { ignore_prefixes: &lib.ignore_prefixes, ignore_tags: &lib.ignore_tags, prefixes: &prefixes },
        );
        assert_eq!(lines[0].style_id.as_deref(), Some("s1"));
        assert_eq!(lines[1].style_id.as_deref(), Some("s5"));
        assert_eq!(lines[2].style_id, None);
        assert!(lines[3].ignored);
        // With no font to offer, a starter style keeps TypeR's own default rather than nothing.
        let lib = starter(None);
        assert!(lib.styles.iter().all(|s| props::family(&s.text_props) == props::DEFAULT_FONT));
        // The face is each style type's own even before a family is known: Whisper is italic and
        // Shout is bold in TypeR's library, and the placeholder family must not flatten them.
        let faces: Vec<String> = lib.styles.iter().map(|s| props::font_style(&s.text_props)).collect();
        assert_eq!(faces, ["Regular", "Italic", "Bold", "Italic", "Bold"]);
        // The whole thing survives a round trip through the library format.
        let back = Library::from_json(&lib.to_json());
        assert_eq!(back, lib);
    }
}
