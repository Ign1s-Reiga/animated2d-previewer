//! The default motion and handovers a real bundle's controller yields.
//!
//! The unit tests cover each rule on hand-built controllers; only a real
//! bundle says the shape they assume (one default state, every other motion
//! falling back to it at the end of its clip) is the one exports have.
//!
//! The asset is never committed (spec §11). `A2D_FIXTURE_CUBISM` names one
//! bundle file (not a directory):
//!
//! ```bash
//! A2D_FIXTURE_CUBISM=/path/to/bundle cargo test -p a2d-cli --test real_handover -- --ignored --nocapture
//! ```

use a2d_core::{AnimatedModel, LoadReport};

#[test]
#[ignore = "needs a real asset; set A2D_FIXTURE_CUBISM to a Cubism Unity AssetBundle"]
fn a_real_bundle_gives_a_default_motion_and_a_handover_back_to_it_for_every_other_motion() {
    let Ok(path) = std::env::var("A2D_FIXTURE_CUBISM") else { return };
    let bytes = std::fs::read(&path).unwrap_or_else(|e| panic!("A2D_FIXTURE_CUBISM points at {path:?}: {e}"));
    let mut report = LoadReport::new();
    let package = a2d_import::import_bundle(&bytes, &mut report).expect("the bundle should import");
    let ir = package.model.as_cubism().expect("a Cubism package");

    let default = ir.default_motion.as_deref().expect("the controller's default state should name a motion");
    assert!(ir.motions.iter().any(|m| m.name == default), "the default {default:?} names no motion");

    println!("default: {default}; {} motions, {} handovers", ir.motions.len(), ir.handovers.len());
    let mut missing = Vec::new();
    for motion in ir.motions.iter().filter(|m| m.name != default) {
        let Some(h) = ir.handovers.iter().find(|h| h.from == motion.name) else {
            missing.push(motion.name.clone());
            continue;
        };
        assert_eq!(h.handover.animation, default, "{} hands over to something else", motion.name);
        assert!((h.handover.mix - 0.25).abs() < 0.01, "{}: mix {}", motion.name, h.handover.mix);
        let expected = motion.duration - h.handover.mix;
        assert!(
            (h.handover.at - expected).abs() < 0.01,
            "{}: at {} but the clip ends at {} with a {} mix",
            motion.name,
            h.handover.at,
            motion.duration,
            h.handover.mix
        );
        assert!(h.handover.at <= motion.duration);
    }
    assert!(missing.is_empty(), "motions with no handover: {missing:?}");
    assert!(ir.handovers.iter().all(|h| h.from != default), "the default must not hand over");

    // Playback applies the handovers, so neither loading nor the manifest says
    // they are stored but unused.
    assert!(!ir.handovers.is_empty(), "the model has other motions, so some handover must exist");
    let model = a2d_cubism::GenericCubismModel::load(ir.clone(), "handover");
    let mut loaded = LoadReport::new();
    model.absorb_degradations(&mut loaded);
    let text = loaded.to_string();
    assert!(!text.contains("does not apply them yet"), "{text}");
    assert!(
        !package.manifest.recorded_at_import().iter().any(|w| w.contains("does not apply them yet")),
        "the stale note must not appear in the manifest"
    );
    println!("import notes:\n{report}\nload notes:\n{text}");
}
