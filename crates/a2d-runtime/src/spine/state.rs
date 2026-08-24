//! Animation state: tracks, queueing and crossfading.
//!
//! Evaluation is delta-time based and independent of rendering frame rate
//! (spec §12): the same sequence of `dt` values always produces the same pose,
//! whatever wall-clock rate they arrive at.

use std::collections::VecDeque;

use a2d_core::ir::spine::SpineIr;

use crate::spine::apply::{apply_animation, FiredEvent, MixBlend};
use crate::spine::pose::SkeletonPose;

/// One animation playing on a track.
#[derive(Debug, Clone, PartialEq)]
pub struct TrackEntry {
    /// Index into [`SpineIr::animations`].
    pub animation: usize,
    /// Seconds elapsed since the animation started, before looping is applied.
    pub time: f32,
    /// The previous frame's time, for event windows. Negative before the first
    /// evaluation, which is what suppresses events on the very first frame.
    pub last_time: f32,
    pub looping: bool,
    pub speed: f32,
    /// Seconds still to wait before this entry starts playing.
    pub delay: f32,
    /// Crossfade length when this entry replaces another.
    pub mix_duration: f32,
}

impl TrackEntry {
    fn new(animation: usize) -> Self {
        TrackEntry { animation, time: 0.0, last_time: -1.0, looping: true, speed: 1.0, delay: 0.0, mix_duration: 0.0 }
    }

    /// Playback position within one pass, honouring looping.
    pub fn animation_time(&self, duration: f32) -> f32 {
        if duration <= 0.0 {
            return 0.0;
        }
        if !self.looping {
            return self.time.clamp(0.0, duration);
        }
        let t = self.time % duration;
        if t < 0.0 {
            t + duration
        } else {
            t
        }
    }

    fn is_complete(&self, duration: f32) -> bool {
        !self.looping && self.time >= duration
    }
}

/// One animation track. Higher-numbered tracks compose over lower ones.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct Track {
    current: Option<TrackEntry>,
    /// The entry being crossfaded out, if any.
    mixing_from: Option<TrackEntry>,
    mix_time: f32,
    mix_duration: f32,
    queue: VecDeque<TrackEntry>,
}

impl Track {
    pub fn current(&self) -> Option<&TrackEntry> {
        self.current.as_ref()
    }

    pub fn is_empty(&self) -> bool {
        self.current.is_none() && self.queue.is_empty()
    }

    /// Crossfade progress in `0..=1`. Reaches 1 when no mix is in flight.
    fn mix_alpha(&self) -> f32 {
        if self.mixing_from.is_none() || self.mix_duration <= 0.0 {
            1.0
        } else {
            (self.mix_time / self.mix_duration).clamp(0.0, 1.0)
        }
    }
}

/// What one track contributes to a frame: which animation to sample, at what
/// time, and at what crossfade weight.
///
/// Collected before the pose is touched, so `self.tracks` is not borrowed while
/// the pose is being mutated.
struct TrackPlan {
    /// Animation index, playback time, and previous time.
    current: Option<(usize, f32, f32)>,
    /// The animation being crossfaded out, in the same shape.
    from: Option<(usize, f32, f32)>,
    mix_alpha: f32,
}

/// The playing state of one skeleton.
#[derive(Debug, Clone, Default)]
pub struct AnimationState {
    tracks: Vec<Track>,
    pub paused: bool,
    /// Global playback rate, multiplied with each entry's own speed.
    pub time_scale: f32,
    /// Events fired by the most recent [`AnimationState::update`].
    pub events: Vec<FiredEvent>,
}

impl AnimationState {
    pub fn new() -> Self {
        AnimationState { tracks: Vec::new(), paused: false, time_scale: 1.0, events: Vec::new() }
    }

    pub fn track(&self, index: usize) -> Option<&Track> {
        self.tracks.get(index)
    }

    pub fn tracks(&self) -> &[Track] {
        &self.tracks
    }

    fn track_mut(&mut self, index: usize) -> &mut Track {
        if index >= self.tracks.len() {
            self.tracks.resize_with(index + 1, Track::default);
        }
        &mut self.tracks[index]
    }

    /// Starts `animation` on `track`, replacing whatever is there.
    ///
    /// A non-zero `mix_duration` crossfades from the outgoing animation.
    pub fn set_animation(
        &mut self,
        track: usize,
        animation: usize,
        looping: bool,
        speed: f32,
        delay: f32,
        mix_duration: f32,
    ) {
        let entry = TrackEntry { looping, speed, delay, mix_duration, ..TrackEntry::new(animation) };
        let t = self.track_mut(track);
        t.queue.clear();
        if mix_duration > 0.0 {
            if let Some(outgoing) = t.current.take() {
                t.mixing_from = Some(outgoing);
                t.mix_time = 0.0;
                t.mix_duration = mix_duration;
            }
        } else {
            t.mixing_from = None;
            t.mix_time = 0.0;
            t.mix_duration = 0.0;
        }
        t.current = Some(entry);
    }

    /// Appends `animation` after whatever is already queued on `track`.
    pub fn add_animation(
        &mut self,
        track: usize,
        animation: usize,
        looping: bool,
        speed: f32,
        delay: f32,
        mix_duration: f32,
    ) {
        let entry = TrackEntry { looping, speed, delay, mix_duration, ..TrackEntry::new(animation) };
        let t = self.track_mut(track);
        if t.current.is_none() {
            t.current = Some(entry);
        } else {
            t.queue.push_back(entry);
        }
    }

    /// Clears a track immediately.
    pub fn clear_track(&mut self, track: usize) {
        if let Some(t) = self.tracks.get_mut(track) {
            *t = Track::default();
        }
    }

    pub fn clear_all(&mut self) {
        for t in &mut self.tracks {
            *t = Track::default();
        }
    }

    /// Stops every entry playing `animation`, on any track.
    pub fn stop_animation(&mut self, animation: usize) {
        for t in &mut self.tracks {
            if t.current.as_ref().is_some_and(|e| e.animation == animation) {
                t.current = t.queue.pop_front();
                t.mixing_from = None;
                t.mix_time = 0.0;
                t.mix_duration = 0.0;
            }
            if t.mixing_from.as_ref().is_some_and(|e| e.animation == animation) {
                t.mixing_from = None;
            }
            t.queue.retain(|e| e.animation != animation);
        }
    }

    /// Sets a track's playback time directly.
    ///
    /// Unlike [`AnimationState::update`] this performs no completion handling,
    /// so scrubbing a non-looping animation to exactly its duration poses the
    /// final frame instead of ending the track. Events are suppressed, because
    /// a scrub is not playback.
    pub fn seek(&mut self, track: usize, time: f32) {
        let Some(t) = self.tracks.get_mut(track) else { return };
        if let Some(entry) = &mut t.current {
            entry.time = time;
            entry.last_time = -1.0;
            entry.delay = 0.0;
        }
        t.mixing_from = None;
        t.mix_time = 0.0;
        t.mix_duration = 0.0;
    }

    pub fn is_playing(&self, animation: usize) -> bool {
        self.tracks.iter().any(|t| t.current.as_ref().is_some_and(|e| e.animation == animation))
    }

    /// Advances every track by `dt` seconds.
    pub fn update(&mut self, ir: &SpineIr, dt: f32) {
        if self.paused || dt == 0.0 {
            return;
        }
        let scale = self.time_scale;
        for track in &mut self.tracks {
            // Advance the outgoing side of a crossfade so it keeps animating
            // while it fades, rather than freezing on its last frame.
            if let Some(from) = &mut track.mixing_from {
                advance(from, ir, dt * scale);
            }
            if track.mixing_from.is_some() {
                track.mix_time += dt * scale;
                if track.mix_duration <= 0.0 || track.mix_time >= track.mix_duration {
                    track.mixing_from = None;
                    track.mix_time = 0.0;
                    track.mix_duration = 0.0;
                }
            }

            let Some(entry) = &mut track.current else { continue };
            if entry.delay > 0.0 {
                entry.delay -= dt * scale;
                if entry.delay > 0.0 {
                    continue;
                }
                // Roll the leftover time into playback so a delay never costs a
                // fraction of a frame.
                let leftover = -entry.delay;
                entry.delay = 0.0;
                advance(entry, ir, leftover);
            } else {
                advance(entry, ir, dt * scale);
            }

            let duration = ir.animations.get(entry.animation).map_or(0.0, |a| a.duration);
            if entry.is_complete(duration) {
                let next = track.queue.pop_front();
                match next {
                    Some(next) => {
                        if next.mix_duration > 0.0 {
                            track.mixing_from = track.current.take();
                            track.mix_time = 0.0;
                            track.mix_duration = next.mix_duration;
                        }
                        track.current = Some(next);
                    }
                    None => track.current = None,
                }
            }
        }
    }

    /// Rebuilds the pose from the setup pose plus every active track.
    pub fn apply(&mut self, pose: &mut SkeletonPose) {
        self.events.clear();
        pose.reset_to_setup();

        let ir = pose.ir().clone();
        let mut first = true;
        // Collected first so `pose` is not borrowed while tracks are read.
        let plan: Vec<TrackPlan> = self
            .tracks
            .iter()
            .filter_map(|track| {
                let entry = track.current.as_ref()?;
                if entry.delay > 0.0 {
                    return None;
                }
                let duration = ir.animations.get(entry.animation)?.duration;
                let from = track.mixing_from.as_ref().and_then(|f| {
                    let d = ir.animations.get(f.animation)?.duration;
                    Some((f.animation, f.animation_time(d), f.last_time_in(d)))
                });
                Some(TrackPlan {
                    current: Some((entry.animation, entry.animation_time(duration), entry.last_time_in(duration))),
                    from,
                    mix_alpha: track.mix_alpha(),
                })
            })
            .collect();

        for TrackPlan { current, from, mix_alpha } in plan {
            let blend = if first { MixBlend::Setup } else { MixBlend::Replace };
            first = false;

            if let Some((animation, time, last_time)) = from {
                if let Some(a) = ir.animations.get(animation) {
                    apply_animation(pose, a, last_time, time, 1.0, blend, &mut self.events);
                }
            }
            if let Some((animation, time, last_time)) = current {
                if let Some(a) = ir.animations.get(animation) {
                    // A crossfade blends the incoming animation over whatever
                    // the outgoing one just wrote, so it always replaces.
                    let (alpha, blend) = if from.is_some() { (mix_alpha, MixBlend::Replace) } else { (1.0, blend) };
                    apply_animation(pose, a, last_time, time, alpha, blend, &mut self.events);
                }
            }
        }

        pose.update_world_transforms();
    }
}

impl TrackEntry {
    /// Previous evaluation time mapped into the animation's own span.
    fn last_time_in(&self, duration: f32) -> f32 {
        if self.last_time < 0.0 {
            return -1.0;
        }
        if duration <= 0.0 || !self.looping {
            return self.last_time;
        }
        let t = self.last_time % duration;
        if t < 0.0 {
            t + duration
        } else {
            t
        }
    }
}

fn advance(entry: &mut TrackEntry, ir: &SpineIr, dt: f32) {
    let duration = ir.animations.get(entry.animation).map_or(0.0, |a| a.duration);
    entry.last_time = entry.time;
    entry.time += dt * entry.speed;
    if entry.looping && duration > 0.0 && entry.time >= duration {
        // Keep `last_time` comparable with `time` after a wrap so the event
        // window does not span the loop point twice.
        entry.last_time -= duration * (entry.time / duration).floor();
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use a2d_core::ir::ids::BoneId;
    use a2d_core::ir::spine::{Animation, Bone, ScalarKey, Timeline};
    use a2d_core::Interpolation;
    use std::sync::Arc;

    fn ir_with(animations: Vec<Animation>) -> Arc<SpineIr> {
        let mut ir = SpineIr { bones: vec![Bone::new("root", None)], animations, ..Default::default() };
        ir.rebuild_derived();
        Arc::new(ir)
    }

    fn rotate_anim(name: &str, duration: f32, to: f32) -> Animation {
        Animation {
            name: name.into(),
            duration,
            timelines: vec![Timeline::BoneRotate {
                bone: BoneId(0),
                keys: vec![
                    ScalarKey { time: 0.0, value: 0.0, interp: Interpolation::Linear },
                    ScalarKey { time: duration, value: to, interp: Interpolation::Linear },
                ],
            }],
        }
    }

    /// Index of an animation after the IR's name sort.
    fn index_of(ir: &SpineIr, name: &str) -> usize {
        ir.animations.iter().position(|a| a.name == name).expect("animation should exist")
    }

    #[test]
    fn a_looping_animation_wraps_its_time() {
        let ir = ir_with(vec![rotate_anim("idle", 1.0, 90.0)]);
        let mut state = AnimationState::new();
        state.set_animation(0, 0, true, 1.0, 0.0, 0.0);
        for _ in 0..15 {
            state.update(&ir, 0.1);
        }
        let entry = state.track(0).unwrap().current().unwrap();
        assert!((entry.animation_time(1.0) - 0.5).abs() < 1e-4, "got {}", entry.animation_time(1.0));
    }

    #[test]
    fn a_one_shot_animation_clears_its_track_when_it_ends() {
        let ir = ir_with(vec![rotate_anim("hit", 0.5, 90.0)]);
        let mut state = AnimationState::new();
        state.set_animation(0, 0, false, 1.0, 0.0, 0.0);
        for _ in 0..4 {
            state.update(&ir, 0.1);
        }
        assert!(state.track(0).unwrap().current().is_some());
        state.update(&ir, 0.2);
        assert!(state.track(0).unwrap().current().is_none());
    }

    #[test]
    fn a_queued_animation_starts_when_the_previous_one_finishes() {
        let ir = ir_with(vec![rotate_anim("a", 0.5, 90.0), rotate_anim("b", 1.0, 45.0)]);
        let (a, b) = (index_of(&ir, "a"), index_of(&ir, "b"));
        let mut state = AnimationState::new();
        state.set_animation(0, a, false, 1.0, 0.0, 0.0);
        state.add_animation(0, b, true, 1.0, 0.0, 0.0);
        for _ in 0..6 {
            state.update(&ir, 0.1);
        }
        assert_eq!(state.track(0).unwrap().current().unwrap().animation, b);
    }

    #[test]
    fn playback_speed_scales_time() {
        let ir = ir_with(vec![rotate_anim("idle", 10.0, 90.0)]);
        let mut state = AnimationState::new();
        state.set_animation(0, 0, true, 2.0, 0.0, 0.0);
        state.update(&ir, 1.0);
        assert!((state.track(0).unwrap().current().unwrap().time - 2.0).abs() < 1e-5);
    }

    #[test]
    fn the_global_time_scale_multiplies_entry_speed() {
        let ir = ir_with(vec![rotate_anim("idle", 10.0, 90.0)]);
        let mut state = AnimationState::new();
        state.time_scale = 0.5;
        state.set_animation(0, 0, true, 2.0, 0.0, 0.0);
        state.update(&ir, 1.0);
        assert!((state.track(0).unwrap().current().unwrap().time - 1.0).abs() < 1e-5);
    }

    #[test]
    fn pausing_freezes_time() {
        let ir = ir_with(vec![rotate_anim("idle", 10.0, 90.0)]);
        let mut state = AnimationState::new();
        state.set_animation(0, 0, true, 1.0, 0.0, 0.0);
        state.update(&ir, 1.0);
        state.paused = true;
        state.update(&ir, 5.0);
        assert!((state.track(0).unwrap().current().unwrap().time - 1.0).abs() < 1e-5);
    }

    #[test]
    fn a_delayed_entry_waits_then_rolls_the_leftover_into_playback() {
        let ir = ir_with(vec![rotate_anim("idle", 10.0, 90.0)]);
        let mut state = AnimationState::new();
        state.set_animation(0, 0, true, 1.0, 0.5, 0.0);
        state.update(&ir, 0.2);
        assert_eq!(state.track(0).unwrap().current().unwrap().time, 0.0);
        state.update(&ir, 0.5);
        // 0.7 elapsed, 0.5 of it delay, so 0.2 of playback.
        assert!((state.track(0).unwrap().current().unwrap().time - 0.2).abs() < 1e-5);
    }

    #[test]
    fn stopping_an_animation_promotes_the_queue() {
        let ir = ir_with(vec![rotate_anim("a", 1.0, 90.0), rotate_anim("b", 1.0, 45.0)]);
        let (a, b) = (index_of(&ir, "a"), index_of(&ir, "b"));
        let mut state = AnimationState::new();
        state.set_animation(0, a, true, 1.0, 0.0, 0.0);
        state.add_animation(0, b, true, 1.0, 0.0, 0.0);
        assert!(state.is_playing(a));
        state.stop_animation(a);
        assert_eq!(state.track(0).unwrap().current().unwrap().animation, b);
        assert!(!state.is_playing(a));
    }

    #[test]
    fn stopping_an_animation_that_is_not_playing_is_a_no_op() {
        let ir = ir_with(vec![rotate_anim("a", 1.0, 90.0)]);
        let mut state = AnimationState::new();
        state.set_animation(0, 0, true, 1.0, 0.0, 0.0);
        state.stop_animation(99);
        assert!(state.is_playing(0));
        // And playback continues undisturbed.
        state.update(&ir, 0.25);
        assert!(state.is_playing(0));
    }

    #[test]
    fn tracks_compose_with_higher_tracks_winning() {
        let ir = ir_with(vec![rotate_anim("a", 2.0, 90.0), rotate_anim("b", 2.0, 45.0)]);
        let (a, b) = (index_of(&ir, "a"), index_of(&ir, "b"));
        let mut pose = SkeletonPose::new(ir.clone());
        let mut state = AnimationState::new();
        state.set_animation(0, a, true, 1.0, 0.0, 0.0);
        state.set_animation(1, b, true, 1.0, 0.0, 0.0);
        // Half way through a 2s pass, so neither animation is at a loop wrap.
        state.update(&ir, 1.0);
        state.apply(&mut pose);
        // Track 1 replaces track 0 outright at full alpha: 45/2, not 90/2.
        assert!((pose.bones[0].local.rotation - 22.5).abs() < 1e-3, "got {}", pose.bones[0].local.rotation);
    }

    #[test]
    fn a_looping_animation_wraps_to_zero_at_exactly_its_duration() {
        let ir = ir_with(vec![rotate_anim("idle", 1.0, 90.0)]);
        let mut pose = SkeletonPose::new(ir.clone());
        let mut state = AnimationState::new();
        state.set_animation(0, 0, true, 1.0, 0.0, 0.0);
        state.update(&ir, 1.0);
        state.apply(&mut pose);
        assert_eq!(pose.bones[0].local.rotation, 0.0, "one full pass returns to the first frame");
    }

    #[test]
    fn a_crossfade_blends_from_one_animation_to_the_other() {
        // `a` ramps 0 -> 100 over 2s; `b` holds 0 throughout.
        let ir = ir_with(vec![rotate_anim("a", 2.0, 100.0), rotate_anim("b", 2.0, 0.0)]);
        let (a, b) = (index_of(&ir, "a"), index_of(&ir, "b"));
        let mut pose = SkeletonPose::new(ir.clone());
        let mut state = AnimationState::new();

        state.set_animation(0, a, true, 1.0, 0.0, 0.0);
        state.update(&ir, 1.0);
        state.apply(&mut pose);
        let before = pose.bones[0].local.rotation;
        assert!((before - 50.0).abs() < 1e-3, "got {before}");

        // Start `b` with a 1s crossfade, then step half way through it.
        state.set_animation(0, b, true, 1.0, 0.0, 1.0);
        state.update(&ir, 0.5);
        state.apply(&mut pose);
        let during = pose.bones[0].local.rotation;
        assert!(during > 1.0 && during < 74.0, "expected a blended value, got {during}");
    }

    #[test]
    fn a_crossfade_completes_and_releases_the_outgoing_entry() {
        let ir = ir_with(vec![rotate_anim("a", 1.0, 100.0), rotate_anim("b", 1.0, 0.0)]);
        let (a, b) = (index_of(&ir, "a"), index_of(&ir, "b"));
        let mut state = AnimationState::new();
        state.set_animation(0, a, true, 1.0, 0.0, 0.0);
        state.set_animation(0, b, true, 1.0, 0.0, 0.5);
        state.update(&ir, 0.6);
        assert!(state.track(0).unwrap().mixing_from.is_none());
        assert_eq!(state.track(0).unwrap().current().unwrap().animation, b);
    }

    #[test]
    fn evaluation_is_deterministic_for_the_same_delta_sequence() {
        let ir = ir_with(vec![rotate_anim("idle", 1.0, 90.0)]);
        let run = |steps: &[f32]| {
            let mut pose = SkeletonPose::new(ir.clone());
            let mut state = AnimationState::new();
            state.set_animation(0, 0, true, 1.0, 0.0, 0.0);
            for dt in steps {
                state.update(&ir, *dt);
            }
            state.apply(&mut pose);
            pose.bones[0].local.rotation
        };
        let steps = [0.016, 0.033, 0.007, 0.5, 0.12];
        assert_eq!(run(&steps), run(&steps));
    }

    #[test]
    fn different_step_sizes_reaching_the_same_total_agree() {
        let ir = ir_with(vec![rotate_anim("idle", 4.0, 90.0)]);
        let run = |steps: &[f32]| {
            let mut pose = SkeletonPose::new(ir.clone());
            let mut state = AnimationState::new();
            state.set_animation(0, 0, true, 1.0, 0.0, 0.0);
            for dt in steps {
                state.update(&ir, *dt);
            }
            state.apply(&mut pose);
            pose.bones[0].local.rotation
        };
        let coarse = run(&[1.0, 1.0]);
        let fine = run(&[0.25; 8]);
        assert!((coarse - fine).abs() < 1e-3, "{coarse} vs {fine}");
    }

    #[test]
    fn clearing_a_track_empties_it() {
        let ir = ir_with(vec![rotate_anim("a", 1.0, 90.0)]);
        let mut state = AnimationState::new();
        state.set_animation(0, 0, true, 1.0, 0.0, 0.0);
        state.clear_track(0);
        state.update(&ir, 1.0);
        assert!(state.track(0).unwrap().is_empty());
        state.update(&ir, 1.0);
        assert!(state.track(0).unwrap().current().is_none());
    }

    #[test]
    fn seeking_poses_the_final_frame_of_a_non_looping_animation() {
        let ir = ir_with(vec![rotate_anim("hit", 1.0, 90.0)]);
        let mut pose = SkeletonPose::new(ir.clone());
        let mut state = AnimationState::new();
        state.set_animation(0, 0, false, 1.0, 0.0, 0.0);
        state.seek(0, 1.0);
        state.apply(&mut pose);
        // `update` would have retired the track here; `seek` must not.
        assert!(state.track(0).unwrap().current().is_some());
        assert!((pose.bones[0].local.rotation - 90.0).abs() < 1e-3, "got {}", pose.bones[0].local.rotation);
    }

    #[test]
    fn seeking_fires_no_events() {
        let ir = ir_with(vec![rotate_anim("idle", 1.0, 90.0)]);
        let mut pose = SkeletonPose::new(ir.clone());
        let mut state = AnimationState::new();
        state.set_animation(0, 0, true, 1.0, 0.0, 0.0);
        state.seek(0, 0.5);
        state.apply(&mut pose);
        assert!(state.events.is_empty());
    }

    #[test]
    fn a_zero_duration_animation_does_not_divide_by_zero() {
        let ir = ir_with(vec![Animation::new("empty")]);
        let mut pose = SkeletonPose::new(ir.clone());
        let mut state = AnimationState::new();
        state.set_animation(0, 0, true, 1.0, 0.0, 0.0);
        state.update(&ir, 1.0);
        state.apply(&mut pose);
        assert!(pose.bones[0].local.rotation.is_finite());
    }
}
