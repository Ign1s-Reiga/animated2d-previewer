//! Decodes the `AnimatorController`s of a real Unity bundle.
//!
//! The synthetic tests prove the decoder and its test writer agree; only a real
//! bundle says the layout is the one the engine writes. The decoder is strict
//! (exact final byte), so a successful decode is itself the main evidence.
//!
//! The asset is never committed (spec §11). `A2D_FIXTURE_CUBISM` names one
//! bundle file (not a directory):
//!
//! ```bash
//! A2D_FIXTURE_CUBISM=/path/to/bundle cargo test -p a2d-cli --test real_animator_controller -- --ignored --nocapture
//! ```

use a2d_unity::{animator_controller_pptr, AnimationClip, AnimatorController, Bundle, ClassId, SerializedFile};

#[path = "support/mod.rs"]
mod support;

fn serialized() -> Option<SerializedFile> {
    let path = std::env::var("A2D_FIXTURE_CUBISM").ok()?;
    let bytes = std::fs::read(&path).unwrap_or_else(|e| panic!("A2D_FIXTURE_CUBISM points at {path:?}: {e}"));
    let bundle = Bundle::parse(&bytes).expect("the bundle should parse");
    let node = bundle.nodes.iter().find(|n| n.is_serialized()).expect("the bundle should hold a serialized file");
    Some(SerializedFile::parse(bundle.node_data(node).expect("the node should read")).expect("the file should parse"))
}

#[test]
#[ignore = "needs a real asset; set A2D_FIXTURE_CUBISM to a Cubism Unity AssetBundle"]
fn every_animator_controller_in_a_real_bundle_decodes_and_resolves() {
    let Some(file) = serialized() else { return };
    let mut controllers = 0;
    for object in file.objects.iter().filter(|o| o.class_id == ClassId::ANIMATOR_CONTROLLER) {
        let data = file.object_data(object).expect("the object should read");
        let c = AnimatorController::parse(data).unwrap_or_else(|e| panic!("controller object {}: {e}", object.path_id));
        let problems = c.unresolved_references();
        assert!(problems.is_empty(), "unresolved references: {problems:?}");

        // Each clip, decoded, with its name and length.
        let clips: Vec<(String, f32)> = c
            .clips
            .iter()
            .map(|p| {
                assert!(p.is_local(), "a clip pointer leaves the file: {p:?}");
                let target = file.find_object(p.path_id).expect("the clip pointer should resolve");
                assert_eq!(target.class_id, ClassId::ANIMATION_CLIP);
                let clip = AnimationClip::parse(file.object_data(target).expect("the clip should read"))
                    .expect("the clip should parse");
                (clip.name.clone(), clip.stop_time - clip.start_time)
            })
            .collect();

        let (mut states, mut transitions, mut conditions, mut leaves, mut unnamed) = (0, 0, 0, 0, 0);
        let (mut named_like_clip, mut exit_matches, mut exit_checked) = (0, 0, 0);
        for sm in &c.state_machines {
            // Any-state transitions belong to the machine, not to a state.
            for t in &sm.any_state_transitions {
                transitions += 1;
                conditions += t.conditions.len();
            }
            for s in &sm.states {
                states += 1;
                let state_name = c.name(s.name_id);
                if state_name.is_none() {
                    unnamed += 1;
                }
                assert!(s.speed.is_finite() && s.speed != 0.0, "state speed {}", s.speed);
                assert!(s.cycle_offset.is_finite());
                let own_clips: Vec<u32> = s.leaf_clip_indices().collect();
                leaves += own_clips.len();
                for &clip in &own_clips {
                    assert_eq!(
                        Some(clips[clip as usize].0.as_str()),
                        state_name,
                        "a state plays a clip whose name differs from its own"
                    );
                    named_like_clip += 1;
                }
                for t in &s.transitions {
                    transitions += 1;
                    conditions += t.conditions.len();
                    if c.name(t.full_path_id).is_none() {
                        unnamed += 1;
                    }
                    assert!(t.duration.is_finite() && t.duration >= 0.0, "duration {}", t.duration);
                    assert!(t.offset.is_finite() && (0.0..=1.0).contains(&t.offset), "offset {}", t.offset);
                    assert!(t.exit_time.is_finite(), "exit time {}", t.exit_time);
                    if let (&[clip], true) = (own_clips.as_slice(), t.has_fixed_duration) {
                        let length = clips[clip as usize].1;
                        let predicted = 1.0 - t.duration / length;
                        let ok = (t.exit_time - predicted).abs() < 1e-3;
                        exit_checked += 1;
                        exit_matches += usize::from(ok);
                        println!(
                            "  transition: exit_time {:.4}, 1 - duration/length {:.4} (duration {}, length {:.3}){}",
                            t.exit_time,
                            predicted,
                            t.duration,
                            length,
                            if ok { "" } else { "  <-- differs" }
                        );
                    }
                }
            }
        }
        assert_eq!(unnamed, 0, "{unnamed} state or transition ids are missing from m_TOS");
        assert!(states > 0 && leaves > 0, "a controller with no state or no clip");
        println!(
            "controller: {} layers, {} machines, {} states, {} transitions ({} conditions), {} clip leaves of {} clips, \
             {} parameters, {} names; {}/{} leaf clips match their state name; {}/{} exit times equal 1 - duration/length",
            c.layers.len(),
            c.state_machines.len(),
            states,
            transitions,
            conditions,
            leaves,
            c.clips.len(),
            c.parameters.len(),
            c.names.len(),
            named_like_clip,
            leaves,
            exit_matches,
            exit_checked,
        );
        controllers += 1;
    }
    assert!(controllers > 0, "the bundle holds no AnimatorController at all");
}

#[test]
#[ignore = "needs a real asset; set A2D_FIXTURE_CUBISM to a Cubism Unity AssetBundle"]
fn every_animator_names_a_controller_in_the_bundle() {
    let Some(file) = serialized() else { return };
    let mut animators = 0;
    for object in file.objects.iter().filter(|o| o.class_id == ClassId::ANIMATOR) {
        let data = file.object_data(object).expect("the object should read");
        let ptr = animator_controller_pptr(data).expect("the Animator should read");
        assert!(ptr.is_local(), "the Animator's controller is {ptr:?}");
        let target = file.find_object(ptr.path_id).expect("the controller pointer should resolve");
        assert_eq!(target.class_id, ClassId::ANIMATOR_CONTROLLER);
        animators += 1;
    }
    println!("{animators} animators, each naming a controller of this bundle");
}
