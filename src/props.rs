//! Text properties: TyperTools' `textProps` (Adobe's "jam" `layerText` shape) in and out of
//! PhotoCraft's type commands.
//!
//! A style's properties are kept **verbatim in TyperTools' own shape**, so a library written in
//! Photoshop and one written here are the same file, and a property this port does not understand
//! survives a round trip rather than being dropped. This module is the only place that translates
//! between that shape and the engine's parameters (`type.create`, `type.setStyle`, `type.edit` —
//! see `crates/engine/src/type_cmds.rs`).
//!
//! The shape, from `utils.js`'s `getDefaultStyle()`, is
//!
//! ```json
//! {"layerText": {"textGridding": "none", "orientation": "horizontal", "antiAlias": "antiAliasSmooth",
//!   "textStyleRange": [{"from": 0, "to": 100, "textStyle": {…}}],
//!   "paragraphStyleRange": [{"from": 0, "to": 100, "paragraphStyle": {…}}]},
//!  "typeUnit": "pixelsUnit"}
//! ```
//!
//! Three mappings are worth stating, because they are where the models differ:
//!
//! * **Units.** Photoshop stores a size in the layer's ruler unit (`typeUnit`), PhotoCraft in points.
//!   Sizes, leadings, indents and baseline shifts convert at the document's resolution
//!   ([`crate::geom::length_to_pt`]); `horizontalScale`, `verticalScale`, `tracking` and
//!   `autoLeadingPercentage` are ratios or per-mille in both models and pass through.
//! * **Super/subscript.** Adobe has a `baseline: "superScript"|"subScript"` attribute; PhotoCraft's
//!   `CharStyle` has none. TyperTools' control is therefore emulated the way such text *renders* —
//!   a raised or lowered baseline and a smaller size ([`SCRIPT_SIZE`], [`SCRIPT_SHIFT`]) — so the
//!   control works rather than doing nothing. It will not come back as a PSD superscript attribute,
//!   and [`from_info`] recovers it by the same ratio it was written with. A `CharStyle.script` field
//!   is the host-side fix, and is recorded as a gap.
//! * **Anti-aliasing** is a *layer* property in Adobe's shape (`layerText.antiAlias`) but a parameter
//!   of `type.edit` in PhotoCraft, so applying a style is a `type.setStyle` plus, when the layer's
//!   anti-aliasing differs, a `type.edit`. Adobe's two platform modes ("Windows Gray", "Windows LCD")
//!   have no engine parameter; they map to `sharp`, which is how PhotoCraft renders them
//!   (`AntiAlias::Windows`/`WindowsLcd` "rendered like Sharp").
//!
//! Two Adobe paragraph options have no host support at all and are **not** translated:
//! `hangingRoman` (burasagari) and `burasagari` itself, which need kinsoku line breaking
//! (`crates/text/src/cjk.rs` is font fallback only). The style editor shows them and says so instead
//! of pretending; the values are still stored, so a library round-trips. `hyphenate` *is* translated
//! (the engine stores and round-trips it) but does not yet break lines in `crates/text/src/layout.rs`
//! — the editor notes that too.

use serde_json::{Map, Value, json};

use crate::geom;

/// The family TyperTools writes into a new style. It is a Windows font, so on Linux the panel
/// replaces it with one the host actually has once `type.fonts` has answered
/// ([`default_props_for`]); a library imported from Photoshop still names Tahoma, and the engine
/// reports a missing font rather than silently substituting.
pub const DEFAULT_FONT: &str = "Tahoma";
pub const DEFAULT_FONT_STYLE: &str = "Regular";
/// TyperTools' default size, in `typeUnit` (pixels).
pub const DEFAULT_SIZE: f64 = 14.0;
/// Photoshop's default auto-leading multiple, as TyperTools' default paragraph style carries it.
pub const DEFAULT_AUTO_LEADING: f64 = 1.2;
/// The ruler unit a new style is written in, as TyperTools writes it.
pub const DEFAULT_TYPE_UNIT: &str = "pixelsUnit";

/// Superscript/subscript emulation: the size and the baseline shift as fractions of the size the
/// text would have had. Photoshop's own script metrics are not public; these render the way
/// Photoshop's do, and they are this port's numbers, not a claim about Adobe's.
pub const SCRIPT_SIZE: f64 = 0.583;
pub const SCRIPT_SHIFT: f64 = 0.333;

/// The paragraph keys that exist in Adobe's shape but have no PhotoCraft equivalent (see the module
/// note). The editor refuses to *claim* they work.
pub const UNSUPPORTED_PARAGRAPH_KEYS: [&str; 4] = ["hangingRoman", "burasagari", "singleWordJustification", "justificationMethodType"];

/// A new style's properties: byte for byte what TyperTools' `getDefaultStyle()` returns.
pub fn default_props() -> Value {
    json!({
        "layerText": {
            "textGridding": "none",
            "orientation": "horizontal",
            "antiAlias": "antiAliasSmooth",
            "textStyleRange": [{
                "from": 0,
                "to": 100,
                "textStyle": {
                    "fontPostScriptName": DEFAULT_FONT,
                    "fontName": DEFAULT_FONT,
                    "fontStyleName": DEFAULT_FONT_STYLE,
                    "fontScript": 0,
                    "fontTechnology": 1,
                    "fontAvailable": true,
                    "size": DEFAULT_SIZE,
                    "impliedFontSize": DEFAULT_SIZE,
                    "horizontalScale": 100,
                    "verticalScale": 100,
                    "autoLeading": true,
                    "tracking": 0,
                    "baselineShift": 0,
                    "impliedBaselineShift": 0,
                    "autoKern": "metricsKern",
                    "fontCaps": "normal",
                    "digitSet": "defaultDigits",
                    "markYDistFromBaseline": 100,
                    "otbaseline": "normal",
                    "ligature": false,
                    "altligature": false,
                    "connectionForms": false,
                    "contextualLigatures": false,
                    "baselineDirection": "withStream",
                    "color": {"red": 0, "green": 0, "blue": 0}
                }
            }],
            "paragraphStyleRange": [{
                "from": 0,
                "to": 100,
                "paragraphStyle": {
                    "burasagari": "burasagariNone",
                    "singleWordJustification": "justifyAll",
                    "justificationMethodType": "justifMethodAutomatic",
                    "textEveryLineComposer": false,
                    "alignment": "center",
                    "hangingRoman": true,
                    "hyphenate": true
                }
            }]
        },
        "typeUnit": DEFAULT_TYPE_UNIT
    })
}

/// The same, lettering in `family` — what a *new* style uses once the host has told the panel which
/// fonts exist, so a style made on Linux does not open with a missing-font warning.
pub fn default_props_for(family: &str, font_style: &str) -> Value {
    let mut props = default_props();
    set_font(&mut props, family, font_style, Some(family));
    props
}

/// A `textProps` object that has the shape everything below assumes: `layerText` with a non-empty
/// `textStyleRange` and `paragraphStyleRange`, and a string `typeUnit`. What is already there is
/// kept — this fills gaps, it does not reset — so a partial or hand-written library still works.
/// Anything that is not an object becomes the default style.
pub fn normalise(v: Value) -> Value {
    let Value::Object(from) = v else {
        return default_props();
    };
    // Rebuilt as an object-merge over the defaults rather than field by field: Adobe's shape has
    // many keys this port never reads, and dropping them would lose them on the next export.
    let mut out = default_props();
    merge_into(&mut out, &Value::Object(from));
    if !matches!(out.get("typeUnit"), Some(Value::String(_))) {
        out["typeUnit"] = json!(DEFAULT_TYPE_UNIT);
    }
    // Ranges that are missing, empty or the wrong shape are the one thing a caller cannot work
    // around, so they are replaced rather than merged.
    for (range, entry) in [("textStyleRange", "textStyle"), ("paragraphStyleRange", "paragraphStyle")] {
        let slot = out.get_mut("layerText").and_then(|l| l.get_mut(range));
        let ok = matches!(slot, Some(Value::Array(a)) if matches!(a.first(), Some(Value::Object(o)) if o.get(entry).is_some_and(Value::is_object)));
        if !ok && let Some(slot) = slot {
            let fresh = default_props();
            *slot = fresh.get("layerText").and_then(|l| l.get(range)).cloned().unwrap_or_else(|| json!([]));
        }
    }
    out
}

/// A deep merge of `from` into `into`: objects merge key by key, arrays merge entry by entry, and
/// everything else replaces.
///
/// Arrays are merged rather than replaced because Adobe's shape keeps almost everything in one:
/// a file that says only `textStyleRange: [{textStyle: {size: 30}}]` means "30, and whatever else
/// the default style says", and replacing the default range with it would leave the style with no
/// font at all. A key the defaults do not have is added; one the file does not have is kept.
fn merge_into(into: &mut Value, from: &Value) {
    match (into, from) {
        (Value::Object(into), Value::Object(from)) => {
            for (k, v) in from {
                match into.get_mut(k) {
                    // One rule for every shape: merge when both sides are the same kind of
                    // container, replace otherwise — which the fallback arm below does.
                    Some(slot) => merge_into(slot, v),
                    None => {
                        into.insert(k.clone(), v.clone());
                    }
                }
            }
        }
        // Entry by entry: an object entry merges with the default's, anything else replaces it, and
        // entries the file has past the end of the default's array are appended.
        (Value::Array(into), Value::Array(from)) => {
            for (i, v) in from.iter().enumerate() {
                match into.get_mut(i) {
                    Some(slot) if slot.is_object() && v.is_object() => merge_into(slot, v),
                    Some(slot) => *slot = v.clone(),
                    None => into.push(v.clone()),
                }
            }
            // An array the file emptied stays empty rather than keeping the default's entries:
            // `normalise` is what replaces an empty range, since that is the one thing a style
            // cannot be.
            into.truncate(from.len());
        }
        (into, from) => *into = from.clone(),
    }
}

/// `typeUnit`, or the default when a style names one this port does not know.
pub fn type_unit(props: &Value) -> &str {
    props.get("typeUnit").and_then(Value::as_str).unwrap_or(DEFAULT_TYPE_UNIT)
}

/// The layer-level `layerText` object.
pub fn layer_text(props: &Value) -> Option<&Value> {
    props.get("layerText")
}

/// One character property (`layerText.textStyleRange[0].textStyle.<key>`).
pub fn char<'a>(props: &'a Value, key: &str) -> Option<&'a Value> {
    props.get("layerText")?.get("textStyleRange")?.get(0)?.get("textStyle")?.get(key)
}

/// One paragraph property (`layerText.paragraphStyleRange[0].paragraphStyle.<key>`).
pub fn para<'a>(props: &'a Value, key: &str) -> Option<&'a Value> {
    props.get("layerText")?.get("paragraphStyleRange")?.get(0)?.get("paragraphStyle")?.get(key)
}

/// Sets a character property. The key is *inserted* when the style does not have it: Adobe's shape
/// omits every property at its default, so `leading`, `kerning` and `baseline` are all absent from
/// TyperTools' default style and a style taken off a layer has to be able to gain them. A path that
/// is not there at all is left alone — [`normalise`] is what guarantees the shape, and a property
/// that cannot be written is not worth a panic.
pub fn set_char(props: &mut Value, key: &str, value: Value) {
    if let Some(style) = props
        .get_mut("layerText")
        .and_then(|l| l.get_mut("textStyleRange"))
        .and_then(|r| r.get_mut(0))
        .and_then(|e| e.get_mut("textStyle"))
        .and_then(|s| s.as_object_mut())
    {
        style.insert(key.to_string(), value);
    }
}

/// Sets a paragraph property, or removes it when `value` is `Value::Null` (Adobe writes "unset" as
/// an absent key, and TyperTools' own editor deletes rather than writing nulls).
pub fn set_para(props: &mut Value, key: &str, value: Value) {
    let Some(style) = props
        .get_mut("layerText")
        .and_then(|l| l.get_mut("paragraphStyleRange"))
        .and_then(|r| r.get_mut(0))
        .and_then(|e| e.get_mut("paragraphStyle"))
        .and_then(|s| s.as_object_mut())
    else {
        return;
    };
    if value.is_null() {
        style.remove(key);
    } else {
        style.insert(key.to_string(), value);
    }
}

/// The paragraph options the style asks for that this host has no equivalent for. Adobe's shape
/// includes them, and every style carries them at their inert defaults — `burasagariNone`,
/// `hangingRoman: true` — so only a value that *departs* from those counts as asking for something.
/// The port keeps them in the library rather than dropping them, but it cannot apply them, so the
/// editor names the ones a style is asking for instead of letting an option that does nothing look
/// as though it works.
pub fn unsupported_paragraph_keys(props: &Value) -> Vec<&'static str> {
    let inert = default_props();
    UNSUPPORTED_PARAGRAPH_KEYS
        .iter()
        .copied()
        .filter(|k| match para(props, k) {
            None => false,
            Some(v) => para(&inert, k) != Some(v),
        })
        .collect()
}

/// Removes a character property.
pub fn remove_char(props: &mut Value, key: &str) {
    if let Some(style) = props
        .get_mut("layerText")
        .and_then(|l| l.get_mut("textStyleRange"))
        .and_then(|r| r.get_mut(0))
        .and_then(|e| e.get_mut("textStyle"))
        .and_then(|s| s.as_object_mut())
    {
        style.remove(key);
    }
}

/// The family a style letters in.
pub fn family(props: &Value) -> String {
    char(props, "fontName").and_then(Value::as_str).unwrap_or(DEFAULT_FONT).to_string()
}

/// The face name a style letters in.
pub fn font_style(props: &Value) -> String {
    char(props, "fontStyleName").and_then(Value::as_str).unwrap_or(DEFAULT_FONT_STYLE).to_string()
}

/// Sets the font, the way TyperTools' editor does: all three names move together, because
/// PhotoCraft resolves the face from family plus style and Adobe from the PostScript name.
pub fn set_font(props: &mut Value, family: &str, font_style: &str, postscript: Option<&str>) {
    set_char(props, "fontName", json!(family));
    set_char(props, "fontStyleName", json!(font_style));
    set_char(props, "fontPostScriptName", json!(postscript.unwrap_or(family)));
}

/// The layer's anti-aliasing, in Adobe's naming.
pub fn anti_alias(props: &Value) -> &str {
    layer_text(props).and_then(|l| l.get("antiAlias")).and_then(Value::as_str).unwrap_or("antiAliasSmooth")
}

pub fn set_anti_alias(props: &mut Value, value: &str) {
    if let Some(layer) = props.get_mut("layerText").and_then(Value::as_object_mut) {
        layer.insert("antiAlias".to_string(), json!(value));
    }
}

/// The layer's orientation: `horizontal` or `vertical` (tategaki).
pub fn orientation(props: &Value) -> &str {
    layer_text(props).and_then(|l| l.get("orientation")).and_then(Value::as_str).unwrap_or("horizontal")
}

/// A style's size in points, at the document's resolution: `size` in `typeUnit`.
pub fn size_pt(props: &Value, dpi: f64) -> f64 {
    let size = char(props, "size").and_then(Value::as_f64).unwrap_or(DEFAULT_SIZE);
    geom::length_to_pt(size, Some(type_unit(props)), dpi).max(geom::MIN_SIZE_PT)
}

/// Writes a size in points back as `size` in the style's unit, with `impliedFontSize` following it
/// as Adobe keeps the two together.
pub fn set_size_pt(props: &mut Value, pt: f64, dpi: f64) {
    let v = geom::pt_to_length(pt, Some(type_unit(props)), dpi);
    set_char(props, "size", json!(v));
    set_char(props, "impliedFontSize", json!(v));
}

/// Adobe's anti-aliasing name for a `type.edit` parameter.
pub fn anti_alias_param(name: &str) -> &'static str {
    match name {
        "antiAliasNone" => "none",
        "antiAliasCrisp" => "crisp",
        "antiAliasStrong" => "strong",
        // PhotoCraft renders both platform modes with the `Sharp` rasteriser (see `AntiAlias`).
        "antiAliasPlatformGray" | "antiAliasPlatformLCD" | "antiAliasSharp" => "sharp",
        _ => "smooth",
    }
}

/// Adobe's name for the anti-aliasing PhotoCraft reports, the inverse of [`anti_alias_param`]. The
/// two platform modes are the only lossy pair: they come back as `antiAliasSharp`.
pub fn anti_alias_name(param: &str) -> &'static str {
    match param {
        "None" => "antiAliasNone",
        "Sharp" => "antiAliasSharp",
        "Windows" => "antiAliasPlatformGray",
        "WindowsLcd" => "antiAliasPlatformLCD",
        "Crisp" => "antiAliasCrisp",
        "Strong" => "antiAliasStrong",
        _ => "antiAliasSmooth",
    }
}

/// A `weight`/`italic` pair as a face name, for the case where [`from_info`] has a weight but no
/// style name. The engine reads the weight back out of the name it is given
/// (`fonts::guess_from_postscript`), so a synthesised name is not a guess at the font — it is the
/// same information in the form the style's own field carries.
pub fn style_name_from(weight: u16, italic: bool) -> String {
    let base = match weight {
        0..=149 => "Thin",
        150..=249 => "ExtraLight",
        250..=349 => "Light",
        350..=449 => "Regular",
        450..=549 => "Medium",
        550..=649 => "SemiBold",
        650..=749 => "Bold",
        750..=849 => "ExtraBold",
        _ => "Black",
    };
    if italic { format!("{base} Italic") } else { base.to_string() }
}

/// The baseline a shift and a size imply: Adobe's script attribute, recovered from the emulation
/// [`to_params`] writes. The ratio is `SCRIPT_SHIFT / SCRIPT_SIZE` for raised text, its negative for
/// lowered, and the thresholds are well clear of a hand-set baseline shift.
pub fn baseline_from(shift_pt: f64, size_pt: f64) -> &'static str {
    if !(size_pt.is_finite() && size_pt > 0.0) {
        return "normal";
    }
    let ratio = shift_pt / size_pt;
    if ratio > 0.5 {
        "superScript"
    } else if ratio < -0.5 {
        "subScript"
    } else {
        "normal"
    }
}

/// A jam colour (`{red, green, blue}` 0–255) as the `#rrggbb` the engine takes.
pub fn color_param(color: &Value) -> String {
    let component = |k: &str| color.get(k).and_then(Value::as_f64).unwrap_or(0.0).clamp(0.0, 255.0).round() as u8;
    format!("#{:02x}{:02x}{:02x}", component("red"), component("green"), component("blue"))
}

/// The `#rrggbb` the editor shows for a style.
pub fn color_hex(props: &Value) -> String {
    color_param(char(props, "color").unwrap_or(&Value::Null))
}

/// `#rrggbb` (or `#rgb`) back into a jam colour, for the editor's colour field.
pub fn hex_color(hex: &str) -> Value {
    let digits: Vec<u8> = hex.trim_start_matches('#').chars().filter_map(|c| c.to_digit(16).map(|d| d as u8)).collect();
    let (r, g, b) = match digits.len() {
        3 => (digits[0] * 17, digits[1] * 17, digits[2] * 17),
        6 => (digits[0] * 16 + digits[1], digits[2] * 16 + digits[3], digits[4] * 16 + digits[5]),
        // Anything else is black, as Adobe's own colour parser falls back to. The editor validates
        // what it accepts, so this is a floor, not a policy.
        _ => (0, 0, 0),
    };
    json!({"red": r, "green": g, "blue": b})
}

/// A style's properties as the parameters of `type.create` or `type.setStyle`.
///
/// `scale` multiplies the size and the leading: TyperTools' Text Scale field does exactly this, and
/// only on Create and Insert (`previewBlock.jsx`), never when a style is applied to a layer.
pub fn to_params(props: &Value, dpi: f64, scale: f64) -> Value {
    let scale = if scale.is_finite() && scale > 0.0 { scale } else { 1.0 };
    let mut params = Map::new();

    // Font: family and face. `postscriptName` is deliberately not sent — the engine prefers the
    // installed face's real PostScript name over anything a style claims (#3556).
    let mut font_any = false;
    if let Some(v) = char(props, "fontName").and_then(Value::as_str) {
        params.insert("font".into(), json!(v));
        font_any = true;
    }
    if let Some(v) = char(props, "fontStyleName").and_then(Value::as_str) {
        params.insert("fontStyle".into(), json!(v));
        font_any = true;
    }
    if !font_any {
        params.insert("font".into(), json!(family(props)));
    }

    // Orientation, which Adobe keeps on the layer rather than in a style range. `type.create` is the
    // one command that takes it, so a style written for vertical type (tategaki) letters vertically;
    // turning a layer that already exists is `type.orientation.*`, which is the layer's own business
    // and not something a style gets to decide behind the letterer's back.
    params.insert("orientation".into(), json!(orientation_param(props)));

    let mut size = size_pt(props, dpi) * scale;
    // Leading: `autoLeading` true (or no `leading`) means automatic, which the engine spells
    // "auto" — a *set* leading, so a layer that had a fixed one loses it, as TyperTools' own
    // whole-style replacement does.
    match char(props, "leading").and_then(Value::as_f64) {
        Some(leading) if !is_true(char(props, "autoLeading")) => {
            params.insert("leading".into(), json!((geom::length_to_pt(leading, Some(type_unit(props)), dpi) * scale).max(geom::LEADING_MIN_PT)));
        }
        _ => {
            params.insert("leading".into(), json!("auto"));
        }
    }

    // Baseline, then the super/subscript emulation, which is expressed *as* a baseline shift and a
    // size and must therefore win over the style's own two fields.
    let mut shift = char(props, "baselineShift").and_then(Value::as_f64).map(|v| geom::length_to_pt(v, Some(type_unit(props)), dpi)).unwrap_or(0.0);
    match char(props, "baseline").and_then(Value::as_str) {
        Some("superScript") | Some("subScript") => {
            shift = size * SCRIPT_SHIFT * if char(props, "baseline").and_then(Value::as_str) == Some("subScript") { -1.0 } else { 1.0 };
            size *= SCRIPT_SIZE;
        }
        _ => {}
    }
    params.insert("size".into(), json!(size.clamp(geom::MIN_SIZE_PT, geom::MAX_SIZE_PT)));
    params.insert("baselineShift".into(), json!(shift.clamp(-geom::BASELINE_SHIFT_MAX_PT, geom::BASELINE_SHIFT_MAX_PT)));

    for (jam, param) in [("horizontalScale", "horizontalScale"), ("verticalScale", "verticalScale")] {
        if let Some(v) = char(props, jam).and_then(Value::as_f64) {
            params.insert(param.into(), json!(v));
        }
    }
    if let Some(v) = char(props, "tracking").and_then(Value::as_f64) {
        params.insert("tracking".into(), json!(v.clamp(geom::TRACKING_MIN, geom::TRACKING_MAX)));
    }
    // Kerning: Adobe's `autoKern` with an optional manual `kerning`. A manual value is a number,
    // which in the engine means "no automatic kerning plus this value" — the same thing.
    match char(props, "autoKern").and_then(Value::as_str) {
        Some("opticalKern") => {
            params.insert("kerning".into(), json!("optical"));
        }
        Some("manual") => match char(props, "kerning").and_then(Value::as_f64) {
            Some(k) if k.is_finite() => {
                params.insert("kerning".into(), json!(k.clamp(geom::KERN_MIN, geom::KERN_MAX)));
            }
            _ => {
                params.insert("kerning".into(), json!("off"));
            }
        },
        Some("metricsKern") => {
            params.insert("kerning".into(), json!("metrics"));
        }
        _ => {}
    }
    params.insert(
        "caps".into(),
        json!(match char(props, "fontCaps").and_then(Value::as_str) {
            Some("allCaps") => "all",
            Some("smallCaps") => "small",
            _ => "normal",
        }),
    );
    for (jam, param) in [("syntheticBold", "fauxBold"), ("syntheticItalic", "fauxItalic")] {
        if let Some(v) = char(props, jam).and_then(Value::as_bool) {
            params.insert(param.into(), json!(v));
        }
    }
    // Adobe writes an underline and a strike as enum names; anything but the "off" one is on.
    params.insert("underline".into(), json!(is_on(char(props, "underline"), "underlineOff")));
    params.insert("strikethrough".into(), json!(is_on(char(props, "strikethrough"), "strikethroughOff")));
    // Adobe's ligature switches are three separate keys; the standard one is `ligature`. An
    // untouched default (`false` in TyperTools' own default style) must not turn ligatures off
    // globally, so it is only sent when it is on — see the note in `README.md`.
    if is_true(char(props, "ligature")) || is_true(char(props, "altligature")) || is_true(char(props, "contextualLigatures")) {
        params.insert("ligatures".into(), json!(true));
    }
    if is_true(char(props, "altligature")) {
        params.insert("discretionaryLigatures".into(), json!(true));
    }
    params.insert("color".into(), json!(color_param(char(props, "color").unwrap_or(&Value::Null))));

    // Paragraph.
    if let Some(a) = para(props, "alignment").and_then(Value::as_str) {
        params.insert(
            "align".into(),
            json!(match a {
                "center" => "center",
                "right" => "right",
                "justify" | "justifyLeft" => "justify",
                "justifyCenter" => "justifyCenter",
                "justifyRight" => "justifyRight",
                "justifyAll" => "justifyAll",
                _ => "left",
            }),
        );
    }
    if let Some(v) = para(props, "autoLeadingPercentage").and_then(Value::as_f64) {
        params.insert("autoLeading".into(), json!(v * 100.0));
    }
    for (jam, param) in [
        ("firstLineIndent", "firstLineIndent"),
        ("leftIndent", "startIndent"),
        ("rightIndent", "endIndent"),
        ("spaceBefore", "spaceBefore"),
        ("spaceAfter", "spaceAfter"),
    ] {
        if let Some(v) = para(props, jam).and_then(Value::as_f64) {
            params.insert(param.into(), json!(geom::length_to_pt(v, Some(type_unit(props)), dpi)));
        }
    }
    if let Some(v) = para(props, "hyphenate").and_then(Value::as_bool) {
        params.insert("hyphenate".into(), json!(v));
    }
    Value::Object(params)
}

/// The `orientation` parameter for `type.create`, from a style.
pub fn orientation_param(props: &Value) -> &'static str {
    if orientation(props) == "vertical" { "vertical" } else { "horizontal" }
}

/// A `textProps` object for the text on a layer, from `type.info`'s reply — what "copy the style of
/// the active layer" builds, and what makes a style from a Photoshop-shaped library editable here.
///
/// Every length `type.info` reports is in **points**, so each one is converted into `unit` at the
/// document's resolution: a style has to say what unit it is in, and handing a point value to a
/// style that says `pixelsUnit` would make it letter a quarter of the size it was copied from.
pub fn from_info(info: &Value, unit: &str, dpi: f64) -> Value {
    let mut props = default_props();
    props["typeUnit"] = json!(unit);
    let len = |v: f64| geom::pt_to_length(v, Some(unit), dpi);
    let style = info.get("runs").and_then(Value::as_array).and_then(|r| r.first()).and_then(|r| r.get("style"));
    if let Some(s) = style {
        let num = |k: &str| s.get(k).and_then(Value::as_f64);
        let size = num("size_pt").unwrap_or(DEFAULT_SIZE);
        set_font(
            &mut props,
            s.get("font_family").and_then(Value::as_str).unwrap_or(DEFAULT_FONT),
            &style_name_of(s),
            s.get("postscript_name").and_then(Value::as_str),
        );
        // The size, in the unit this style is written in: `type.info` reports points, and a point
        // value written into a `pixelsUnit` style would letter a quarter of the size it was copied
        // from. `impliedFontSize` follows it, as Adobe keeps the two together.
        set_size_pt(&mut props, size, dpi);
        set_char(&mut props, "horizontalScale", json!(num("horizontal_scale").unwrap_or(1.0) * 100.0));
        set_char(&mut props, "verticalScale", json!(num("vertical_scale").unwrap_or(1.0) * 100.0));
        set_char(&mut props, "tracking", json!(num("tracking").unwrap_or(0.0)));
        let shift = num("baseline_shift_pt").unwrap_or(0.0);
        set_char(&mut props, "baselineShift", json!(len(shift)));
        set_char(&mut props, "impliedBaselineShift", json!(len(shift)));
        set_char(&mut props, "baseline", json!(baseline_from(shift, size)));
        set_char(
            &mut props,
            "autoKern",
            json!(match s.get("kerning").and_then(Value::as_str) {
                Some("Optical") => "opticalKern",
                Some("Off") => "manual",
                _ => "metricsKern",
            }),
        );
        if let Some(k) = num("kern").filter(|k| *k != 0.0) {
            set_char(&mut props, "kerning", json!(k));
        }
        set_char(
            &mut props,
            "fontCaps",
            json!(match s.get("caps").and_then(Value::as_str) {
                Some("AllCaps") => "allCaps",
                Some("SmallCaps") => "smallCaps",
                _ => "normal",
            }),
        );
        for (key, jam) in [("ligatures", "ligature"), ("discretionary_ligatures", "altligature")] {
            set_char(&mut props, jam, json!(s.get(key).and_then(Value::as_bool).unwrap_or(false)));
        }
        for (key, jam) in [("underline", "underline"), ("strikethrough", "strikethrough")] {
            let on = s.get(key).and_then(Value::as_bool).unwrap_or(false);
            set_char(
                &mut props,
                jam,
                json!(if on {
                    if jam == "underline" { "underlineOnLeftInVertical" } else { "xHeightStrikethroughOn" }
                } else if jam == "underline" {
                    "underlineOff"
                } else {
                    "strikethroughOff"
                }),
            );
        }
        set_char(&mut props, "color", color_of(s.get("color")));
        // A fixed leading, if the layer has one; auto leading otherwise, which is Adobe's absent
        // `leading` plus `autoLeading: true`.
        match num("leading_pt") {
            Some(leading) => {
                set_char(&mut props, "leading", json!(len(leading)));
                set_char(&mut props, "autoLeading", json!(false));
            }
            None => {
                remove_char(&mut props, "leading");
                set_char(&mut props, "autoLeading", json!(true));
            }
        }
    }
    set_anti_alias(&mut props, anti_alias_name(info.get("antialias").and_then(Value::as_str).unwrap_or("Smooth")));
    if let Some(layer) = props.get_mut("layerText").and_then(Value::as_object_mut) {
        layer.insert("orientation".into(), json!(if info.get("orientation").and_then(Value::as_str) == Some("Vertical") { "vertical" } else { "horizontal" }));
        // `bounds` is deliberately not carried: it is the layer's, not the style's, and TyperTools
        // deletes it when a style is taken off a layer (`copyLayerStyle`).
    }
    // No paragraph style at all is `left`, the alignment every layer starts in — not the default
    // *style*'s centre, which is TyperTools' own taste for a style it is about to write, not a
    // reading of one that exists.
    set_para(&mut props, "alignment", json!("left"));
    if let Some(p) = info.get("paragraphs").and_then(Value::as_array).and_then(|r| r.first()).and_then(|r| r.get("style")) {
        let num = |k: &str| p.get(k).and_then(Value::as_f64);
        set_para(
            &mut props,
            "alignment",
            json!(match p.get("align").and_then(Value::as_str) {
                Some("Center") => "center",
                Some("Right") => "right",
                Some("JustifyLeft") => "justifyLeft",
                Some("JustifyCenter") => "justifyCenter",
                Some("JustifyRight") => "justifyRight",
                Some("JustifyAll") => "justifyAll",
                _ => "left",
            }),
        );
        set_para(&mut props, "autoLeadingPercentage", json!(num("auto_leading").unwrap_or(DEFAULT_AUTO_LEADING)));
        set_para(&mut props, "hyphenate", json!(p.get("hyphenate").and_then(Value::as_bool).unwrap_or(false)));
        for (key, jam) in [
            ("first_line_indent_pt", "firstLineIndent"),
            ("start_indent_pt", "leftIndent"),
            ("end_indent_pt", "rightIndent"),
            ("space_before_pt", "spaceBefore"),
            ("space_after_pt", "spaceAfter"),
        ] {
            if let Some(v) = num(key).filter(|v| *v != 0.0) {
                set_para(&mut props, jam, json!(len(v)));
            }
        }
    }
    normalise(props)
}

/// The style name of a `CharStyle` as serialised by `type.info`: its own face name, or one made
/// from its weight and slant when it has none.
fn style_name_of(s: &Value) -> String {
    match s.get("font_style").and_then(Value::as_str) {
        Some(name) if !name.is_empty() => name.to_string(),
        _ => style_name_from(
            s.get("weight").and_then(Value::as_u64).unwrap_or(400).min(u16::MAX as u64) as u16,
            s.get("italic").and_then(Value::as_bool).unwrap_or(false),
        ),
    }
}

/// A serialised `Color` (`{mode, c: [r, g, b, …], alpha}`) as a jam colour. The components are
/// normalised floats, so they are scaled to 0–255 as Adobe stores them — and stored as whole
/// numbers, because that is what a jam colour is and what [`hex_color`] produces.
fn color_of(color: Option<&Value>) -> Value {
    let Some(c) = color.and_then(|c| c.get("c")).and_then(Value::as_array) else {
        return json!({"red": 0, "green": 0, "blue": 0});
    };
    let at = |i: usize| (c.get(i).and_then(Value::as_f64).unwrap_or(0.0).clamp(0.0, 1.0) * 255.0).round() as u8;
    json!({"red": at(0), "green": at(1), "blue": at(2)})
}

/// Adobe writes booleans as booleans, but a hand-edited library may hold `"true"`, `1` or `"yes"`.
pub fn is_true(v: Option<&Value>) -> bool {
    match v {
        Some(Value::Bool(b)) => *b,
        Some(Value::String(s)) => !s.is_empty() && s != "false",
        Some(Value::Number(n)) => n.as_f64().is_some_and(|n| n != 0.0),
        _ => false,
    }
}

/// Adobe's on/off enums: absent means off, and the "off" member means off; anything else is on.
pub fn is_on(v: Option<&Value>, off: &str) -> bool {
    match v {
        Some(Value::String(s)) => s != off && !s.is_empty(),
        other => is_true(other),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// 300 dpi: the resolution the lettering corpora are drawn at.
    const DPI: f64 = 300.0;

    #[test]
    fn the_default_style_is_typertools_own() {
        let p = default_props();
        assert_eq!(char(&p, "fontName"), Some(&json!(DEFAULT_FONT)));
        assert_eq!(char(&p, "size"), Some(&json!(14.0)));
        assert_eq!(char(&p, "autoKern"), Some(&json!("metricsKern")));
        assert_eq!(char(&p, "autoLeading"), Some(&json!(true)));
        assert_eq!(char(&p, "fontCaps"), Some(&json!("normal")));
        assert_eq!(para(&p, "alignment"), Some(&json!("center")), "TyperTools' default style is centred");
        assert_eq!(para(&p, "hangingRoman"), Some(&json!(true)));
        assert_eq!(para(&p, "hyphenate"), Some(&json!(true)));
        assert_eq!(type_unit(&p), "pixelsUnit");
    }

    #[test]
    fn a_new_style_can_letter_in_a_font_the_host_has() {
        let p = default_props_for("Inter", "Regular");
        assert_eq!(family(&p), "Inter");
        assert_eq!(char(&p, "fontPostScriptName"), Some(&json!("Inter")));
        // Everything else is untouched.
        assert_eq!(char(&p, "size"), Some(&json!(14.0)));
    }

    #[test]
    fn normalising_fills_gaps_without_resetting_what_is_there() {
        // A library from Photoshop with a property this port never reads: it must survive.
        let v = json!({"layerText": {"textStyleRange": [{"textStyle": {"size": 30, "someAdobKey": [1, 2]}}]}, "typeUnit": "pointsUnit"});
        let p = normalise(v);
        assert_eq!(char(&p, "size"), Some(&json!(30)), "what the file said, as the file wrote it");
        assert_eq!(char(&p, "someAdobKey"), Some(&json!([1, 2])), "an unknown key is kept");
        assert_eq!(char(&p, "fontName"), Some(&json!(DEFAULT_FONT)), "and the defaults fill the rest");
        assert_eq!(para(&p, "alignment"), Some(&json!("center")));
        assert_eq!(type_unit(&p), "pointsUnit");
        assert_eq!(normalise(json!("nonsense")), default_props());
        assert_eq!(normalise(Value::Null), default_props());
        // A range that is empty is the one thing that cannot be merged around.
        let p = normalise(json!({"layerText": {"textStyleRange": []}}));
        assert_eq!(char(&p, "size"), Some(&json!(DEFAULT_SIZE)));
        let p = normalise(json!({"layerText": {"paragraphStyleRange": [{"paragraphStyle": 7}]}}));
        assert_eq!(para(&p, "alignment"), Some(&json!("center")));
    }

    #[test]
    fn a_point_size_becomes_points_at_the_documents_resolution() {
        let p = normalise(json!({"typeUnit": "pixelsUnit", "layerText": {"textStyleRange": [{"textStyle": {"size": 300}}]}}));
        // 300 px at 300 dpi is an inch: 72 pt.
        assert!((size_pt(&p, DPI) - 72.0).abs() < 1e-9);
        let mut q = p.clone();
        set_size_pt(&mut q, 72.0, DPI);
        assert_eq!(char(&q, "size"), Some(&json!(300.0)), "and back again");
        assert_eq!(char(&q, "impliedFontSize"), Some(&json!(300.0)));
        // A style written in points is not converted.
        let r = normalise(json!({"typeUnit": "pointsUnit", "layerText": {"textStyleRange": [{"textStyle": {"size": 24}}]}}));
        assert!((size_pt(&r, DPI) - 24.0).abs() < 1e-9);
    }

    #[test]
    fn a_style_becomes_command_parameters() {
        let p = normalise(json!({
            "typeUnit": "pointsUnit",
            "layerText": {
                "antiAlias": "antiAliasCrisp",
                "textStyleRange": [{"textStyle": {
                    "fontName": "Tahoma", "fontStyleName": "Bold", "size": 24, "leading": 28,
                    "autoLeading": false, "tracking": 50, "horizontalScale": 90, "verticalScale": 110,
                    "baselineShift": 3, "color": {"red": 255, "green": 0, "blue": 128},
                    "autoKern": "opticalKern", "fontCaps": "smallCaps",
                    "syntheticBold": true, "syntheticItalic": false,
                    "underline": "underlineOnLeftInVertical", "strikethrough": "strikethroughOff"
                }}],
                "paragraphStyleRange": [{"paragraphStyle": {"alignment": "right", "autoLeadingPercentage": 1.5, "hyphenate": true}}]
            }
        }));
        let params = to_params(&p, DPI, 1.0);
        assert_eq!(params["font"], "Tahoma");
        assert_eq!(params["fontStyle"], "Bold");
        assert_eq!(params["size"], 24.0);
        assert_eq!(params["leading"], 28.0);
        assert_eq!(params["tracking"], 50.0);
        assert_eq!(params["horizontalScale"], 90.0);
        assert_eq!(params["baselineShift"], 3.0);
        assert_eq!(params["color"], "#ff0080");
        assert_eq!(params["kerning"], "optical");
        assert_eq!(params["caps"], "small");
        assert_eq!(params["fauxBold"], true);
        assert_eq!(params["fauxItalic"], false);
        assert_eq!(params["underline"], true);
        assert_eq!(params["strikethrough"], false);
        assert_eq!(params["align"], "right");
        assert_eq!(params["autoLeading"], 150.0);
        assert_eq!(params["hyphenate"], true);
        // The options with no host support are absent rather than invented.
        assert!(params.get("hangingRoman").is_none());
        assert!(params.get("burasagari").is_none());
    }

    #[test]
    fn auto_leading_is_sent_as_auto_and_a_set_one_as_a_number() {
        let auto = normalise(json!({"typeUnit": "pointsUnit", "layerText": {"textStyleRange": [{"textStyle": {"leading": 30, "autoLeading": true}}]}}));
        assert_eq!(to_params(&auto, 300.0, 1.0)["leading"], "auto", "autoLeading wins over a stale leading");
        let set = normalise(json!({"typeUnit": "pointsUnit", "layerText": {"textStyleRange": [{"textStyle": {"leading": 30, "autoLeading": false}}]}}));
        assert_eq!(to_params(&set, 300.0, 1.0)["leading"], 30.0);
        // A leading in the library's own unit is converted like the size is: 30 px at 300 dpi.
        let px = normalise(json!({"typeUnit": "pixelsUnit", "layerText": {"textStyleRange": [{"textStyle": {"leading": 30, "autoLeading": false}}]}}));
        assert!((to_params(&px, 300.0, 1.0)["leading"].as_f64().unwrap_or(0.0) - 7.2).abs() < 1e-9);
        // No leading at all is automatic, which is Adobe's convention.
        let none = normalise(json!({"layerText": {"textStyleRange": [{"textStyle": {}}]}}));
        assert_eq!(to_params(&none, 300.0, 1.0)["leading"], "auto");
    }

    #[test]
    fn the_text_scale_field_scales_the_size_and_the_leading() {
        let p =
            normalise(json!({"typeUnit": "pointsUnit", "layerText": {"textStyleRange": [{"textStyle": {"size": 20, "leading": 30, "autoLeading": false}}]}}));
        let params = to_params(&p, 300.0, 1.5);
        assert_eq!(params["size"], 30.0, "20 pt × 150 %");
        assert_eq!(params["leading"], 45.0);
        // Automatic leading has nothing to scale, and stays automatic.
        let auto = normalise(json!({"typeUnit": "pointsUnit", "layerText": {"textStyleRange": [{"textStyle": {"size": 20}}]}}));
        assert_eq!(to_params(&auto, 300.0, 1.5)["leading"], "auto");
        // A nonsense scale is no scaling rather than a nonsense size.
        assert_eq!(to_params(&p, 300.0, 0.0)["size"], 20.0);
        assert_eq!(to_params(&p, 300.0, f64::NAN)["size"], 20.0);
    }

    #[test]
    fn superscript_and_subscript_are_baseline_shift_and_size() {
        for (name, sign) in [("superScript", 1.0), ("subScript", -1.0)] {
            let p = normalise(json!({"typeUnit": "pointsUnit", "layerText": {"textStyleRange": [{"textStyle": {"size": 20, "baseline": name}}]}}));
            let params = to_params(&p, 300.0, 1.0);
            assert_eq!(params["size"], 20.0 * SCRIPT_SIZE);
            assert_eq!(params["baselineShift"], 20.0 * SCRIPT_SHIFT * sign);
            // And the ratio is what reads it back.
            assert_eq!(baseline_from(params["baselineShift"].as_f64().unwrap_or(0.0), params["size"].as_f64().unwrap_or(0.0)), name);
        }
        assert_eq!(baseline_from(0.0, 20.0), "normal");
        assert_eq!(baseline_from(3.0, 20.0), "normal", "an ordinary raised baseline is not a superscript");
        assert_eq!(baseline_from(1.0, 0.0), "normal", "and nothing divides by zero");
    }

    #[test]
    fn a_colour_survives_both_ways() {
        let v = hex_color("#ff0080");
        assert_eq!(v, json!({"red": 255, "green": 0, "blue": 128}));
        assert_eq!(color_param(&v), "#ff0080");
        assert_eq!(color_param(&hex_color("#abc")), "#aabbcc", "the short form is expanded");
        assert_eq!(color_param(&hex_color("nonsense")), "#000000");
        assert_eq!(color_param(&json!({"red": 999, "green": -5})), "#ff0000");
        assert_eq!(color_param(&Value::Null), "#000000");
    }

    #[test]
    fn a_style_can_be_taken_off_a_layer() {
        // `type.info`'s shape, from `crates/engine/src/type_cmds.rs::info`.
        let info = json!({
            "layer": 3,
            "text": "Hello",
            "shape": {"Box": {"x": 0.0, "y": 0.0, "width": 100.0, "height": 50.0}},
            "antialias": "Crisp",
            "orientation": "Vertical",
            "runs": [{"start": 0, "end": 5, "style": {
                "font_family": "Inter", "font_style": "", "postscript_name": "Inter-Bold",
                "weight": 700, "italic": true, "size_pt": 36.0,
                "color": {"mode": "Rgb", "c": [1.0, 0.0, 0.5, 0.0], "alpha": 1.0},
                "tracking": 25.0, "leading_pt": 42.0, "baseline_shift_pt": 0.0,
                "horizontal_scale": 0.9, "vertical_scale": 1.0,
                "underline": true, "strikethrough": false, "faux_bold": false, "faux_italic": false,
                "kerning": "Optical", "kern": 0.0, "caps": "AllCaps",
                "ligatures": true, "discretionary_ligatures": false
            }}],
            "paragraphs": [{"start": 0, "end": 5, "style": {
                "align": "JustifyAll", "first_line_indent_pt": 0.0, "start_indent_pt": 12.0,
                "end_indent_pt": 0.0, "space_before_pt": 0.0, "space_after_pt": 6.0,
                "auto_leading": 1.4, "direction": "Auto", "hyphenate": true
            }}],
            "bounds": [0.0, 0.0, 100.0, 50.0],
        });
        let p = from_info(&info, "pointsUnit", DPI);
        assert_eq!(family(&p), "Inter");
        assert_eq!(char(&p, "fontStyleName"), Some(&json!("Bold Italic")), "a weight and a slant make a face name");
        assert_eq!(char(&p, "fontPostScriptName"), Some(&json!("Inter-Bold")));
        assert_eq!(char(&p, "size"), Some(&json!(36.0)));
        assert_eq!(char(&p, "leading"), Some(&json!(42.0)));
        assert_eq!(char(&p, "autoLeading"), Some(&json!(false)));
        assert_eq!(char(&p, "color"), Some(&json!({"red": 255, "green": 0, "blue": 128})));
        assert_eq!(char(&p, "autoKern"), Some(&json!("opticalKern")));
        assert_eq!(char(&p, "fontCaps"), Some(&json!("allCaps")));
        assert_eq!(char(&p, "underline"), Some(&json!("underlineOnLeftInVertical")));
        assert_eq!(anti_alias(&p), "antiAliasCrisp");
        assert_eq!(orientation(&p), "vertical");
        assert_eq!(para(&p, "alignment"), Some(&json!("justifyAll")));
        assert_eq!(para(&p, "autoLeadingPercentage"), Some(&json!(1.4)));
        assert_eq!(para(&p, "leftIndent"), Some(&json!(12.0)));
        assert_eq!(para(&p, "spaceAfter"), Some(&json!(6.0)));
        // The layer's bounds are not part of a style.
        assert!(layer_text(&p).and_then(|l| l.get("bounds")).is_none());
        // The paragraph options with no host support keep the defaults rather than being invented.
        assert_eq!(para(&p, "hangingRoman"), Some(&json!(true)));
        // And the whole thing is a style that translates back.
        let params = to_params(&p, 72.0, 1.0);
        assert_eq!(params["font"], "Inter");
        assert_eq!(params["size"], 36.0);
        assert_eq!(params["align"], "justifyAll");
    }

    #[test]
    fn only_a_paragraph_option_that_asks_for_something_counts_as_unsupported() {
        // Every style carries Adobe's paragraph options at their inert defaults. Saying "this host
        // cannot apply hanging punctuation" about all of them would be noise the letterer learns to
        // ignore, and the one style that really asks for it would be lost in it.
        assert!(unsupported_paragraph_keys(&default_props()).is_empty());

        let mut p = default_props();
        set_para(&mut p, "hangingRoman", json!(false));
        assert_eq!(unsupported_paragraph_keys(&p), ["hangingRoman"]);
        set_para(&mut p, "burasagari", json!("burasagariStandard"));
        assert_eq!(unsupported_paragraph_keys(&p), ["hangingRoman", "burasagari"], "in the list's own order");

        // A key that is absent is not a request either — and neither is one Adobe has but this port
        // cannot read at all, which is the same answer as one at its default.
        let mut p = default_props();
        set_para(&mut p, "hangingRoman", Value::Null);
        assert!(unsupported_paragraph_keys(&p).is_empty());

        // The options that *are* supported never appear, even when they are set away from default.
        let mut p = default_props();
        set_para(&mut p, "alignment", json!("justifyAll"));
        set_para(&mut p, "hyphenate", json!(false));
        assert!(unsupported_paragraph_keys(&p).is_empty());
    }

    #[test]
    fn a_layer_with_no_style_or_a_hostile_one_still_makes_a_style() {
        // Every field missing, wrong-typed or absurd; nothing here may panic.
        let info = json!({"runs": "not an array", "paragraphs": [7], "antialias": 5, "orientation": null, "pxPerPt": 0});
        let p = from_info(&info, "pixelsUnit", DPI);
        assert_eq!(family(&p), DEFAULT_FONT);
        assert_eq!(char(&p, "size"), Some(&json!(DEFAULT_SIZE)));
        assert_eq!(para(&p, "alignment"), Some(&json!("left")), "no paragraph style means left, not the default style's centre");
        assert!(to_params(&p, 72.0, 1.0).is_object());

        let p = from_info(&json!({}), "pixelsUnit", DPI);
        assert_eq!(type_unit(&p), "pixelsUnit");
        assert!(to_params(&p, 0.0, 1.0)["size"].as_f64().is_some_and(|v| v > 0.0), "and a nonsense resolution still gives a usable size");
    }

    #[test]
    fn a_style_with_every_field_absurd_still_makes_valid_parameters() {
        let p = normalise(json!({"typeUnit": "nonsense", "layerText": {
            "textStyleRange": [{"textStyle": {"size": -5, "leading": -1, "autoLeading": false, "tracking": 1e9, "horizontalScale": -3, "baselineShift": 1e12, "autoKern": "sideways", "fontCaps": 7}}],
            "paragraphStyleRange": [{"paragraphStyle": {"alignment": 3, "autoLeadingPercentage": -2}}]
        }}));
        let params = to_params(&p, 300.0, 1.0);
        let num = |k: &str| params.get(k).and_then(Value::as_f64).unwrap_or(f64::NAN);
        assert!(num("size") >= geom::MIN_SIZE_PT && num("size") <= geom::MAX_SIZE_PT, "{params}");
        assert!(num("leading") >= geom::LEADING_MIN_PT, "{params}");
        assert_eq!(num("tracking"), geom::TRACKING_MAX, "the engine's own limit");
        assert!(num("baselineShift").abs() <= geom::BASELINE_SHIFT_MAX_PT);
        assert_eq!(num("autoLeading"), -200.0, "clamped by the engine, not silently dropped");
        assert!(params.get("align").is_none(), "a paragraph style that is not a string is not an alignment");
    }

    #[test]
    fn anti_aliasing_maps_both_ways() {
        for (jam, param) in [
            ("antiAliasNone", "none"),
            ("antiAliasSharp", "sharp"),
            ("antiAliasCrisp", "crisp"),
            ("antiAliasStrong", "strong"),
            ("antiAliasSmooth", "smooth"),
            ("antiAliasPlatformGray", "sharp"),
            ("antiAliasPlatformLCD", "sharp"),
            ("something else", "smooth"),
        ] {
            assert_eq!(anti_alias_param(jam), param, "{jam}");
        }
        for (param, jam) in [
            ("None", "antiAliasNone"),
            ("Sharp", "antiAliasSharp"),
            ("Crisp", "antiAliasCrisp"),
            ("Strong", "antiAliasStrong"),
            ("Smooth", "antiAliasSmooth"),
            ("Windows", "antiAliasPlatformGray"),
            ("WindowsLcd", "antiAliasPlatformLCD"),
        ] {
            assert_eq!(anti_alias_name(param), jam, "{param}");
        }
    }
}
