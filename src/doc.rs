//! What the panel reads back from the host: the document, a layer's type, and the fonts.
//!
//! Everything here is a **translation of the answers the host sends** (`doc.info`, `doc.changed`,
//! `type.info`, `type.fonts`) into the shapes the panel reasons about. Nothing is cached beyond the
//! last answer, and nothing is assumed to be there: a panel is handed whatever the document happens
//! to be, and a missing field is a `None` rather than a reason to fail. A plug-in that panicked on an
//! unexpected answer would take its own panel down, and the *Never crash* rule applies to plug-ins
//! exactly as it does to the host.

use serde_json::Value;

use crate::geom::Rect;

/// The active document, as `doc.info` and `doc.changed` describe it.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct Doc {
    pub width: f64,
    pub height: f64,
    /// Pixels per inch. Every point/pixel conversion in the port goes through this, because
    /// PhotoCraft sizes type in points and TyperTools' styles are written in the layer's ruler unit.
    pub dpi: f64,
    pub active_layer: Option<u64>,
    /// The layers the user has selected, in document order.
    pub selected: Vec<u64>,
    /// The marquee as `[x, y, width, height]` in document pixels, when there is one.
    pub selection: Option<[f64; 4]>,
    /// The document's revision, so a stale answer can be recognised.
    pub revision: u64,
    /// Every layer, groups and their children included, top to bottom.
    pub layers: Vec<LayerRef>,
}

/// One layer, as much of it as lettering needs: what it is called and what kind of thing it is.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct LayerRef {
    pub id: u64,
    pub name: String,
    /// `Layer::kind_name()`: `"Type"` for a type layer.
    pub kind: String,
}

impl Doc {
    /// The document a `doc.info` or `doc.changed` answer describes, or `None` when there is none
    /// (the event's `doc` is `null`, or the answer is not a document at all).
    pub fn from_json(v: &Value) -> Option<Doc> {
        if !v.is_object() {
            return None;
        }
        let mut layers = Vec::new();
        if let Some(top) = v.get("layers").and_then(Value::as_array) {
            collect_layers(top, &mut layers, 0);
        }
        Some(Doc {
            width: number(v, "width"),
            height: number(v, "height"),
            dpi: match number(v, "resolution") {
                dpi if dpi > 0.0 => dpi,
                _ => 72.0,
            },
            active_layer: v.get("activeLayer").and_then(Value::as_u64),
            selected: v.get("selectedLayers").and_then(Value::as_array).map(|a| a.iter().filter_map(Value::as_u64).collect()).unwrap_or_default(),
            selection: selection_of(v),
            revision: v.get("revision").and_then(Value::as_u64).unwrap_or(0),
            layers,
        })
    }

    /// Whether `id` is a type layer.
    pub fn is_type(&self, id: u64) -> bool {
        self.layers.iter().any(|l| l.id == id && l.kind == "Type")
    }

    /// A layer's name, for the panel to show — `"layer 7"` when the document did not say.
    pub fn name_of(&self, id: u64) -> String {
        self.layers.iter().find(|l| l.id == id).map(|l| l.name.clone()).unwrap_or_else(|| format!("layer {id}"))
    }

    /// The layers an action applies to, which is TyperTools' rule: every **selected** type layer, or
    /// — when the selection holds none — the active layer if that is one.
    ///
    /// A range selection in the Layers panel often includes raster balloons and shading layers along
    /// with the type; those are skipped rather than refused, because that is what a letterer selecting
    /// a page's worth of layers means.
    pub fn target_layers(&self) -> Vec<u64> {
        let mut out: Vec<u64> = self.selected.iter().copied().filter(|id| self.is_type(*id)).collect();
        if out.is_empty()
            && let Some(active) = self.active_layer.filter(|id| self.is_type(*id))
        {
            out.push(active);
        }
        out
    }
}

/// The marquee, when the answer says there is one. `selectionBounds` is `[x, y, w, h]`; a document
/// with `hasSelection: false` (or no bounds at all) has none.
///
/// Also called on a `doc.info` answer from a run's first step, which is why it is not private.
pub(crate) fn selection_of(v: &Value) -> Option<[f64; 4]> {
    if v.get("hasSelection").and_then(Value::as_bool) == Some(false) {
        return None;
    }
    let b = v.get("selectionBounds").and_then(Value::as_array)?;
    let mut out = [0.0; 4];
    for (i, slot) in out.iter_mut().enumerate() {
        *slot = b.get(i).and_then(Value::as_f64)?;
    }
    Some(out)
}

fn number(v: &Value, key: &str) -> f64 {
    v.get(key).and_then(Value::as_f64).filter(|n| n.is_finite()).unwrap_or(0.0)
}

/// Flattens the layer tree, groups included, bounded so a cyclic or absurd document cannot spin here.
fn collect_layers(from: &[Value], into: &mut Vec<LayerRef>, depth: usize) {
    if depth > 32 {
        return;
    }
    for l in from {
        if let Some(id) = l.get("id").and_then(Value::as_u64) {
            into.push(LayerRef {
                id,
                name: l.get("name").and_then(Value::as_str).unwrap_or_default().to_string(),
                kind: l.get("kind").and_then(Value::as_str).unwrap_or_default().to_string(),
            });
        }
        if let Some(children) = l.get("children").and_then(Value::as_array) {
            collect_layers(children, into, depth + 1);
        }
    }
}

/// A type layer as `type.info` describes it, reduced to what the geometry needs.
///
/// `bounds` is `[x0, y0, x1, y1]` in document pixels — the *content* box, not the text box, which is
/// the distinction the whole port turns on: TyperTools measures where the type actually landed and
/// then moves the layer by the difference between that and where it should be.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct Measured {
    /// The layer's text, as the host reports it.
    pub text: String,
    /// `true` for point text: one line from a click point, with no box to fit.
    pub point: bool,
    /// The size of the first run, in points.
    pub size_pt: f64,
    /// Where the type landed, when the host could measure it.
    pub bounds: Option<Rect>,
    /// The text box in document pixels, for paragraph text (`TextShape::Box`, through the layer's
    /// transform) — what Resize grows and Insert stretches.
    pub box_rect: Option<Rect>,
    /// The layer's anti-aliasing, as `type.info` spells it.
    pub antialias: Option<String>,
}

impl Measured {
    pub fn of(info: &Value) -> Measured {
        let transform = info.get("transform");
        Measured {
            text: info.get("text").and_then(Value::as_str).unwrap_or_default().to_string(),
            point: info.get("shape").and_then(Value::as_str) == Some("Point"),
            size_pt: info
                .get("runs")
                .and_then(Value::as_array)
                .and_then(|r| r.first())
                .and_then(|r| r.get("style"))
                .and_then(|s| s.get("size_pt"))
                .and_then(Value::as_f64)
                .filter(|s| s.is_finite() && *s > 0.0)
                .unwrap_or(crate::geom::FALLBACK_SIZE_PT),
            bounds: info.get("bounds").and_then(Value::as_array).and_then(|b| {
                let mut r = [0.0; 4];
                for (i, slot) in r.iter_mut().enumerate() {
                    *slot = b.get(i).and_then(Value::as_f64).filter(|n| n.is_finite())?;
                }
                Some(Rect::from_bounds(r))
            }),
            box_rect: box_rect(info.get("shape"), transform),
            antialias: info.get("antialias").and_then(Value::as_str).map(str::to_string),
        }
    }

    /// The measurement's centre, when it has one.
    pub fn centre(&self) -> Option<[f64; 2]> {
        self.bounds.map(Rect::centre)
    }
}

/// The text box of a paragraph layer, in document pixels: `shape.Box` is in *text* space, so the
/// layer's transform maps it onto the canvas. A rotation would make this a quadrilateral; the port
/// reads the axis-aligned box an unrotated layer has, and a rotated one falls back to its bounds.
fn box_rect(shape: Option<&Value>, transform: Option<&Value>) -> Option<Rect> {
    let b = shape?.get("Box")?;
    let x = b.get("x").and_then(Value::as_f64)?;
    let y = b.get("y").and_then(Value::as_f64)?;
    let w = b.get("width").and_then(Value::as_f64)?;
    let h = b.get("height").and_then(Value::as_f64)?;
    let m: Vec<f64> = match transform.and_then(Value::as_array) {
        Some(a) => a.iter().filter_map(Value::as_f64).collect(),
        None => vec![1.0, 0.0, 0.0, 1.0, 0.0, 0.0],
    };
    if m.len() != 6 || !m.iter().all(|n| n.is_finite()) {
        return None;
    }
    let (sx, sy) = ((m[0] * m[0] + m[1] * m[1]).sqrt(), (m[2] * m[2] + m[3] * m[3]).sqrt());
    Some(Rect::new(m[0] * x + m[2] * y + m[4], m[1] * x + m[3] * y + m[5], w * sx, h * sy))
}

/// The fonts the host has, as much of them as the style editor offers.
///
/// The family list comes from `type.fonts` and the faces from `type.fonts {"family": …}`, both of
/// which PhotoCraft answers from the fonts it can actually render with — there is no Adobe font
/// assumption anywhere in the port, and a style naming a font this machine does not have is reported
/// by the engine (with a warning on `type.create`) rather than silently substituted.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Fonts {
    /// Family names, in the order the host listed them.
    pub families: Vec<String>,
    /// The family the faces below belong to.
    pub face_family: String,
    /// That family's face ("style") names: `Regular`, `Bold Italic`, …
    pub faces: Vec<String>,
}

impl Fonts {
    /// Reads a `type.fonts` answer: the family list, or one family's faces.
    pub fn absorb(&mut self, v: &Value) {
        if let Some(f) = v.get("families").and_then(Value::as_array) {
            self.families = f.iter().filter_map(Value::as_str).map(str::to_string).collect();
        }
        if let Some(faces) = v.get("faces").and_then(Value::as_array) {
            let family = faces.first().and_then(|f| f.get("family")).and_then(Value::as_str).unwrap_or_default().to_string();
            self.face_family = family;
            self.faces = faces.iter().filter_map(|f| f.get("style").and_then(Value::as_str)).map(str::to_string).collect();
        }
    }

    /// Whether the host has even one family to offer. A host with no enumerable fonts still letters
    /// (the engine falls back), but the editor says so instead of showing an empty dropdown.
    pub fn is_empty(&self) -> bool {
        self.families.is_empty()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn doc_json() -> Value {
        json!({
            "type": "result", "id": 1, "ok": true,
            "width": 2000, "height": 3000, "resolution": 300,
            "activeLayer": 7, "selectedLayers": [4, 7], "hasSelection": true,
            "selectionBounds": [100, 50, 200, 160], "revision": 12,
            "layers": [
                {"id": 7, "name": "Balloon", "kind": "Type", "selected": true},
                {"id": 6, "name": "Group", "kind": "Group", "children": [
                    {"id": 4, "name": "SFX", "kind": "Type", "selected": true},
                    {"id": 3, "name": "Shading", "kind": "Raster"}]},
            ],
        })
    }

    #[test]
    fn a_document_answer_becomes_the_document() {
        let d = Doc::from_json(&doc_json()).unwrap();
        assert_eq!((d.width, d.height, d.dpi), (2000.0, 3000.0, 300.0));
        assert_eq!(d.active_layer, Some(7));
        assert_eq!(d.selected, vec![4, 7]);
        assert_eq!(d.selection, Some([100.0, 50.0, 200.0, 160.0]));
        assert_eq!(d.revision, 12);
        assert_eq!(d.layers.len(), 4, "groups and their children are flattened in");
        assert!(d.is_type(4) && d.is_type(7));
        assert!(!d.is_type(3) && !d.is_type(6));
        assert_eq!(d.name_of(4), "SFX");
        assert_eq!(d.name_of(99), "layer 99");
    }

    #[test]
    fn an_action_takes_the_selected_type_layers_or_the_active_one() {
        let d = Doc::from_json(&doc_json()).unwrap();
        assert_eq!(d.target_layers(), vec![4, 7], "both selected type layers, in document order");
        // A selection with no type in it falls back to the active layer — but only if it is type.
        let mut only_raster = doc_json();
        only_raster["selectedLayers"] = json!([3]);
        let d = Doc::from_json(&only_raster).unwrap();
        assert_eq!(d.target_layers(), vec![7], "the active layer");
        only_raster["activeLayer"] = json!(3);
        let d = Doc::from_json(&only_raster).unwrap();
        assert!(d.target_layers().is_empty(), "a raster layer is not something to letter");
    }

    #[test]
    fn there_is_no_document_when_the_host_says_so() {
        assert_eq!(Doc::from_json(&Value::Null), None);
        assert_eq!(Doc::from_json(&json!("nothing")), None);
        // A document with no selection says so, and one that cannot be measured is not guessed at.
        let d = Doc::from_json(&json!({"hasSelection": false, "selectionBounds": null})).unwrap();
        assert_eq!(d.selection, None);
        assert_eq!(d.dpi, 72.0, "a missing resolution falls back rather than dividing by zero");
        let d = Doc::from_json(&json!({"hasSelection": true})).unwrap();
        assert_eq!(d.selection, None);
        let d = Doc::from_json(&json!({"resolution": 0, "selectionBounds": [1, 2, 3]})).unwrap();
        assert_eq!(d.dpi, 72.0);
        assert_eq!(d.selection, None, "bounds that are not four numbers are no bounds");
    }

    fn info_json() -> Value {
        json!({
            "type": "result", "id": 2, "ok": true,
            "layer": 7, "text": "I'll be home by six.",
            "shape": {"Box": {"x": 0, "y": 0, "width": 162, "height": 96}},
            "transform": [1, 0, 0, 1, 110, 60],
            "antialias": "Smooth", "orientation": "Horizontal",
            "runs": [{"start": 0, "end": 20, "style": {"size_pt": 14.0, "font_family": "Inter"}}],
            "bounds": [110, 60, 272, 96],
        })
    }

    #[test]
    fn a_type_answer_becomes_a_measurement() {
        let m = Measured::of(&info_json());
        assert_eq!(m.text, "I'll be home by six.");
        assert!(!m.point);
        assert_eq!(m.size_pt, 14.0);
        assert_eq!(m.bounds, Some(Rect::new(110.0, 60.0, 162.0, 36.0)));
        // The box is in text space (0, 0) and the transform maps it onto the canvas: origin at
        // (110, 60), and an unrotated layer keeps its width and height.
        assert_eq!(m.box_rect, Some(Rect::new(110.0, 60.0, 162.0, 96.0)));
        assert_eq!(m.centre(), Some([191.0, 78.0]));
        assert_eq!(m.antialias.as_deref(), Some("Smooth"));
    }

    #[test]
    fn point_text_has_no_box_and_missing_measurements_are_not_invented() {
        let m = Measured::of(&json!({"shape": "Point", "runs": [], "bounds": null}));
        assert!(m.point);
        assert_eq!(m.box_rect, None);
        assert_eq!(m.bounds, None);
        assert_eq!(m.size_pt, crate::geom::FALLBACK_SIZE_PT);
        assert_eq!(m.centre(), None);
        // A scaled transform scales the box with it.
        let m = Measured::of(&json!({"shape": {"Box": {"x": 0, "y": 0, "width": 100, "height": 50}}, "transform": [2, 0, 0, 2, 10, 20]}));
        assert_eq!(m.box_rect, Some(Rect::new(10.0, 20.0, 200.0, 100.0)));
        // A transform that is not six numbers is not used to invent a box.
        let m = Measured::of(&json!({"shape": {"Box": {"x": 0, "y": 0, "width": 1, "height": 1}}, "transform": [1, 0]}));
        assert_eq!(m.box_rect, None);
    }

    #[test]
    fn the_font_list_is_whatever_the_host_has() {
        let mut f = Fonts::default();
        assert!(f.is_empty());
        f.absorb(&json!({"families": ["Inter", "DejaVu Sans", 7]}));
        assert_eq!(f.families, vec!["Inter", "DejaVu Sans"]);
        assert!(!f.is_empty());
        f.absorb(&json!({"faces": [{"family": "Inter", "style": "Regular"}, {"family": "Inter", "style": "Bold"}]}));
        assert_eq!(f.face_family, "Inter");
        assert_eq!(f.faces, vec!["Regular", "Bold"]);
        // An empty answer is not a reset: the family list stays.
        f.absorb(&json!({}));
        assert_eq!(f.families.len(), 2);
    }
}
