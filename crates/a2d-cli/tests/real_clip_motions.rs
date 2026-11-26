//! Recovers motions from the `AnimationClip`s of a real Unity bundle.
//!
//! Real bundles keep each motion only as a compressed clip. The synthetic tests
//! prove the decoder and its test writer agree; only a real bundle says the
//! layout, the hashes and the curve encoding are the ones the engine writes.
//!
//! The asset is never committed (spec §11). `A2D_FIXTURE_CUBISM` names one
//! bundle file (not a directory):
//!
//! ```bash
//! A2D_FIXTURE_CUBISM=/path/to/bundle cargo test -p a2d-cli --test real_clip_motions -- --ignored --nocapture
//! ```

use a2d_core::LoadReport;
use a2d_pack::Package;
use a2d_unity::{AnimationClip, Bundle, ClassId, SerializedFile};

#[path = "support/mod.rs"]
mod support;

fn bundle_bytes() -> Option<Vec<u8>> {
    let path = std::env::var("A2D_FIXTURE_CUBISM").ok()?;
    Some(std::fs::read(&path).unwrap_or_else(|e| panic!("A2D_FIXTURE_CUBISM points at {path:?}: {e}")))
}

fn serialized(bytes: &[u8]) -> SerializedFile {
    let bundle = Bundle::parse(bytes).expect("the bundle should parse");
    let node = bundle.nodes.iter().find(|n| n.is_serialized()).expect("the bundle should hold a serialized file");
    SerializedFile::parse(bundle.node_data(node).expect("the node should read")).expect("the file should parse")
}

#[test]
#[ignore = "needs a real asset; set A2D_FIXTURE_CUBISM to a Cubism Unity AssetBundle"]
fn every_animation_clip_in_a_real_bundle_parses() {
    let Some(bytes) = bundle_bytes() else { return };
    let file = serialized(&bytes);
    let mut clips = 0;
    for object in file.objects.iter().filter(|o| o.class_id == ClassId::ANIMATION_CLIP) {
        let data = file.object_data(object).expect("the object should read");
        let clip = AnimationClip::parse(data).unwrap_or_else(|e| panic!("clip object {}: {e}", object.path_id));
        assert_eq!(
            clip.streamed_curve_count() + clip.dense.curve_count as usize + clip.constants.len(),
            clip.bindings.len()
        );
        println!(
            "clip {:?}: {} streamed, {} dense, {} constant curves, {:.3}s",
            clip.name,
            clip.streamed_curve_count(),
            clip.dense.curve_count,
            clip.constants.len(),
            clip.stop_time - clip.start_time
        );
        clips += 1;
    }
    assert!(clips > 0, "the bundle holds no AnimationClip at all");
}

#[test]
#[ignore = "needs a real asset; set A2D_FIXTURE_CUBISM to a Cubism Unity AssetBundle"]
fn clips_become_motions_that_target_real_parameters_and_survive_a_package() {
    let Some(bytes) = bundle_bytes() else { return };
    let file = serialized(&bytes);
    let clip_count = file.objects.iter().filter(|o| o.class_id == ClassId::ANIMATION_CLIP).count();

    let mut report = LoadReport::new();
    let inventory = a2d_import::inspect_bundle(&bytes, &mut LoadReport::new()).expect("the bundle should inventory");
    let package = a2d_import::import_bundle(&bytes, &mut report).expect("the bundle should reconstruct");
    let ir = package.model.as_cubism().expect("a Cubism package");

    println!("{} clips, {} fade sources, {} motions", clip_count, inventory.fade_sources.len(), ir.motions.len());
    println!("{report}");

    if inventory.fade_sources.is_empty() {
        assert_eq!(ir.motions.len(), clip_count, "a bundle with no fade data yields one motion per clip:\n{report}");
    } else {
        assert!(!ir.motions.is_empty());
    }

    // A binding that matches no parameter is information only when its path
    // names no object of the prefab (the source's Animator binds nothing for
    // it); one that names a real object would still fail `validate` below.
    let absent = report
        .warnings()
        .iter()
        .filter(|w| matches!(w, a2d_core::Degradation::Info(t) if t.contains("bound to objects the prefab does not contain")))
        .count();
    println!("{absent} motion(s) with curves bound to objects the prefab does not contain");
    assert!(
        !report.to_string().contains("match no parameter in this model"),
        "a binding names an existing object that is not a parameter:\n{report}"
    );

    // Every curve drives a parameter the MOC3 actually has.
    assert!(ir.unresolved_motion_targets().is_empty(), "a curve targets an id the model lacks");

    for m in &ir.motions {
        assert!(m.duration.is_finite() && m.duration > 0.0, "{} has duration {}", m.name, m.duration);
        let mut last = 0.0f32;
        for c in &m.curves {
            assert!(!c.keys.is_empty(), "{}/{} has no keys", m.name, c.target);
            for w in c.keys.windows(2) {
                assert!(w[0].time <= w[1].time, "{}/{} keys are out of order", m.name, c.target);
            }
            for k in &c.keys {
                assert!(
                    k.time.is_finite() && k.value.is_finite() && k.in_slope.is_finite() && k.out_slope.is_finite(),
                    "{}/{} holds a non-finite key",
                    m.name,
                    c.target
                );
                last = last.max(k.time);
            }
        }
        assert!(m.duration >= last, "{} lasts {}s but has a key at {last}s", m.name, m.duration);
        println!("  {:<32} {:7.3}s  {:3} curves", m.name, m.duration, m.curves.len());
    }

    // Through the filesystem and back, then the package validation the
    // `validate` command runs. Entries recorded at import (repeated as
    // `recorded at import:`; the informational notes are kept apart and never
    // reach `validate`) and the repeat notes for inferred looping parameters are
    // expected; anything else would be a real problem with the package.
    let dir = std::env::temp_dir().join(format!("a2d-clip-motions-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    package.write_to(&dir).expect("the package should write");
    let reloaded = Package::read_from(&dir).expect("the package should read back");
    let _ = std::fs::remove_dir_all(&dir);
    assert_eq!(reloaded.model.as_cubism().expect("cubism").motions, ir.motions, "motions must survive a round trip");
    let problems = support::unexpected_validate_warnings(&reloaded);
    assert!(problems.is_empty(), "validate reported: {problems:#?}");
}
