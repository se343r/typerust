//! The five actions, as state machines: Create, Insert, Align, Resize and Apply.
//!
//! A panel cannot run a command and read its answer in one go — the host hands the answer back as a
//! `result` event — so every action is a little machine: emit one operation, feed the answer in,
//! emit the next. That is exactly the shape TyperTools' ExtendScript had (each of its functions
//! calls into Photoshop, reads the layer back, calls in again), so the sequences below are the same
//! sequences, written for a host that answers asynchronously.
//!
//! Every sequence is one **undo step**: all of an action's commands share one `coalesce` key, which
//! the engine turns into a single history entry (`type_cmds`' own tests pin that for `type.create`
//! plus two `type.edit`s). A run over several layers is still one step, which is what a letterer
//! means by "insert this line" — one thing to undo.
//!
//! The geometry itself is in [`crate::geom`]; what is here is the order the calls go in, which is
//! the part that has to match TypeR 2.5.0 (`app_src/src/host.js`, `typer.jsx`). The sequences are
//! TyperTools 1.4.9's with TypeR's additions: Align leaves the box alone and only moves the layer
//! when `resizeTextBoxOnCenter` is off, and a markdown script applies its character runs in a
//! follow-up `type.edit` after the text is measured, because `type.create` takes no run list. Two
//! details are the reason the box exists at all:
//!
//! * a new block of type is given a box **fifteen times** taller than the balloon before the text is
//!   laid out in it, so the line does not wrap; afterwards the box is refitted around the measured
//!   text ([`crate::geom::Rect::refitted`]), and the layer is moved so the text's centre lands where
//!   the balloon's is. Refitting keeps the box's origin, so both can go in one `type.edit`;
//! * point text has no box, so every box step is skipped for it, and the move is measured the same
//!   way — which is why the port does not need TyperTools' "create as a box, then convert to point"
//!   dance.
//!
//! Nothing here touches the marquee. TyperTools and TypeR contract the selection by changing it
//! and, on Align, deselect at the end; the engine has no command a panel may run to put a selection
//! back, so the port reads the marquee, insets it by TypeR's `internalPadding` when measuring, and
//! leaves it alone. That is a divergence, it is deliberate, and it is the one a letterer will
//! notice — their marquee survives an action.

use std::collections::VecDeque;

use serde_json::{Value, json};

use crate::doc::Measured;
use crate::geom::{self, Rect};

/// What a run asks for next.
#[derive(Clone, Debug, PartialEq)]
pub enum Step {
    /// Ask the host for the document (`doc.info`): the marquee, and the resolution the sizes
    /// convert at.
    Doc,
    /// Run one engine command and feed the answer back in.
    Call { command: &'static str, params: Value },
    /// The run finished; the message says what happened.
    Done(String),
    /// The run could not be carried out; the message says why.
    Failed(String),
}

/// Which action is running.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Kind {
    /// A new type layer in the marquee, lettered with the current style.
    Create,
    /// The line's text into the layers the letterer selected, in the current style.
    Insert,
    /// The selected layers' own text re-shaped and centred in the marquee.
    Align,
    /// Every selected layer's type a step bigger or smaller, kept where it was.
    Resize,
    /// The current style onto the selected layers, their text untouched.
    Apply,
}

impl Kind {
    pub fn label(self) -> &'static str {
        match self {
            Kind::Create => "Create",
            Kind::Insert => "Insert",
            Kind::Align => "Align",
            Kind::Resize => "Resize",
            Kind::Apply => "Apply",
        }
    }

    /// Whether this kind applies a style, and therefore carries the style's anti-aliasing. Align only
    /// reshapes a layer and Resize only changes its size: neither may touch how it is rendered.
    fn styles(self) -> bool {
        matches!(self, Kind::Create | Kind::Insert | Kind::Apply)
    }
}

/// The steps of a run. `self.phase` names the step whose answer is pending — or, before the first
/// call, the step about to be emitted — and [`Run::advance`] is what moves it on.
const DOC: u8 = 0;
const MAKE: u8 = 1;
const INFO: u8 = 2;
const BOX: u8 = 3;
const STYLE: u8 = 4;
const FIT: u8 = 5;
/// Character runs laid over text that is already in the layer (markdown's bold and italic).
const RUNS: u8 = 6;
const END: u8 = 7;

/// One action, in flight.
#[derive(Clone, Debug)]
pub struct Run {
    pub kind: Kind,
    /// Every command of this run carries it, so the whole action is one history step.
    pub coalesce: String,
    /// The style being applied, for the report.
    pub style_name: String,
    /// What the type says (Create, Insert).
    pub text: String,
    /// The style's parameters for `type.create` and `type.setStyle`.
    pub params: Value,
    /// The `type.edit` anti-aliasing the style asks for.
    pub antialias: String,
    /// The size step, in points (Resize).
    pub delta: f64,
    /// Character runs to apply once the text is in the layer, as markdown's weights are. Empty
    /// unless the script's markdown setting is on and the line carries a marker.
    pub runs: Vec<Value>,
    /// TypeR's `resizeTextBoxOnCenter`: whether Align gives the layer the balloon's box as well as
    /// centring it, or only centres the layer it has.
    pub resize: bool,
    /// TypeR's `internalPadding`, in document pixels: taken off both sides of a new box's width.
    pub padding: f64,
    /// Keep each layer's own size when a style is applied (TyperTools' Ctrl+click).
    pub keep_size: bool,
    /// Make point text rather than paragraph text (TyperTools' "paste as point text" setting). Only
    /// Create reads it: every other kind works on a layer that already exists and is measured.
    pub point: bool,
    /// Layers still to be started.
    todo: VecDeque<u64>,
    /// The layer being worked on; `None` until Create has made one.
    layer: Option<u64>,
    phase: u8,
    /// How the layer looked before the change: where an action puts it back.
    before: Option<Measured>,
    /// The latest measurement.
    last: Option<Measured>,
    /// The box a step gave the layer, for the refit that follows it.
    box_set: Option<Rect>,
    /// The contracted marquee, for the kinds that use it.
    sel: Option<Rect>,
    /// Layers finished.
    done: usize,
    /// The size before and after (Resize), for the report.
    size_from: f64,
    size_to: f64,
    /// What the host said while it was making the layer (a missing font, for instance).
    warnings: Vec<String>,
}

impl Run {
    /// Starts a run over `layers`, in the order they are done. Create ignores them — it makes its own
    /// layer — and the others need at least one.
    pub fn begin(kind: Kind, coalesce: String, layers: Vec<u64>) -> Run {
        let mut todo: VecDeque<u64> = layers.into();
        let layer = todo.pop_front();
        let phase = match kind {
            // Create and Align start by reading the marquee and the document's resolution.
            Kind::Create | Kind::Align => DOC,
            Kind::Insert | Kind::Resize | Kind::Apply => INFO,
        };
        Run {
            kind,
            coalesce,
            style_name: String::new(),
            text: String::new(),
            params: json!({}),
            antialias: "smooth".into(),
            delta: 0.0,
            runs: Vec::new(),
            resize: false,
            padding: 0.0,
            keep_size: false,
            point: false,
            todo,
            layer,
            phase,
            before: None,
            last: None,
            box_set: None,
            sel: None,
            done: 0,
            size_from: 0.0,
            size_to: 0.0,
            warnings: Vec::new(),
        }
    }

    /// The next thing to do, given the answer to the last operation (`None` to start).
    pub fn step(&mut self, dpi: f64, answer: Option<&Value>) -> Step {
        if let Some(v) = answer {
            if v.get("ok").and_then(Value::as_bool) != Some(true) {
                return Step::Failed(failure(v));
            }
            if let Some(step) = self.advance(v) {
                return step;
            }
        }
        self.emit(dpi)
    }

    /// Reads a successful answer, moves the phase on, and returns `Some` when the run has ended.
    fn advance(&mut self, v: &Value) -> Option<Step> {
        match self.phase {
            // The marquee, contracted and checked, exactly as TyperTools does before it letters.
            DOC => match geom::checked_selection(crate::doc::selection_of(v)) {
                Ok(sel) => {
                    self.sel = Some(sel);
                    self.phase = if self.kind == Kind::Create { MAKE } else { INFO };
                }
                Err(refusal) => return Some(Step::Failed(refusal.message().to_string())),
            },
            // The new layer: its id, and whatever the host warned about while making it.
            MAKE => {
                let Some(id) = v.get("layer").and_then(Value::as_u64) else {
                    return Some(Step::Failed("the host made a type layer but did not say which".into()));
                };
                self.layer = Some(id);
                self.warnings =
                    v.get("warnings").and_then(Value::as_array).map(|w| w.iter().filter_map(Value::as_str).map(str::to_string).collect()).unwrap_or_default();
                self.phase = if self.runs.is_empty() { INFO } else { RUNS };
            }
            // A layer measured: what the fit needs, and which steps this layer has at all.
            INFO => {
                let m = Measured::of(v);
                if self.kind == Kind::Resize {
                    let to = (m.size_pt + self.delta).clamp(geom::MIN_SIZE_PT, geom::MAX_SIZE_PT);
                    if !m.text.is_empty() && (to - m.size_pt).abs() < f64::EPSILON {
                        return Some(Step::Failed(format!(
                            "{} is already at {} pt, which is as {} as type goes",
                            self.name(),
                            round(m.size_pt),
                            if self.delta > 0.0 { "large" } else { "small" }
                        )));
                    }
                    self.size_from = m.size_pt;
                    self.size_to = to;
                }
                let point = m.point;
                let boxed = m.box_rect.is_some();
                self.before = Some(m.clone());
                self.last = Some(m);
                self.phase = match self.kind {
                    // Create's box came from the layer it made; there is nothing left but the fit.
                    Kind::Create => FIT,
                    // Align gives the layer the balloon's box when TypeR's resize-on-centre is on,
                    // whatever box it had before — but point text has no box, and with the setting
                    // off Align leaves the box alone and only moves the layer.
                    Kind::Align if !point && self.resize => BOX,
                    Kind::Align => FIT,
                    // A style cannot be applied to a point layer's box: it has none.
                    Kind::Apply if point || !boxed => STYLE,
                    Kind::Apply => BOX,
                    Kind::Insert => BOX,
                    Kind::Resize => STYLE,
                };
            }
            // The box has been set (or the text replaced): the type has re-laid out, and this is the
            // measurement the fit moves from.
            BOX => {
                self.last = Some(Measured::of(v));
                self.phase = match self.kind {
                    Kind::Insert if !self.runs.is_empty() => RUNS,
                    Kind::Insert | Kind::Apply => STYLE,
                    _ => FIT,
                };
            }
            RUNS => {
                self.last = Some(Measured::of(v));
                self.phase = if self.kind == Kind::Create { INFO } else { STYLE };
            }
            STYLE => {
                self.last = Some(Measured::of(v));
                // Growing the type grows the box with it, so a bigger line does not start to wrap.
                // Shrinking leaves the box alone, as TyperTools does.
                let grow = self.kind == Kind::Resize && self.delta > 0.0 && self.before.as_ref().and_then(|m| m.box_rect).is_some();
                self.phase = if grow { BOX } else { FIT };
            }
            FIT => return self.next_layer(),
            _ => {}
        }
        None
    }

    /// Moves to the next layer, or ends the run when there is none.
    fn next_layer(&mut self) -> Option<Step> {
        self.done += 1;
        match self.todo.pop_front() {
            Some(id) => {
                self.layer = Some(id);
                self.before = None;
                self.last = None;
                self.box_set = None;
                self.phase = INFO;
                None
            }
            None => {
                self.phase = END;
                Some(Step::Done(self.report()))
            }
        }
    }

    /// The operation for the current step.
    fn emit(&mut self, dpi: f64) -> Step {
        match self.phase {
            END => Step::Done(self.report()),
            DOC => Step::Doc,
            MAKE => {
                let mut params = self.params.clone();
                let Some(o) = params.as_object_mut() else {
                    return Step::Failed("the style is not a set of parameters".into());
                };
                o.insert("text".into(), json!(self.text));
                // The whole action is one undo step, and this — the layer it makes — is the step the
                // letterer would most expect to be one on its own.
                o.insert("coalesce".into(), json!(self.coalesce));
                let Some(sel) = self.sel else {
                    return Step::Failed("there is no selection to letter into".into());
                };
                if self.point {
                    // Point text: created at the balloon's centre, and moved into place once it has
                    // been measured. There is no box to give it.
                    let c = sel.centre();
                    o.insert("x".into(), json!(c[0]));
                    o.insert("y".into(), json!(c[1]));
                } else {
                    // Paragraph text: far too tall on purpose, so the line does not wrap before
                    // there is a box to fit it to.
                    let rect = sel.box_for(self.padding);
                    self.box_set = Some(rect);
                    o.insert("box".into(), json!(rect.as_box()));
                }
                self.call("type.create", params)
            }
            INFO => self.call("type.info", json!({"layer": self.layer})),
            BOX => match self.kind {
                // The line itself, into the layer the letterer picked. Apply only reshapes: applying
                // a style must not quietly rewrite what a layer says.
                Kind::Insert => {
                    let mut params = json!({"layer": self.layer, "text": self.text, "coalesce": self.coalesce});
                    if let Some(b) = self.stretched() {
                        params["box"] = json!(b.as_box());
                        self.box_set = Some(b);
                    }
                    self.call("type.edit", params)
                }
                Kind::Apply => {
                    let Some(b) = self.stretched() else {
                        return Step::Failed("the host did not report the layer's text box".into());
                    };
                    self.box_set = Some(b);
                    self.call("type.edit", json!({"layer": self.layer, "box": b.as_box(), "coalesce": self.coalesce}))
                }
                // The balloon's own box: the type re-wraps in it and is fitted to it.
                Kind::Align => {
                    let Some(sel) = self.sel else {
                        return Step::Failed("there is no selection to align to".into());
                    };
                    let rect = sel.box_for(self.padding);
                    self.box_set = Some(rect);
                    self.call("type.edit", json!({"layer": self.layer, "box": rect.as_box(), "coalesce": self.coalesce}))
                }
                // The layer's own box, grown by the size step's factors.
                _ => {
                    let Some(rect) = self.before.as_ref().and_then(|m| m.box_rect) else {
                        return Step::Failed("the host did not report the layer's text box".into());
                    };
                    let grown = rect.grown();
                    self.box_set = Some(grown);
                    self.call("type.edit", json!({"layer": self.layer, "box": grown.as_box(), "coalesce": self.coalesce}))
                }
            },
            STYLE => {
                let mut params = if self.kind == Kind::Resize { json!({}) } else { self.params.clone() };
                if let Some(o) = params.as_object_mut() {
                    o.insert("layer".into(), json!(self.layer));
                    o.insert("coalesce".into(), json!(self.coalesce));
                    if self.kind == Kind::Resize {
                        o.insert("size".into(), json!(self.size_to));
                    } else if self.keep_size
                        && let Some(size) = self.last.as_ref().map(|m| m.size_pt)
                    {
                        // TyperTools' Ctrl+click in the styles list: the style, at the size the layer
                        // already had. TyperTools writes that size into the library as a side effect;
                        // this port leaves the style alone, which is the only difference.
                        o.insert("size".into(), json!(size));
                    }
                }
                self.call("type.setStyle", params)
            }
            RUNS => self.call("type.edit", json!({"layer": self.layer, "runs": self.runs, "coalesce": self.coalesce})),
            FIT => {
                let Some(m) = self.last.as_ref() else {
                    return Step::Failed("the host did not measure the layer".into());
                };
                let mut params = json!({"layer": self.layer, "coalesce": self.coalesce});
                // A step that changed the size keeps the box it grew (or shrank inside): the refit is
                // for the kinds that fit the box around type that has just been laid out in it.
                if self.kind != Kind::Resize
                    && let (Some(rect), Some(bounds)) = (self.box_set, m.bounds)
                {
                    let size_px = geom::pt_to_px(m.size_pt, dpi);
                    params["box"] = json!(rect.refitted(bounds.h, size_px).as_box());
                }
                // Create and Align put the type where the balloon is; the others put it back where it
                // was, because restyling or resizing a line must not move it.
                let target = match self.kind {
                    Kind::Create | Kind::Align => self.sel.map(Rect::centre),
                    _ => self.before.as_ref().and_then(|b| b.centre()),
                };
                if let (Some(target), Some(bounds)) = (target, m.bounds) {
                    let offset = geom::centre_offset(bounds, target);
                    // A move of nothing is not worth a parameter.
                    if offset != [0.0, 0.0] {
                        params["move"] = json!(offset);
                    }
                }
                // Anti-aliasing is a layer property in Adobe's model and a `type.edit` parameter
                // here, so a style's is set with the fit rather than with the style.
                if self.kind.styles() && !self.antialias.is_empty() {
                    params["antialias"] = json!(self.antialias);
                }
                self.call("type.edit", params)
            }
            _ => Step::Failed(format!("{} got stuck", self.kind.label())),
        }
    }

    fn call(&self, command: &'static str, params: Value) -> Step {
        Step::Call { command, params }
    }

    /// The box a layer's text is given before it is refitted: the box it has, stretched to fifteen
    /// times its height so the line lays out on one line. Point text has no box, so this is `None`.
    fn stretched(&self) -> Option<Rect> {
        if self.point_text() {
            return None;
        }
        let rect = self.before.as_ref().and_then(|m| m.box_rect)?;
        Some(Rect::new(rect.x, rect.y, rect.w, rect.h * geom::BOX_HEIGHT_FACTOR))
    }

    /// Whether the layer being worked on is point text, which has no box and no wrapping. The one
    /// the run is about to *make* is [`Run::point`], which is a setting rather than a measurement.
    fn point_text(&self) -> bool {
        self.before.as_ref().map(|m| m.point).unwrap_or(false)
    }

    fn name(&self) -> String {
        self.layer.map(|id| format!("layer {id}")).unwrap_or_else(|| "that layer".into())
    }

    /// What the letterer is told when the run finishes.
    fn report(&self) -> String {
        let layers = if self.done == 1 { "1 layer".to_string() } else { format!("{} layers", self.done) };
        let mut message = match self.kind {
            Kind::Create => format!("Placed “{}” in {}", short(&self.text), style_or(&self.style_name)),
            Kind::Insert => format!("Set “{}” into {layers}", short(&self.text)),
            Kind::Align => format!("Centred {layers} in the selection"),
            Kind::Resize => {
                let way = if self.delta > 0.0 { "up" } else { "down" };
                format!("Nudged {layers} {way}: {} → {} pt", round(self.size_from), round(self.size_to))
            }
            Kind::Apply => format!("Applied {} to {layers}", style_or(&self.style_name)),
        };
        for w in &self.warnings {
            message.push_str(". ");
            message.push_str(w);
        }
        message.push('.');
        message
    }
}

/// A style's name in a report, or a phrase for "no style".
fn style_or(name: &str) -> String {
    if name.is_empty() { "the current style".to_string() } else { format!("the style “{name}”") }
}

/// The first few words of a line, so a report does not run off the panel.
fn short(text: &str) -> String {
    let t = text.trim();
    if t.chars().count() <= 40 {
        return t.to_string();
    }
    let cut: String = t.chars().take(39).collect();
    format!("{cut}…")
}

fn round(v: f64) -> String {
    format!("{v:.1}")
}

/// Why an operation failed, as the host phrased it.
fn failure(v: &Value) -> String {
    match v.get("error") {
        Some(Value::String(e)) if !e.is_empty() => e.clone(),
        Some(Value::Null) | None => "the host did not say why it failed".to_string(),
        Some(other) => other.to_string(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// 300 dpi: the resolution the lettering corpora are drawn at.
    const DPI: f64 = 300.0;

    /// A `doc.info` answer with a 200 × 160 marquee, which contracts to 180 × 140 — the balloon case
    /// TyperTools was written for.
    fn doc_answer() -> Value {
        json!({"ok": true, "hasSelection": true, "selectionBounds": [100, 50, 200, 160], "resolution": DPI})
    }

    /// A `type.info`-shaped answer. `bounds` is `[x0, y0, x1, y1]`; `box` is the text box, or `None`
    /// for point text.
    fn info(text: &str, bounds: [f64; 4], size: f64, rect: Option<[f64; 4]>) -> Value {
        json!({
            "ok": true, "text": text,
            "shape": match rect { Some(b) => json!({"Box": {"x": 0, "y": 0, "width": b[2], "height": b[3]}}), None => json!("Point") },
            "transform": match rect { Some(b) => json!([1, 0, 0, 1, b[0], b[1]]), None => json!([1, 0, 0, 1, 0, 0]) },
            "runs": [{"style": {"size_pt": size}}],
            "bounds": bounds,
        })
    }

    fn create_run() -> Run {
        let mut r = Run::begin(Kind::Create, "tt1".into(), Vec::new());
        r.text = "I'll be home by six.".into();
        r.style_name = "Dialogue".into();
        r.params = json!({"font": "Inter", "size": 14.0, "align": "center"});
        r.antialias = "smooth".into();
        r
    }

    /// The command a step asks for.
    fn call(step: Step) -> (&'static str, Value) {
        match step {
            Step::Call { command, params } => (command, params),
            other => panic!("expected a call, got {other:?}"),
        }
    }

    #[test]
    fn create_makes_a_box_far_too_tall_and_then_fits_it() {
        let mut r = create_run();
        assert_eq!(r.step(DPI, None), Step::Doc);

        // 90 % of the contracted width, fifteen times its height — and the style's own font and size.
        let (command, params) = call(r.step(DPI, Some(&doc_answer())));
        assert_eq!(command, "type.create");
        assert_eq!(params["box"], json!([110.0, 60.0, 162.0, 2100.0]));
        assert_eq!(params["text"], json!("I'll be home by six."));
        assert_eq!(params["font"], json!("Inter"));
        assert_eq!(params["size"], json!(14.0));

        // The layer it made is then measured…
        let (command, params) = call(r.step(DPI, Some(&json!({"ok": true, "layer": 9}))));
        assert_eq!(command, "type.info");
        assert_eq!(params["layer"], json!(9));

        // …and the box is refitted around the text that laid out (two lines 30 px tall, 14 pt at
        // 300 dpi is 58.33 px, plus the 2 px pad), while the layer is moved so the type's centre
        // (191, 75) lands on the balloon's (200, 130).
        let measured = info("I'll be home by six.", [110.0, 60.0, 272.0, 90.0], 14.0, Some([110.0, 60.0, 162.0, 2100.0]));
        let (command, params) = call(r.step(DPI, Some(&measured)));
        assert_eq!(command, "type.edit");
        assert_eq!(params["layer"], json!(9));
        assert_eq!(params["antialias"], json!("smooth"));
        assert_eq!(params["coalesce"], json!("tt1"));
        assert_eq!(
            params["box"].as_array().map(|b| b[..3].to_vec()),
            Some(vec![json!(110.0), json!(60.0), json!(162.0)]),
            "the refit keeps the box's origin and width"
        );
        let h = 30.0 + 14.0 * DPI / 72.0 + 2.0;
        assert!((params["box"][3].as_f64().unwrap_or(0.0) - h).abs() < 1e-9, "{}", params["box"]);
        assert_eq!(params["move"], json!([9.0, 55.0]));

        assert_eq!(r.step(DPI, Some(&json!({"ok": true}))), Step::Done("Placed “I'll be home by six.” in the style “Dialogue”.".into()));
    }

    #[test]
    fn point_text_is_created_at_the_balloons_centre_and_moved_into_place() {
        let mut r = create_run();
        r.params = json!({"font": "Inter", "size": 14.0});
        r.point = true;
        assert_eq!(r.step(DPI, None), Step::Doc);
        let (command, params) = call(r.step(DPI, Some(&doc_answer())));
        assert_eq!(command, "type.create");
        assert_eq!(params["x"], json!(200.0), "the balloon's centre");
        assert_eq!(params["y"], json!(130.0));
        assert!(params.get("box").is_none(), "point text has no box");

        let (command, _params) = call(r.step(DPI, Some(&json!({"ok": true, "layer": 9}))));
        assert_eq!(command, "type.info");
        let (command, params) = call(r.step(DPI, Some(&info("hi", [300.0, 200.0, 340.0, 214.0], 14.0, None))));
        assert_eq!(command, "type.edit");
        assert!(params.get("box").is_none(), "there is no box to refit");
        assert_eq!(params["move"], json!([-120.0, -77.0]), "the type's centre (320, 207) to the balloon's (200, 130)");
        assert_eq!(params["antialias"], json!("smooth"));
    }

    #[test]
    fn a_small_or_missing_selection_is_refused_with_a_reason() {
        for (answer, want) in [
            (json!({"ok": true, "hasSelection": false}), "Select the balloon first"),
            (json!({"ok": true, "hasSelection": true, "selectionBounds": [0, 0, 30, 30]}), "too small to letter"),
        ] {
            let mut r = create_run();
            assert_eq!(r.step(DPI, None), Step::Doc);
            match r.step(DPI, Some(&answer)) {
                Step::Failed(why) => assert!(why.contains(want), "{why}"),
                other => panic!("{other:?}"),
            }
        }
    }

    #[test]
    fn a_missing_font_is_reported_with_the_line_that_was_placed() {
        let mut r = create_run();
        assert_eq!(r.step(DPI, None), Step::Doc);
        assert!(matches!(r.step(DPI, Some(&doc_answer())), Step::Call { command: "type.create", .. }));
        let made = json!({"ok": true, "layer": 9, "warnings": ["the font “Tahoma” is not installed"]});
        assert!(matches!(r.step(DPI, Some(&made)), Step::Call { command: "type.info", .. }));
        assert!(matches!(r.step(DPI, Some(&info("hi", [0.0, 0.0, 1.0, 1.0], 14.0, None))), Step::Call { command: "type.edit", .. }));
        match r.step(DPI, Some(&json!({"ok": true}))) {
            Step::Done(what) => assert!(what.ends_with("the style “Dialogue”. the font “Tahoma” is not installed."), "{what}"),
            other => panic!("{other:?}"),
        }
    }

    #[test]
    fn insert_sets_the_text_then_the_style_then_puts_the_layer_back() {
        let mut r = Run::begin(Kind::Insert, "tt2".into(), vec![4]);
        r.text = "You said that yesterday.".into();
        r.style_name = "Whisper".into();
        r.params = json!({"font": "Inter", "size": 12.0});
        r.antialias = "crisp".into();

        let (command, params) = call(r.step(DPI, None));
        assert_eq!((command, params["layer"].clone()), ("type.info", json!(4)));

        // The layer's own box is stretched fifteen times, so the new line lays out on one line.
        let before = info("old", [300.0, 400.0, 500.0, 430.0], 14.0, Some([300.0, 400.0, 200.0, 30.0]));
        let (command, params) = call(r.step(DPI, Some(&before)));
        assert_eq!(command, "type.edit");
        assert_eq!(params["text"], json!("You said that yesterday."));
        assert_eq!(params["box"], json!([300.0, 400.0, 200.0, 450.0]));
        assert_eq!(params["coalesce"], json!("tt2"));

        // Then the style, at the wrapped measurement.
        let wrapped = info("You said that yesterday.", [300.0, 400.0, 500.0, 460.0], 14.0, Some([300.0, 400.0, 200.0, 450.0]));
        let (command, params) = call(r.step(DPI, Some(&wrapped)));
        assert_eq!(command, "type.setStyle");
        assert_eq!((params["size"].clone(), params["layer"].clone()), (json!(12.0), json!(4)));
        assert!(params.get("antialias").is_none(), "anti-aliasing is set with the fit, not with the style");

        // Finally the fit: the box around the new text, and the layer back where it was. Its centre
        // was (400, 415) and the text now ends at (400, 445).
        let after = info("You said that yesterday.", [300.0, 400.0, 500.0, 490.0], 12.0, Some([300.0, 400.0, 200.0, 450.0]));
        let (command, params) = call(r.step(DPI, Some(&after)));
        assert_eq!(command, "type.edit");
        assert_eq!(params["antialias"], json!("crisp"));
        assert_eq!(params["move"], json!([0.0, -30.0]));
        let h = 90.0 + 12.0 * DPI / 72.0 + 2.0;
        assert!((params["box"][3].as_f64().unwrap_or(0.0) - h).abs() < 1e-9, "{}", params["box"]);

        match r.step(DPI, Some(&json!({"ok": true}))) {
            Step::Done(what) => assert!(what.starts_with("Set “You said that yesterday.” into 1 layer."), "{what}"),
            other => panic!("{other:?}"),
        }
    }

    #[test]
    fn align_shapes_each_layer_to_the_balloon_and_centres_it() {
        let mut r = Run::begin(Kind::Align, "tt3".into(), vec![4, 5]);
        r.resize = true;
        assert_eq!(r.step(DPI, None), Step::Doc);
        let (command, params) = call(r.step(DPI, Some(&doc_answer())));
        assert_eq!((command, params["layer"].clone()), ("type.info", json!(4)));

        // The balloon's box replaces whatever box the layer had.
        let before = info("hi", [0.0, 0.0, 40.0, 20.0], 14.0, Some([0.0, 0.0, 100.0, 20.0]));
        let (command, params) = call(r.step(DPI, Some(&before)));
        assert_eq!(command, "type.edit");
        assert_eq!(params["box"], json!([110.0, 60.0, 162.0, 2100.0]));

        let wrapped = info("hi", [110.0, 60.0, 272.0, 90.0], 14.0, Some([110.0, 60.0, 162.0, 2100.0]));
        let (command, params) = call(r.step(DPI, Some(&wrapped)));
        assert_eq!(command, "type.edit");
        assert_eq!(params["move"], json!([9.0, 55.0]));
        assert_eq!(params["layer"], json!(4));
        assert!(params.get("antialias").is_none(), "Align does not restyle the layer");

        // …and the second layer follows, then the run ends.
        let (command, params) = call(r.step(DPI, Some(&json!({"ok": true}))));
        assert_eq!((command, params["layer"].clone()), ("type.info", json!(5)));
        assert!(matches!(r.step(DPI, Some(&info("x", [0.0, 0.0, 1.0, 1.0], 14.0, None))), Step::Call { command: "type.edit", .. }));
        match r.step(DPI, Some(&json!({"ok": true}))) {
            Step::Done(what) => assert_eq!(what, "Centred 2 layers in the selection."),
            other => panic!("{other:?}"),
        }
    }

    #[test]
    fn align_with_resize_off_moves_the_layer_without_touching_its_box() {
        // TypeR's `resizeTextBoxOnCenter` defaults off: Align is then a centring move and nothing
        // else — the box the letterer set in Photoshop stays exactly as it was.
        let mut r = Run::begin(Kind::Align, "tt3b".into(), vec![4]);
        assert_eq!(r.step(DPI, None), Step::Doc);
        let (command, _) = call(r.step(DPI, Some(&doc_answer())));
        assert_eq!(command, "type.info");
        let before = info("hi", [0.0, 0.0, 40.0, 20.0], 14.0, Some([0.0, 0.0, 100.0, 20.0]));
        let (command, params) = call(r.step(DPI, Some(&before)));
        assert_eq!(command, "type.edit");
        assert!(params.get("box").is_none(), "the box is not resized: {}", params);
        assert_eq!(params["move"], json!([180.0, 120.0]));
    }

    #[test]
    fn resize_grows_the_box_with_the_type_and_keeps_the_line_where_it_was() {
        let mut r = Run::begin(Kind::Resize, "tt4".into(), vec![4]);
        r.delta = 0.24; // one document pixel: 0.24 pt at 300 dpi

        let before = info("I'll be home by six.", [300.0, 400.0, 500.0, 430.0], 14.0, Some([300.0, 400.0, 200.0, 30.0]));
        assert!(matches!(r.step(DPI, None), Step::Call { command: "type.info", .. }));
        let (command, params) = call(r.step(DPI, Some(&before)));
        assert_eq!(command, "type.setStyle");
        assert!((params["size"].as_f64().unwrap_or(0.0) - 14.24).abs() < 1e-9, "{}", params["size"]);
        assert!(params.get("font").is_none(), "only the size changes");

        // Growing a box grows its bottom and its right, from its own origin.
        let bigger = info("I'll be home by six.", [300.0, 400.0, 512.0, 434.0], 14.24, Some([300.0, 400.0, 200.0, 30.0]));
        let (command, params) = call(r.step(DPI, Some(&bigger)));
        assert_eq!(command, "type.edit");
        assert_eq!(params["box"], json!([300.0, 400.0, 212.0, 33.6]));
        assert!(params.get("antialias").is_none(), "Resize does not restyle the layer");

        // The centre was (400, 415) and is now (410, 420): back it goes. The box is left as it is —
        // it was grown deliberately, not fitted around text that had just been laid out.
        let grown = info("I'll be home by six.", [300.0, 400.0, 520.0, 440.0], 14.24, Some([300.0, 400.0, 212.0, 33.6]));
        let (command, params) = call(r.step(DPI, Some(&grown)));
        assert_eq!(command, "type.edit");
        assert_eq!(params["move"], json!([-10.0, -5.0]));
        assert!(params.get("box").is_none());

        match r.step(DPI, Some(&json!({"ok": true}))) {
            Step::Done(what) => assert_eq!(what, "Nudged 1 layer up: 14.0 → 14.2 pt."),
            other => panic!("{other:?}"),
        }
    }

    #[test]
    fn shrinking_point_text_leaves_its_shape_alone() {
        let mut r = Run::begin(Kind::Resize, "tt5".into(), vec![4]);
        r.delta = -0.24;
        let before = info("shout", [0.0, 0.0, 100.0, 40.0], 20.0, None);
        assert!(matches!(r.step(DPI, None), Step::Call { command: "type.info", .. }));
        let (command, params) = call(r.step(DPI, Some(&before)));
        assert_eq!(command, "type.setStyle");
        assert!((params["size"].as_f64().unwrap_or(0.0) - 19.76).abs() < 1e-9);
        // Point text: no box step, straight to the move — which has nothing to do.
        let after = info("shout", [0.0, 0.0, 100.0, 40.0], 19.76, None);
        let (command, params) = call(r.step(DPI, Some(&after)));
        assert_eq!(command, "type.edit");
        assert!(params.get("box").is_none() && params.get("move").is_none(), "{params}");
    }

    #[test]
    fn a_size_that_cannot_move_any_further_says_so() {
        let mut r = Run::begin(Kind::Resize, "tt6".into(), vec![4]);
        r.delta = 0.24;
        assert!(matches!(r.step(DPI, None), Step::Call { command: "type.info", .. }));
        match r.step(DPI, Some(&info("x", [0.0, 0.0, 1.0, 1.0], geom::MAX_SIZE_PT, None))) {
            Step::Failed(why) => assert!(why.contains("already at 1296.0 pt"), "{why}"),
            other => panic!("{other:?}"),
        }
    }

    #[test]
    fn apply_keeps_the_layer_where_it_is_and_can_keep_its_size() {
        let mut r = Run::begin(Kind::Apply, "tt7".into(), vec![4]);
        r.style_name = "Shout".into();
        r.params = json!({"font": "Inter", "size": 18.0});
        r.keep_size = true;
        r.antialias = "strong".into();

        let before = info("shout", [300.0, 400.0, 500.0, 430.0], 24.0, Some([300.0, 400.0, 200.0, 30.0]));
        assert!(matches!(r.step(DPI, None), Step::Call { command: "type.info", .. }));
        let (command, params) = call(r.step(DPI, Some(&before)));
        assert_eq!(command, "type.edit");
        assert_eq!(params["box"], json!([300.0, 400.0, 200.0, 450.0]));
        assert!(params.get("text").is_none(), "applying a style does not rewrite what the layer says");

        let (command, params) = call(r.step(DPI, Some(&info("shout", [300.0, 400.0, 500.0, 460.0], 24.0, Some([300.0, 400.0, 200.0, 450.0])))));
        assert_eq!(command, "type.setStyle");
        assert_eq!(params["size"], json!(24.0), "keeping the size is what Ctrl+click means");
        assert_eq!(params["font"], json!("Inter"));

        let (command, params) = call(r.step(DPI, Some(&info("shout", [300.0, 400.0, 500.0, 490.0], 24.0, Some([300.0, 400.0, 200.0, 450.0])))));
        assert_eq!((command, params["antialias"].clone()), ("type.edit", json!("strong")));
        assert_eq!(params["move"], json!([0.0, -30.0]));
    }

    #[test]
    fn applying_a_style_to_point_text_skips_the_box_entirely() {
        let mut r = Run::begin(Kind::Apply, "tt8".into(), vec![7]);
        r.style_name = "Thought".into();
        r.params = json!({"size": 10.0});
        assert!(matches!(r.step(DPI, None), Step::Call { command: "type.info", .. }));
        let (command, _) = call(r.step(DPI, Some(&info("hmm", [0.0, 0.0, 40.0, 20.0], 14.0, None))));
        assert_eq!(command, "type.setStyle", "no box to stretch, so no box edit");
    }

    #[test]
    fn a_failing_operation_ends_the_run_with_the_hosts_reason() {
        let mut r = create_run();
        assert_eq!(r.step(DPI, None), Step::Doc);
        match r.step(DPI, Some(&json!({"ok": false, "error": "the font “Tahoma” is not installed"}))) {
            Step::Failed(why) => assert_eq!(why, "the font “Tahoma” is not installed"),
            other => panic!("{other:?}"),
        }
        match r.step(DPI, Some(&json!({"ok": false}))) {
            Step::Failed(why) => assert!(why.contains("did not say why"), "{why}"),
            other => panic!("{other:?}"),
        }
        match r.step(DPI, Some(&json!({"ok": false, "error": {"code": 7}}))) {
            Step::Failed(why) => assert!(why.contains("code"), "{why}"),
            other => panic!("{other:?}"),
        }
    }

    #[test]
    fn a_report_shows_only_the_first_few_words_of_a_line() {
        assert_eq!(short("  hi  "), "hi");
        assert_eq!(short("x".repeat(40).as_str()).chars().count(), 40);
        assert_eq!(short("x".repeat(41).as_str()).chars().count(), 40);
        assert!(short("x".repeat(41).as_str()).ends_with('…'));
        assert_eq!(style_or(""), "the current style");
        assert_eq!(style_or("Dialogue"), "the style “Dialogue”");
    }
}
