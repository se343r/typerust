//! Typerust — a manga and comic lettering panel for PhotoCraft (plug-in ABI v2).
//!
//! [TypeR](https://github.com/ScanR/TypeR) is a Photoshop CEP panel for letterers, and an enhanced,
//! maintained descendant of [TyperTools](https://github.com/Swirt/typertools-src): you paste a script,
//! one line of dialogue per row, tag each line with a style's prefix, and it sets the line into the
//! page in that style — then lets you align it to a balloon and size the type to fit. Typerust is a
//! port of it to PhotoCraft's own type engine, packaged as a real plug-in rather than a built-in
//! feature. Both upstreams are MIT, so the port reuses their source — the strings, the library JSON
//! shape, the geometry constants and the pane layout — with the notices in `NOTICE` and
//! `ATTRIBUTION.md`; the two things TypeR only bundles, the GPL `lib/jam` and Adobe's
//! `CSInterface.js`, stay out. Typerust is not affiliated with either project's authors.
//!
//! **Status.** The port is built from TypeR 2.5.0 as its reference. What is here today is the part
//! shared with TyperTools 1.4.9 plus TypeR's own features that need no host change: its three-pane
//! layout, nested folders and folder-scoped prefix priority, `//` inheritance, ignore tags, markdown
//! runs, RTL direction, internal padding, resize-on-centre, both size steps, quick style size, saved
//! states, and both export shapes. What remains — the stroke a style carries, auto-centring without a
//! selection and multi-bubble mode — needs a host allow-list row and is named in `README.md`.
//! Anything not implemented says so on screen rather than behaving differently in silence.
//!
//! The crate is two halves. `actions`, `doc`, `geom`, `library`, `props`, `script` and `view` are the
//! plug-in itself — pure functions and a state machine, with no host access of any kind, which is why
//! they can be tested as ordinary Rust. This file is the ABI: the exports the host looks for, and the
//! two-way JSON envelope around [`panel::Panel::handle`].
//!
//! ## Why this file is the way it is
//!
//! * **No imports, and no `unsafe` either.** The host refuses a module with an import of any kind
//!   ([`docs/plugins-v2.md`](https://github.com/storytold/photocraft/blob/main/docs/plugins-v2.md)),
//!   so everything the panel says crosses
//!   through `pc_event`, as JSON. The usual way to write that glue is `core::slice::from_raw_parts`
//!   over the pointers the host passes; this one keeps every live block in a table instead, so an
//!   event is read by *looking its address up* and copying the bytes out. No pointer is ever
//!   dereferenced, and the crate compiles under `unsafe_code = "forbid"` like the rest of PhotoCraft.
//! * **A panic is a stopped panel.** The host catches a trap and keeps the document — but the
//!   letterer's panel is gone until they restart it. Every entry point here is written not to fail:
//!   an event that is not JSON is ignored, an oversized view is replaced by a notice that says so,
//!   and an allocation that fails reports the ABI's "out of memory" rather than aborting.

#![deny(clippy::unwrap_used, clippy::expect_used, clippy::panic, clippy::unimplemented, clippy::todo, clippy::unreachable)]
// The modules below are ordinary Rust with ordinary tests: `unwrap` in a test is how a test says
// what it expects, and the ABI's own guards are about the shipped module, not the test binary.
#![cfg_attr(test, allow(clippy::unwrap_used, clippy::expect_used, clippy::panic, clippy::indexing_slicing, clippy::arithmetic_side_effects))]

mod actions;
mod doc;
mod geom;
mod library;
mod panel;
mod props;
mod script;
mod view;

use std::sync::{Mutex, MutexGuard, PoisonError};

use serde_json::Value;

use crate::panel::Panel;

/// The manifest, read once when the host loads the module. `panel` and `capabilities` are the ABI v2
/// additions: a dock-tab title and a minimum width, and the two things this panel needs to letter —
/// to see the document, and to add type layers to it. Everything is off unless declared, so the two
/// capabilities are the whole of what the host will let it do.
///
/// `minWidth` is both the width the host opens the panel at and the floor it resizes to, and a row of
/// framed buttons wraps rather than overflowing, so the Create/Insert/Align/A−/A+ row is one line only
/// because 420 is wide enough for it. Narrow the panel by hand and the host moves A−/A+ onto a second
/// line; that is the host's layout, not a fault in the view.
const MANIFEST: &str = r#"{
  "id": "org.photocraft.typerust",
  "name": "Typerust",
  "version": "0.1.0",
  "kind": "panel",
  "author": "PhotoCraft contributors",
  "description": "Manga and comic lettering: paste a script, tag each line with a style's prefix, and set it into the page in that style. A port of TypeR 2.5.0 for Adobe Photoshop (MIT, © 2017 Swirt) and of TyperTools 1.4.9 before it, to PhotoCraft's own type engine.",
  "panel": { "title": "Typerust", "minWidth": 420 },
  "capabilities": { "document": ["read", "addLayers"], "storage": true }
}"#;

/// The largest block the host may take from [`pc_alloc`]. Events are capped at the host's
/// `ops::MAX_OPS_BYTES` (1 MiB), so this is that with room to spare; a larger request is answered with
/// 0, which the host reads as "out of memory" rather than as a plug-in that talks nonsense.
const MAX_BLOCK_BYTES: usize = 4 * 1024 * 1024;

/// The host's own cap on one reply. A reply over it stops the panel, so [`answer`] checks first and
/// substitutes a notice (see [`TOO_LARGE`]).
const MAX_REPLY_BYTES: usize = 1024 * 1024;

/// The reply sent in place of a view that did not fit — the one thing a panel can still usefully say
/// when its own drawing is what went wrong.
const TOO_LARGE: &[u8] =
    br#"[{"op":"ui.notice","level":"error","text":"This page of the panel was too large to draw. Open Help and shorten the JSON, or use a shorter script."}]"#;

/// The panel instance. The host starts one and keeps it for the life of the process, and it never
/// calls back in while an event is being handled, so this one `Mutex` is the whole of the sharing.
static PANEL: Mutex<Option<Panel>> = Mutex::new(None);

/// The blocks the host is holding, by the address it was given.
///
/// A safe arena: [`alloc_block`] hands out the address of a `Vec<u8>`'s buffer and keeps the `Vec`
/// here, so the block stays alive and unmoved while the host writes into it. [`answer`] finds the
/// block by that address and copies the bytes out, and [`free_block`] drops it — which is why
/// nothing in this file needs a raw pointer. The host holds at most two blocks at a time (one event,
/// one reply), and frees both after every event, so this never grows.
static BLOCKS: Mutex<Vec<(usize, Vec<u8>)>> = Mutex::new(Vec::new());

/// The block table. A poisoned lock is taken anyway: a panel that answered nothing would be worse
/// than one that carries on with what it has.
fn blocks() -> MutexGuard<'static, Vec<(usize, Vec<u8>)>> {
    BLOCKS.lock().unwrap_or_else(PoisonError::into_inner)
}

// ---------------------------------------------------------------- the arena
//
// These are in the machine's own address width, and the `pc_*` exports below are the 32-bit ABI
// over them. The split is not decoration: a plug-in is a `wasm32` module, where a pointer *is* 32
// bits, and that is the only place the ABI's addresses can be a real pointer. Keeping the logic at
// the host machine's width is what lets the tests in this file drive the whole of it — a 64-bit
// test binary's own addresses do not fit in the ABI's fields, so driving the exports there would
// test the cast rather than the arena.

/// A block of `size` bytes, as the address the host may write into — or `None` when there is none to
/// give, which the ABI reports as "out of memory".
fn alloc_block(size: usize) -> Option<usize> {
    // The host never asks for zero bytes — a zero-byte allocation is not portable — but a block of
    // zero bytes has no address to hand back either, so one byte is asked for and none is used.
    let len = size.max(1);
    if len > MAX_BLOCK_BYTES {
        return None;
    }
    let mut bytes: Vec<u8> = Vec::new();
    // `try_reserve` rather than `vec![0; len]`: an allocation that cannot be met has to come back as
    // the ABI's "out of memory", not as a trap that stops the panel.
    if bytes.try_reserve_exact(len).is_err() {
        return None;
    }
    bytes.resize(len, 0);
    // The buffer does not move when the `Vec` does, so this stays the address of the block.
    let addr = bytes.as_ptr() as usize;
    blocks().push((addr, bytes));
    Some(addr)
}

/// Releases a block. The host frees the event block and the reply block after every event, so this
/// runs constantly. An address that is not in the table — one already freed, or the manifest, which
/// the host reads in place and never owns — is ignored: `blob.free`'s rule, that freeing something
/// twice is not an error.
fn free_block(addr: usize) {
    let mut known = blocks();
    if let Some(i) = known.iter().position(|(p, _)| *p == addr) {
        known.swap_remove(i);
    }
}

/// The event the host wrote into a block it took from [`alloc_block`]: found by its address, so no
/// pointer is followed.
fn event_bytes(addr: usize, len: usize) -> Option<Vec<u8>> {
    let known = blocks();
    let (_, bytes) = known.iter().find(|(p, _)| *p == addr)?;
    bytes.get(..len).map(<[u8]>::to_vec)
}

/// The operations to carry out, encoded as UTF-8 JSON — or `None` when there are none.
fn answer(addr: usize, len: usize) -> Option<Vec<u8>> {
    let event = event_bytes(addr, len)?;
    // An event that is not JSON is not something a panel can act on or even complain about usefully.
    // Ignoring it costs one frame; stopping the panel over it would cost the letterer their session.
    let event = serde_json::from_slice::<Value>(&event).ok()?;
    let ops = dispatch(&event);
    if ops.is_empty() {
        return None;
    }
    let reply = serde_json::to_vec(&Value::Array(ops)).ok()?;
    if reply.len() > MAX_REPLY_BYTES {
        return Some(TOO_LARGE.to_vec());
    }
    Some(reply)
}

/// Runs one event through the panel, starting the instance on the first one.
fn dispatch(event: &Value) -> Vec<Value> {
    let mut slot = PANEL.lock().unwrap_or_else(PoisonError::into_inner);
    match slot.as_mut() {
        Some(p) => p.handle(event),
        None => {
            let mut p = Panel::new();
            let ops = p.handle(event);
            *slot = Some(p);
            ops
        }
    }
}

// ---------------------------------------------------------------- the ABI

/// The ABI this module speaks: 2, the version that introduced panels.
#[unsafe(no_mangle)]
pub extern "C" fn pc_abi_version() -> u32 {
    2
}

/// `len << 32 | ptr` of the UTF-8 manifest JSON. It lives in read-only data, so the host reads it in
/// place and never asks for it to be freed.
///
/// The address is masked to the field's 32 bits: a plug-in is a `wasm32` module, where that is the
/// whole of a pointer, and the mask is what keeps the two fields from overlapping anywhere else.
#[unsafe(no_mangle)]
pub extern "C" fn pc_manifest() -> i64 {
    ((MANIFEST.len() as u64) << 32 | MANIFEST.as_ptr() as u64 & 0xffff_ffff) as i64
}

/// A block of `size` bytes for the host to write an event into, or 0 when there is none to give.
#[unsafe(no_mangle)]
pub extern "C" fn pc_alloc(size: u32) -> u32 {
    let Some(addr) = alloc_block(size as usize) else {
        return 0;
    };
    match u32::try_from(addr) {
        Ok(ptr) => ptr,
        // The ABI's addresses are 32-bit because a plug-in is a `wasm32` module. Anywhere else the
        // address does not fit, and a truncated one would point into the wrong place — so the block
        // is given back and the host told there is no memory.
        Err(_) => {
            free_block(addr);
            0
        }
    }
}

/// Releases a block. `blob.free`'s rule applies: freeing something twice is not an error.
#[unsafe(no_mangle)]
pub extern "C" fn pc_free(ptr: u32, _size: u32) {
    free_block(ptr as usize);
}

/// Answers one event: `len << 32 | ptr` of the operations JSON, or 0 for "no operations".
#[unsafe(no_mangle)]
pub extern "C" fn pc_event(ptr: u32, len: u32) -> i64 {
    let Some(reply) = answer(ptr as usize, len as usize) else {
        return 0;
    };
    let addr = reply.as_ptr() as usize;
    let Ok(rptr) = u32::try_from(addr) else {
        return 0;
    };
    let rlen = reply.len() as u64;
    blocks().push((addr, reply));
    ((rlen << 32) | u64::from(rptr)) as i64
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    /// One plug-in instance is one module, so the ABI's state is a single global. These tests run one
    /// at a time and each starts from a fresh panel and an empty block table.
    static TURN: Mutex<()> = Mutex::new(());

    fn fresh() -> MutexGuard<'static, ()> {
        let turn = TURN.lock().unwrap_or_else(PoisonError::into_inner);
        *PANEL.lock().unwrap_or_else(PoisonError::into_inner) = None;
        blocks().clear();
        turn
    }

    /// Takes a block the way the host does — allocate, write, hand it to the panel — and returns the
    /// reply bytes, freeing both blocks. A [`None`] from the panel means "no operations"; nothing in
    /// these tests is allowed to mean that.
    fn send(event: &str) -> Vec<u8> {
        let addr = alloc_block(event.len()).expect("the arena handed out a block");
        {
            let mut known = blocks();
            let (_, block) = known.iter_mut().find(|(p, _)| *p == addr).expect("the block is in the table");
            block[..event.len()].copy_from_slice(event.as_bytes());
        }
        let reply = answer(addr, event.len()).unwrap_or_else(|| panic!("the panel answered {event}"));
        free_block(addr);
        free_block(reply.as_ptr() as usize);
        reply
    }

    fn ops(event: &str) -> Vec<Value> {
        serde_json::from_slice(&send(event)).expect("the reply is a JSON array of operations")
    }

    /// The one `ui.view` in a reply, which every reply carries.
    fn view(reply: &[Value]) -> Value {
        reply.iter().find(|op| op["op"] == "ui.view").map(|op| op["view"].clone()).expect("the reply draws the panel")
    }

    const INIT: &str = r#"{"type":"init","now":1,"storage":{},"grants":{"commands":["type.create"]}}"#;

    #[test]
    fn the_abi_version_is_the_one_that_introduced_panels() {
        assert_eq!(pc_abi_version(), 2);
    }

    #[test]
    fn the_manifest_is_in_read_only_data_where_the_host_reads_it() {
        let packed = pc_manifest() as u64;
        let (ptr, len) = ((packed & 0xffff_ffff) as usize, (packed >> 32) as usize);
        assert_eq!(len, MANIFEST.len());
        // The address the ABI carries is 32 bits, so only a `wasm32` module — the only thing the host
        // loads — can check that it is the manifest's own address. Everywhere else the host-side
        // `abi.rs` test does it against the built fixture; here the JSON is what can be checked.
        if cfg!(target_pointer_width = "32") {
            assert_eq!(ptr, MANIFEST.as_ptr() as usize, "the manifest is read in place, not copied out");
        }
        let v: Value = serde_json::from_slice(MANIFEST.as_bytes()).expect("the manifest is JSON");
        assert_eq!(v["kind"], json!("panel"));
        assert_eq!(v["panel"]["title"], json!("Typerust"));
        assert_eq!(v["id"], json!("org.photocraft.typerust"), "its own id, since the rename from org.photocraft.typer");
        assert_eq!(v["capabilities"]["document"], json!(["read", "addLayers"]));
        assert_eq!(v["capabilities"]["storage"], json!(true));
        assert!(v["capabilities"]["network"].is_null(), "this panel asks for no network");
    }

    #[test]
    fn a_started_panel_draws_a_view_and_asks_what_is_open() {
        let _turn = fresh();
        let reply = ops(INIT);
        assert!(reply.iter().any(|op| op["op"] == "doc.info"), "it asks what is open");
        let view = view(&reply);
        assert_eq!(view["type"], "column");
        // Every widget in it has a type and, where it has an id, one the panel answers.
        assert!(view["children"].as_array().is_some_and(|c| c.len() > 4));
    }

    #[test]
    fn the_instance_survives_between_events() {
        // What makes a panel a panel rather than a filter: the script typed in one event is lettered
        // from the next, so the instance has to be the same one.
        let _turn = fresh();
        let _ = send(INIT);
        let _ = ops(r#"{"type":"ui.change","now":2,"id":"script","value":"x. BANG"}"#);
        let reply = ops(r#"{"type":"doc.changed","now":3,"doc":null}"#);
        assert!(view(&reply).to_string().contains("BANG"), "the script is still there");
    }

    #[test]
    fn a_fresh_instance_is_started_for_the_first_event_only() {
        let _turn = fresh();
        // The first event starts the instance; a panel that was never started has no state, and the
        // second event must not lay a second one down over the first.
        let _ = send(INIT);
        let _ = ops(r#"{"type":"ui.change","now":2,"id":"script","value":"d. One"}"#);
        let _ = send(r#"{"type":"init","now":3,"storage":{},"grants":{}}"#);
        // `init` does not wipe what is there: storage is read into the panel that exists.
        let reply = ops(r#"{"type":"doc.changed","now":4,"doc":null}"#);
        assert!(view(&reply).to_string().contains("One"), "one instance, not two");
    }

    #[test]
    fn an_event_that_is_not_json_does_not_stop_the_panel() {
        let _turn = fresh();
        let addr = alloc_block(3).expect("the block");
        {
            let mut known = blocks();
            let (_, block) = known.iter_mut().find(|(p, _)| *p == addr).expect("the block");
            block[..3].copy_from_slice(b"{{{");
        }
        assert!(answer(addr, 3).is_none(), "there is nothing useful to say about it, so: no operations");
        free_block(addr);
        assert!(ops(INIT).len() > 1, "and the panel is still working");
    }

    #[test]
    fn an_event_the_panel_does_not_know_is_answered_rather_than_ignored() {
        let _turn = fresh();
        let _ = send(INIT);
        // A host that grows a new event must not stop a panel that predates it: the reply says so in
        // the footer and draws the page as usual.
        let reply = ops(r#"{"type":"doc.something.new","now":2}"#);
        assert!(view(&reply).to_string().contains("does not know the event"));
    }

    #[test]
    fn freeing_a_block_twice_is_not_an_error() {
        let _turn = fresh();
        let addr = alloc_block(8).expect("the block");
        free_block(addr);
        free_block(addr);
        free_block(0);
        assert!(blocks().is_empty(), "the table does not leak");
    }

    #[test]
    fn an_absurd_allocation_is_refused_rather_than_aborting() {
        let _turn = fresh();
        assert_eq!(alloc_block(u32::MAX as usize), None, "over the cap: the ABI's out of memory, not a trap");
        assert!(alloc_block(0).is_some(), "a zero-byte request still gets an address to write none into");
        assert_eq!(blocks().len(), 1, "and only the one that was given out");
    }

    #[test]
    fn an_event_that_was_never_allocated_here_is_ignored() {
        let _turn = fresh();
        // An address the table does not know — a host that frees an event twice, or a reply's address
        // reused — must be a missing answer, never a read of whatever is at that address.
        assert!(answer(0xdead_beef, 16).is_none());
        assert!(answer(0, 0).is_none());
        assert!(blocks().is_empty());
    }

    #[test]
    fn every_block_the_host_takes_is_given_back() {
        let _turn = fresh();
        // The host allocates one event block and frees it and the reply after every event, so the
        // table is empty between events whatever the panel answered.
        for i in 0..16 {
            let _ = ops(if i == 0 { INIT } else { r#"{"type":"panel.visible","now":2,"visible":true}"# });
            assert_eq!(blocks().len(), 0, "after event {i}");
        }
    }
}
