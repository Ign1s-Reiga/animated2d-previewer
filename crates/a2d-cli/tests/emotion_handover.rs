//! An emotion picked in the viewer plays once and hands back as the source says,
//! on a real Cubism model built from synthetic data (no GPU, no game asset).

use std::time::Duration;

use a2d_core::ir::cubism::{Motion, MotionHandover};
use a2d_core::{Handover, LoadReport};
use a2d_desktop::{Config, LoadedModel, ViewerState};

#[path = "support/mod.rs"]
mod support;

const STEP: Duration = Duration::from_micros(62_500);

fn model() -> LoadedModel {
    let mut package = support::cubism_package(true);
    let a2d_pack::PackageModel::Cubism(ir) = &mut package.model else { panic!("a Cubism package") };
    ir.motions.clear();
    for (name, duration) in [("idle", 4.0), ("wave", 1.0), ("bow", 1.0)] {
        ir.motions.push(Motion { name: name.into(), duration, ..Motion::default() });
    }
    ir.default_motion = Some("idle".into());
    for (from, to, at) in [("wave", "idle", 0.75), ("bow", "wave", 0.5)] {
        ir.handovers
            .push(MotionHandover { from: from.into(), handover: Handover { animation: to.into(), at, mix: 0.25 } });
    }
    let cubism = a2d_cubism::GenericCubismModel::load(ir.clone(), "emotions");
    LoadedModel::from_parts(
        std::path::Path::new("emotions.a2dpack"),
        Box::new(cubism),
        Vec::new(),
        &mut LoadReport::new(),
    )
}

fn run(model: &mut LoadedModel, steps: usize) {
    for _ in 0..steps {
        model.update(STEP, false).expect("update");
    }
}

#[test]
fn an_emotion_plays_once_then_the_default_is_current_and_the_selector_shows_it() {
    let mut model = model();
    let mut state = ViewerState::from_config(&Config::default(), vec![model.entry()]);
    assert_eq!(state.animation_name(), Some("idle"), "the source's default is what opens");

    assert!(state.select_animation_by_name("wave"));
    model.play("wave", Duration::from_millis(150)).expect("known animation");
    assert_eq!(model.shown_animation(), Some("wave"));

    run(&mut model, 8); // 0.5 s: before the handover time
    assert_eq!(model.shown_animation(), Some("wave"));
    assert!(!state.note_shown(model.shown_animation()));

    run(&mut model, 8); // 1.0 s: past 0.75 s
    assert_eq!(model.model.current_animation(), Some("idle"));
    assert_eq!(model.shown_animation(), Some("idle"));
    assert!(state.note_shown(model.shown_animation()), "the selector learns it");
    assert_eq!(state.shown_name(), Some("idle"));
    assert_eq!(state.animation_name(), Some("wave"), "the pick is untouched");

    // It stays on the default, looping, long after the emotion's own end.
    run(&mut model, 160);
    assert_eq!(model.shown_animation(), Some("idle"));
}

#[test]
fn a_chain_of_emotions_ends_on_the_default() {
    let mut model = model();
    model.play("bow", Duration::ZERO).expect("known animation");
    run(&mut model, 9); // 0.5625 s: bow handed over to wave at 0.5 s
    assert_eq!(model.shown_animation(), Some("wave"));
    run(&mut model, 6); // 0.94 s: wave's own 0.75 s is 0.5 s after taking over, so still wave
    assert_eq!(model.shown_animation(), Some("wave"));
    run(&mut model, 16); // 1.94 s
    assert_eq!(model.shown_animation(), Some("idle"));
}

#[test]
fn the_default_loops_and_nothing_is_queued_behind_it() {
    let mut model = model();
    model.play("idle", Duration::ZERO).expect("known animation");
    run(&mut model, 200);
    assert_eq!(model.shown_animation(), Some("idle"));
}
