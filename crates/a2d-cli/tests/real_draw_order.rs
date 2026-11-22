//! Checks the paint order of a real Cubism model.
//!
//! The rule under test: drawables are painted back to front by their blended
//! draw order rounded to an integer, ties broken by position in the model's
//! draw-order object list (MOC3 section 87).
//!
//! Two checks, because they fail differently:
//!
//! 1. **At rest, against the engine's own answer.** The Unity renderer
//!    component of each drawable stores the render order the official runtime
//!    computed when the prefab was built (the integer written just before the
//!    `_depthOffset` field, whose value is 1e-5). When those integers are a
//!    permutation of `0..n` the rule at default parameters must reproduce them
//!    exactly. When they are not, the prefab carries values from another
//!    revision of the model; that is printed and the comparison skipped, since
//!    nothing can be concluded from stale numbers.
//! 2. **In motion.** Every motion is played at 60 Hz through the model. Each
//!    frame's emitted meshes must have unique, ascending `z_order`, name every
//!    visible drawable at most once, and come out in exactly the order the
//!    rule gives. The count of frames that differ from the first frame's
//!    order is printed per motion, so a regression to a static order shows up.
//!
//! The renderer component is parsed here from the public `a2d-unity` API
//! (`Inventory::by_script`, `behaviour_owner_name`, `behaviour_body`); the
//! field position is found by the 1e-5 marker rather than a type tree, so the
//! parsing stays in this file rather than in a library.
//!
//! The asset is never committed (spec §11). `A2D_FIXTURE_CUBISM` names one
//! bundle file (not a directory):
//!
//! ```bash
//! A2D_FIXTURE_CUBISM=/path/to/bundle cargo test -p a2d-cli --test real_draw_order -- --ignored --nocapture
//! ```

use std::collections::HashMap;
use std::time::Duration;

use a2d_core::ir::cubism::CubismIr;
use a2d_core::{AnimatedModel, LoadReport, PlayOptions, RenderList};
use a2d_cubism::emit::paint_sequence;
use a2d_cubism::{CubismEval, GenericCubismModel, Pose};
use a2d_unity::{behaviour_body, behaviour_owner_name, Bundle, Inventory, SerializedFile};

const FPS: f32 = 60.0;
/// Below this opacity the emitter draws nothing.
const HIDDEN: f32 = 1.0 / 512.0;
/// The bit pattern of the `_depthOffset` field, 1e-5 as an `f32`.
const DEPTH_OFFSET_BITS: u32 = 0x3727_C5AC;

fn load() -> Option<(Vec<u8>, a2d_pack::Package, LoadReport)> {
    let path = std::env::var("A2D_FIXTURE_CUBISM").ok()?;
    let bytes = std::fs::read(&path).unwrap_or_else(|e| panic!("A2D_FIXTURE_CUBISM points at {path:?}: {e}"));
    let mut report = LoadReport::new();
    let package = a2d_import::import_bundle(&bytes, &mut report).expect("the bundle should reconstruct");
    Some((bytes, package, report))
}

/// The stored render order of every renderer component, keyed by the name of
/// the object it is attached to.
fn stored_render_orders(bytes: &[u8]) -> HashMap<String, i32> {
    let bundle = Bundle::parse(bytes).expect("bundle");
    let node = bundle.nodes.iter().find(|n| n.is_serialized()).expect("a serialized file");
    let file = SerializedFile::parse(bundle.node_data(node).expect("node data")).expect("serialized file");
    let inventory = Inventory::build(&file);
    let mut out = HashMap::new();
    for info in inventory.by_script("CubismRenderer") {
        let object = file.find_object(info.path_id).expect("the inventory names objects that exist");
        let owner = behaviour_owner_name(&file, object).expect("owner").unwrap_or_default();
        let (mut body, _) = behaviour_body(&file, object).expect("body");
        let rest = body.bytes(body.remaining()).expect("remaining bytes");
        let words: Vec<u32> = rest.chunks_exact(4).map(|c| u32::from_le_bytes([c[0], c[1], c[2], c[3]])).collect();
        let marker = words
            .iter()
            .position(|w| *w == DEPTH_OFFSET_BITS)
            .unwrap_or_else(|| panic!("renderer {owner:?}: no depth-offset field found"));
        assert!(marker >= 1, "renderer {owner:?}: nothing before the depth-offset field");
        let previous = out.insert(owner.clone(), words[marker - 1] as i32);
        assert!(previous.is_none(), "two renderers share an owner name");
    }
    out
}

fn ir_of(package: &a2d_pack::Package) -> &CubismIr {
    package.model.as_cubism().expect("a Cubism package")
}

#[test]
#[ignore = "needs a real asset; set A2D_FIXTURE_CUBISM to a Cubism Unity AssetBundle"]
fn the_import_reads_the_draw_order_groups_without_a_note() {
    let Some((_, package, report)) = load() else { return };
    let text = report.to_string();
    println!("{text}");
    assert!(!text.contains("draw-order groups of an unsupported shape"), "{text}");
    let ir = ir_of(&package);
    assert_eq!(ir.draw_order.len(), ir.drawables.len(), "the object list should name every drawable");
}

#[test]
#[ignore = "needs a real asset; set A2D_FIXTURE_CUBISM to a Cubism Unity AssetBundle"]
fn at_rest_the_paint_order_matches_the_render_orders_the_engine_stored() {
    let Some((bytes, package, _)) = load() else { return };
    let ir = ir_of(&package);
    let n = ir.drawables.len();
    let stored = stored_render_orders(&bytes);

    let mut by_drawable = Vec::with_capacity(n);
    for drawable in &ir.drawables {
        let order = *stored.get(&drawable.id).unwrap_or_else(|| {
            panic!("a drawable has no renderer component ({} renderers, {n} drawables)", stored.len())
        });
        by_drawable.push(order);
    }
    assert_eq!(stored.len(), n, "renderer components and drawables should pair one to one");

    let mut seen = vec![false; n];
    let permutation = by_drawable
        .iter()
        .all(|o| usize::try_from(*o).ok().filter(|o| *o < n).is_some_and(|o| !std::mem::replace(&mut seen[o], true)));
    if !permutation {
        let summary = format!(
            "stored render orders are stale ({n} drawables, stored range {}..={})",
            by_drawable.iter().min().copied().unwrap_or(0),
            by_drawable.iter().max().copied().unwrap_or(0)
        );
        // A skip must be a decision, not a silent pass: the comparison is the
        // point of this test, so it fails unless the caller says the prefab
        // is known to be stale.
        assert!(
            std::env::var("A2D_ALLOW_STALE_RENDER_ORDER").is_ok_and(|v| v == "1"),
            "{summary}; set A2D_ALLOW_STALE_RENDER_ORDER=1 to skip the comparison for a model known to be stale"
        );
        eprintln!("{summary}; skipped because A2D_ALLOW_STALE_RENDER_ORDER=1");
        return;
    }

    // Slot k of the stored data is where drawable i sits; the sequence the
    // rule produces lists drawables back to front, so invert it.
    let pose = ir.pose(&[]);
    let sequence = paint_sequence(&pose.draw_orders, &ir.draw_order);
    let mut mine = vec![0usize; n];
    for (position, drawable) in sequence.iter().enumerate() {
        mine[*drawable] = position;
    }
    let differing: Vec<usize> = (0..n).filter(|i| mine[*i] as i32 != by_drawable[*i]).collect();
    println!("{n} drawables; positions differing from the stored render order: {}", differing.len());
    assert!(
        differing.is_empty(),
        "{} of {n} drawables sit at a different position than the engine stored",
        differing.len()
    );
}

/// What each emitted mesh must be, in order: the visible drawables, back to front.
fn expected_visible(ir: &CubismIr, pose: &Pose, sequence: &[usize]) -> Vec<usize> {
    sequence
        .iter()
        .copied()
        .filter(|i| {
            let drawable = &ir.drawables[*i];
            pose.drawables.get(*i).is_some_and(|p| p.len() == drawable.uvs.len())
                && drawable.indices.len() >= 3
                && pose.opacities.get(*i).copied().unwrap_or(1.0) > HIDDEN
        })
        .collect()
}

#[test]
#[ignore = "needs a real asset; set A2D_FIXTURE_CUBISM to a Cubism Unity AssetBundle"]
fn every_motion_frame_paints_each_visible_drawable_once_in_the_rule_order() {
    let Some((_, package, _)) = load() else { return };
    let ir = ir_of(&package).clone();
    let n = ir.drawables.len();
    let mut model = GenericCubismModel::load(ir.clone(), "draw-order");
    let motions: Vec<(String, f32)> = model.animations().iter().map(|a| (a.name.clone(), a.duration)).collect();
    assert!(!motions.is_empty(), "the bundle needs at least one motion");

    // Position of each drawable in the object list, for the tie check.
    let mut listed = vec![usize::MAX; n];
    for (at, d) in ir.draw_order.iter().enumerate() {
        if let Some(slot) = listed.get_mut(*d as usize) {
            if *slot == usize::MAX {
                *slot = at;
            }
        }
    }

    let mut total_frames = 0usize;
    for (k, (name, duration)) in motions.iter().enumerate() {
        let label = format!("motion {k}");
        model.play_animation(name, PlayOptions::looping()).expect("motion should play");
        let frames = ((duration * FPS).floor() as usize).max(1);
        let mut first: Option<Vec<usize>> = None;
        let (mut differing, mut most_visible) = (0usize, 0usize);
        for frame in 0..frames {
            model.update(Duration::from_secs_f32(1.0 / FPS)).expect("update");
            let pose = model.pose();
            let sequence = paint_sequence(&pose.draw_orders, &ir.draw_order);

            // The sequence is a permutation, ascending in (rounded order, list position).
            let mut seen = vec![false; n];
            for i in &sequence {
                assert!(!std::mem::replace(&mut seen[*i], true), "{label} frame {frame}: drawable {i} painted twice");
            }
            assert_eq!(sequence.len(), n, "{label} frame {frame}");
            for pair in sequence.windows(2) {
                let key = |i: usize| (pose.draw_orders[i].round() as i32, listed[i]);
                assert!(key(pair[0]) <= key(pair[1]), "{label} frame {frame}: {pair:?} out of order");
            }

            // What the model emits is exactly the visible part of that sequence.
            let mut list = RenderList::new();
            model.emit(&mut list);
            let meshes = list.meshes();
            let z: Vec<u32> = meshes.iter().map(|m| m.z_order).collect();
            assert!(z.windows(2).all(|w| w[0] < w[1]), "{label} frame {frame}: z values are not unique and ascending");
            let candidates = expected_visible(&ir, pose, &sequence);
            assert!(meshes.len() <= candidates.len(), "{label} frame {frame}: more meshes than visible drawables");
            let mut at = 0;
            for mesh in meshes {
                let found = candidates[at..].iter().position(|i| {
                    let points = &pose.drawables[*i];
                    points.len() == mesh.vertices.len()
                        && points.iter().zip(&mesh.vertices).all(|(p, v)| p.0 == v.x && p.1 == v.y)
                });
                match found {
                    Some(offset) => at += offset + 1,
                    None => {
                        panic!("{label} frame {frame}: an emitted mesh is out of order or matches no visible drawable")
                    }
                }
            }
            most_visible = most_visible.max(meshes.len());

            match &first {
                None => first = Some(sequence),
                Some(f) => {
                    if *f != sequence {
                        differing += 1;
                    }
                }
            }
        }
        println!("  {label:<9} {frames} frames, {differing} differ from the first frame's order, up to {most_visible} meshes");
        total_frames += frames;
    }
    println!("{} motions, {total_frames} frames", motions.len());
}
