//! Animation state: tracks, queueing and crossfading.
//!
//! Evaluation is delta-time based and independent of rendering frame rate
//! (spec §12): the same sequence of `dt` values always produces the same pose,
//! whatever wall-clock rate they arrive at.

use std::collections::VecDeque;

use a2d_core::ir::spine::SpineIr;

use crate::spine::apply::{apply_animation, collect_animation_events, FiredEvent, MixBlend};
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
    /// For a queued entry: the takeover happens when the entry ahead of it
    /// reaches this time within its current pass. `None` is the end of the pass.
    pub handover_at: Option<f32>,
    /// Whether the entry has been advanced yet. A backwards entry starts at the
    /// end of its animation, which is only known on its first step.
    started: bool,
}

impl TrackEntry {
    fn new(animation: usize) -> Self {
        TrackEntry {
            animation,
            time: 0.0,
            last_time: -1.0,
            looping: true,
            speed: 1.0,
            delay: 0.0,
            mix_duration: 0.0,
            handover_at: None,
            started: false,
        }
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

    /// Where a queued entry takes over from this one, given that this entry
    /// just moved from raw time `before` to `self.time`.
    ///
    /// Returns the raw time at which the current pass started (zero for a
    /// one-shot) and how much wall time (before the entry's own speed) lies past
    /// the takeover moment inside this step. The moment is `handover_at` into
    /// the pass when that is a forward-playing, in-range time the playhead
    /// crossed during the step, and otherwise the end of the pass. A
    /// `handover_at` the playhead is already beyond therefore waits for the end
    /// of the pass.
    ///
    /// The end of the pass is direction-aware: forwards it is `duration` (a
    /// one-shot finishing, a loop wrapping); backwards it is time 0, where a
    /// one-shot finishes and a loop wraps. A speed of zero never ends a pass, so
    /// nothing queued behind it takes over. A zero-length animation hands over
    /// at once, carrying the whole step.
    ///
    /// Differs from Cubism in one respect: if the takeover happens while the
    /// track is still crossfading out an older entry, that older mix is dropped
    /// and the pose snaps to the new crossfade, where Cubism freezes the blended
    /// pose and fades from it. Chained mixing is not built because Spine data
    /// never carries handovers, so the situation only arises from a host
    /// queueing short entries behind one another.
    fn takeover(&self, before: f32, duration: f32, handover_at: Option<f32>) -> Option<(f32, f32)> {
        if duration.is_nan() || duration <= 0.0 {
            // Nothing to play out: hand over at once rather than stall the queue.
            let over = if self.speed > 0.0 { (self.time - before).max(0.0) / self.speed } else { 0.0 };
            return Some((0.0, over));
        }
        if self.speed.is_nan() || self.speed == 0.0 {
            return None;
        }
        let forward = self.speed > 0.0;
        let start = match (self.looping, forward) {
            (false, _) => 0.0,
            (true, true) => (before / duration).floor() * duration,
            // A backwards pass covers (start, start + duration].
            (true, false) => ((before / duration).ceil() - 1.0) * duration,
        };
        let (b, a) = (before - start, self.time - start);
        if let Some(at) = handover_at {
            if forward && at.is_finite() && at >= 0.0 && at < duration && b <= at && a >= at {
                return Some((start, (a - at) / self.speed));
            }
        }
        if forward {
            (a >= duration).then(|| (start, (a - duration) / self.speed))
        } else {
            (a <= 0.0).then(|| (start, a / self.speed))
        }
    }

    /// Whether a one-shot has played out, in the direction it plays.
    fn has_ended(&self, duration: f32) -> bool {
        if self.speed < 0.0 {
            self.time <= 0.0
        } else {
            self.time >= duration
        }
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
    /// An entry that just left the track without a crossfade to carry it: its
    /// last event window, as (animation, previous time, time). Fired by the
    /// next [`AnimationState::apply`] so a large step cannot swallow events.
    final_window: Option<(usize, f32, f32)>,
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
        self.add_animation_at(track, animation, looping, speed, delay, mix_duration, None);
    }

    /// [`AnimationState::add_animation`] with a handover time: the entry takes
    /// over when the entry ahead of it reaches `handover_at` seconds into its
    /// current pass, instead of at the end of that pass.
    #[allow(clippy::too_many_arguments)]
    pub(crate) fn add_animation_at(
        &mut self,
        track: usize,
        animation: usize,
        looping: bool,
        speed: f32,
        delay: f32,
        mix_duration: f32,
        handover_at: Option<f32>,
    ) {
        let entry = TrackEntry { looping, speed, delay, mix_duration, handover_at, ..TrackEntry::new(animation) };
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
            entry.started = true;
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
            let duration = ir.animations.get(entry.animation).map_or(0.0, |a| a.duration);
            let before;
            if entry.delay > 0.0 {
                entry.delay -= dt * scale;
                if entry.delay > 0.0 {
                    continue;
                }
                // Roll the leftover time into playback so a delay never costs a
                // fraction of a frame.
                let leftover = -entry.delay;
                entry.delay = 0.0;
                begin(entry, duration);
                before = entry.time;
                advance(entry, ir, leftover);
            } else {
                begin(entry, duration);
                before = entry.time;
                advance(entry, ir, dt * scale);
            }

            let Some(next_at) = track.queue.front().map(|n| n.handover_at) else {
                if !entry.looping && entry.has_ended(duration) {
                    track.final_window =
                        Some((entry.animation, entry.last_time_in(duration), entry.animation_time(duration)));
                    track.current = None;
                }
                continue;
            };
            let Some((start, over)) = entry.takeover(before, duration, next_at) else { continue };
            let Some(mut next) = track.queue.pop_front() else { continue };

            // The outgoing entry plays on, without looping, so the crossfade runs
            // under the rest of its pass and a looping entry holds its last frame
            // rather than snapping back to its first. Its event window ends at
            // the end of the step, so nothing it crossed is lost.
            let mut outgoing = entry.clone();
            outgoing.looping = false;
            outgoing.time -= start;
            outgoing.last_time = before - start;

            // Time past the takeover moment belongs to the incoming entry, and to
            // its crossfade, so the result does not depend on where step
            // boundaries fell. A delay on the incoming entry absorbs it first.
            let over = over.max(0.0);
            let mut carried = over;
            if next.delay > 0.0 {
                let remaining = next.delay - carried;
                carried = (-remaining).max(0.0);
                next.delay = remaining.max(0.0);
            }
            if next.delay <= 0.0 {
                // A backwards entry starts at the end of its animation, so the
                // carried span is measured from there, not from 0.
                let next_duration = ir.animations.get(next.animation).map_or(0.0, |a| a.duration);
                begin(&mut next, next_duration);
                if carried > 0.0 {
                    let start_time = next.time;
                    next.time = start_time + carried * next.speed;
                    // Events inside the carried span fire; `-1` would suppress
                    // them. Backwards the window is empty, as in `advance`.
                    next.last_time = if next.speed < 0.0 { start_time } else { 0.0 };
                }
            }
            // A takeover during a running crossfade replaces it outright (see
            // `takeover`). The outgoing entry stays in `mixing_from` even when
            // the overshoot already covers the whole fade (or the mix is zero),
            // so its last event window fires on this frame; `update` clears it on
            // the next step, and a full-weight mix shows only the incoming pose.
            track.mixing_from = Some(outgoing);
            track.mix_time = over;
            track.mix_duration = next.mix_duration;
            track.current = Some(next);
        }
    }

    /// Rebuilds the pose from the setup pose plus every active track.
    pub fn apply(&mut self, pose: &mut SkeletonPose) {
        self.events.clear();
        pose.reset_to_setup();

        let ir = pose.ir().clone();
        for track in &mut self.tracks {
            if let Some((animation, last_time, time)) = track.final_window.take() {
                if let Some(a) = ir.animations.get(animation) {
                    collect_animation_events(pose, a, last_time, time, &mut self.events);
                }
            }
        }
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

            // An outgoing entry whose fade is already complete (a takeover whose
            // overshoot covered the whole mix) contributes only its last events.
            let faded = from.is_some() && mix_alpha < 1.0;
            if let Some((animation, time, last_time)) = from {
                if let Some(a) = ir.animations.get(animation) {
                    if faded {
                        apply_animation(pose, a, last_time, time, 1.0, blend, &mut self.events);
                    } else {
                        collect_animation_events(pose, a, last_time, time, &mut self.events);
                    }
                }
            }
            if let Some((animation, time, last_time)) = current {
                if let Some(a) = ir.animations.get(animation) {
                    // A crossfade blends the incoming animation over whatever
                    // the outgoing one just wrote, so it always replaces.
                    let (alpha, blend) = if faded { (mix_alpha, MixBlend::Replace) } else { (1.0, blend) };
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
        if self.speed < 0.0 {
            // Event windows only run forwards: a backwards entry fires nothing,
            // so the window is empty rather than wrapped into a bogus span.
            return self.animation_time(duration);
        }
        let t = self.last_time % duration;
        if t < 0.0 {
            t + duration
        } else {
            t
        }
    }
}

/// Places a backwards entry at the end of its animation on its first step.
fn begin(entry: &mut TrackEntry, duration: f32) {
    if !entry.started {
        entry.started = true;
        if entry.speed < 0.0 && !duration.is_nan() {
            entry.time = duration;
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
    fn a_once_entry_hands_back_to_a_queued_loop_with_a_crossfade() {
        let ir = ir_with(vec![rotate_anim("stand", 1.0, 10.0), rotate_anim("fidget", 0.5, 90.0)]);
        let (stand, fidget) = (index_of(&ir, "stand"), index_of(&ir, "fidget"));
        let mut state = AnimationState::new();
        state.set_animation(0, stand, true, 1.0, 0.0, 0.0);
        state.set_animation(0, fidget, false, 1.0, 0.0, 0.3);
        state.add_animation(0, stand, true, 1.0, 0.0, 0.3);
        for _ in 0..6 {
            state.update(&ir, 0.1);
        }
        let track = state.track(0).unwrap();
        assert_eq!(track.current().unwrap().animation, stand);
        let from = track.mixing_from.as_ref().expect("the variation is crossfading out");
        assert_eq!(from.animation, fidget);
        assert!((from.animation_time(0.5) - 0.5).abs() < 1e-5, "held at its end");

        let mut pose = SkeletonPose::new(ir.clone());
        state.apply(&mut pose);
        let rotation = pose.bones[0].local.rotation;
        assert!(rotation > 0.0 && rotation < 90.0, "blended between the two poses, got {rotation}");

        for _ in 0..3 {
            state.update(&ir, 0.1);
        }
        assert!(state.track(0).unwrap().mixing_from.is_none());
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

    // ---- takeover at a loop wrap and at a handover time ----

    /// `a` (1s) and `b` (1s) with distinct slopes so the playing one is evident.
    fn two() -> (Arc<SpineIr>, usize, usize) {
        let ir = ir_with(vec![rotate_anim("a", 1.0, 90.0), rotate_anim("b", 1.0, 45.0)]);
        let (a, b) = (index_of(&ir, "a"), index_of(&ir, "b"));
        (ir, a, b)
    }

    fn playing(state: &AnimationState) -> Option<usize> {
        state.track(0).and_then(|t| t.current()).map(|e| e.animation)
    }

    #[test]
    fn a_queued_entry_takes_over_a_looping_one_at_its_wrap() {
        let (ir, a, b) = two();
        let mut state = AnimationState::new();
        state.set_animation(0, a, true, 1.0, 0.0, 0.0);
        state.add_animation(0, b, true, 1.0, 0.0, 0.0);
        for _ in 0..3 {
            state.update(&ir, 0.25);
        }
        assert_eq!(playing(&state), Some(a), "0.75 s in, not yet");
        state.update(&ir, 0.25);
        assert_eq!(playing(&state), Some(b), "the wrap is the takeover");
        for _ in 0..20 {
            state.update(&ir, 0.25);
        }
        assert_eq!(playing(&state), Some(b), "and it does not take over again");
    }

    #[test]
    fn a_looping_entry_with_nothing_queued_keeps_looping() {
        let (ir, a, _) = two();
        let mut state = AnimationState::new();
        state.set_animation(0, a, true, 1.0, 0.0, 0.0);
        for _ in 0..10 {
            state.update(&ir, 0.3);
        }
        assert_eq!(playing(&state), Some(a));
    }

    #[test]
    fn the_overshoot_at_a_wrap_reaches_the_incoming_entry_and_its_fade() {
        let (ir, a, b) = two();
        let run = |steps: &[f32]| {
            let mut state = AnimationState::new();
            state.set_animation(0, a, true, 1.0, 0.0, 0.0);
            state.add_animation(0, b, true, 1.0, 0.0, 0.5);
            for dt in steps {
                state.update(&ir, *dt);
            }
            let t = state.track(0).unwrap();
            (t.current().unwrap().time, t.mix_time)
        };
        let (big_time, big_mix) = run(&[1.25]);
        assert!((big_time - 0.25).abs() < 1e-5 && (big_mix - 0.25).abs() < 1e-5, "{big_time} {big_mix}");
        let (small_time, small_mix) = run(&[0.25; 5]);
        assert!((small_time - big_time).abs() < 1e-5 && (small_mix - big_mix).abs() < 1e-5);
    }

    #[test]
    fn a_looping_outgoing_entry_holds_its_last_frame_under_the_fade() {
        let (ir, a, b) = two();
        let mut state = AnimationState::new();
        state.set_animation(0, a, true, 1.0, 0.0, 0.0);
        state.add_animation(0, b, true, 1.0, 0.0, 0.5);
        state.update(&ir, 1.1);
        let from = state.track(0).unwrap().mixing_from.as_ref().expect("fading out");
        assert_eq!(from.animation, a);
        assert!((from.animation_time(1.0) - 1.0).abs() < 1e-5, "held at its end, got {}", from.animation_time(1.0));
    }

    #[test]
    fn a_handover_time_starts_the_takeover_exactly_there() {
        let (ir, a, b) = two();
        let run = |steps: &[f32]| {
            let mut state = AnimationState::new();
            state.set_animation(0, a, false, 1.0, 0.0, 0.0);
            state.add_animation_at(0, b, true, 1.0, 0.0, 0.5, Some(0.5));
            for dt in steps {
                state.update(&ir, *dt);
            }
            state
        };
        assert_eq!(playing(&run(&[0.25])), Some(a), "before");
        let at = run(&[0.5]);
        assert_eq!(playing(&at), Some(b), "exactly at");
        assert_eq!(at.track(0).unwrap().current().unwrap().time, 0.0);
        let after = run(&[0.75]);
        assert_eq!(playing(&after), Some(b), "after");
        assert!((after.track(0).unwrap().current().unwrap().time - 0.25).abs() < 1e-5, "overshoot carried");
        assert!((after.track(0).unwrap().mix_time - 0.25).abs() < 1e-5);
        let small = run(&[0.25; 3]);
        assert!((small.track(0).unwrap().current().unwrap().time - 0.25).abs() < 1e-5);
        // The outgoing entry plays on under the fade, past the handover time.
        let from = after.track(0).unwrap().mixing_from.as_ref().unwrap();
        assert!((from.animation_time(1.0) - 0.75).abs() < 1e-5);
    }

    #[test]
    fn a_handover_time_works_behind_a_looping_entry_in_its_current_pass() {
        let (ir, a, b) = two();
        let mut state = AnimationState::new();
        state.set_animation(0, a, true, 1.0, 0.0, 0.0);
        state.add_animation_at(0, b, true, 1.0, 0.0, 0.0, Some(0.5));
        state.update(&ir, 0.4);
        assert_eq!(playing(&state), Some(a));
        state.update(&ir, 0.2);
        assert_eq!(playing(&state), Some(b));
    }

    #[test]
    fn a_handover_time_already_passed_waits_for_the_end_of_the_pass() {
        let (ir, a, b) = two();
        let mut state = AnimationState::new();
        state.set_animation(0, a, true, 1.0, 0.0, 0.0);
        state.update(&ir, 0.75);
        state.add_animation_at(0, b, true, 1.0, 0.0, 0.0, Some(0.5));
        state.update(&ir, 0.2);
        assert_eq!(playing(&state), Some(a), "0.95 s: the 0.5 s moment is behind");
        state.update(&ir, 0.1);
        assert_eq!(playing(&state), Some(b), "the wrap");
    }

    #[test]
    fn an_unusable_handover_time_falls_back_to_the_end_of_the_pass() {
        let (ir, a, b) = two();
        for at in [1.0, 5.0, -0.5, f32::NAN] {
            let mut state = AnimationState::new();
            state.set_animation(0, a, true, 1.0, 0.0, 0.0);
            state.add_animation_at(0, b, true, 1.0, 0.0, 0.0, Some(at));
            state.update(&ir, 0.9);
            assert_eq!(playing(&state), Some(a), "at {at}");
            state.update(&ir, 0.2);
            assert_eq!(playing(&state), Some(b), "at {at}");
        }
        // A backwards once entry cannot cross a time, and falls back to its end.
        let mut state = AnimationState::new();
        state.set_animation(0, a, false, -1.0, 0.0, 0.0);
        state.add_animation_at(0, b, true, 1.0, 0.0, 0.0, Some(0.5));
        state.update(&ir, 0.25);
        assert_eq!(playing(&state), Some(a));
        state.update(&ir, 0.85);
        assert_eq!(playing(&state), Some(b));
    }

    #[test]
    fn a_backwards_once_entry_ends_at_time_zero_and_carries_the_overshoot() {
        let (ir, a, b) = two();
        let mut state = AnimationState::new();
        state.set_animation(0, a, false, -1.0, 0.0, 0.0);
        state.add_animation(0, b, true, 1.0, 0.0, 0.5);
        state.update(&ir, 0.5);
        assert_eq!(playing(&state), Some(a));
        assert!((state.track(0).unwrap().current().unwrap().time - 0.5).abs() < 1e-5, "starts at the end");
        state.update(&ir, 0.75);
        assert_eq!(playing(&state), Some(b));
        let t = state.track(0).unwrap();
        assert!((t.current().unwrap().time - 0.25).abs() < 1e-5);
        assert!((t.mix_time - 0.25).abs() < 1e-5);
        assert!((t.mixing_from.as_ref().unwrap().animation_time(1.0)).abs() < 1e-5, "held at 0");
    }

    #[test]
    fn a_backwards_once_entry_with_nothing_queued_clears_the_track() {
        let (ir, a, _) = two();
        let mut state = AnimationState::new();
        state.set_animation(0, a, false, -1.0, 0.0, 0.0);
        state.update(&ir, 0.5);
        assert!(playing(&state).is_some());
        state.update(&ir, 0.6);
        assert!(playing(&state).is_none());
    }

    #[test]
    fn a_backwards_loop_wraps_at_zero_and_hands_over() {
        let (ir, a, b) = two();
        let mut state = AnimationState::new();
        state.set_animation(0, a, true, -1.0, 0.0, 0.0);
        state.add_animation(0, b, true, 1.0, 0.0, 0.0);
        state.update(&ir, 0.9);
        assert_eq!(playing(&state), Some(a), "0.1 s from the wrap");
        state.update(&ir, 0.35);
        assert_eq!(playing(&state), Some(b));
        assert!((state.track(0).unwrap().current().unwrap().time - 0.25).abs() < 1e-5);
    }

    #[test]
    fn a_backwards_loop_hands_over_on_its_second_wrap_too() {
        let (ir, a, b) = two();
        let mut state = AnimationState::new();
        state.set_animation(0, a, true, -1.0, 0.0, 0.0);
        state.update(&ir, 1.5);
        state.add_animation(0, b, true, 1.0, 0.0, 0.0);
        state.update(&ir, 0.4);
        assert_eq!(playing(&state), Some(a), "0.9 s into the second pass");
        state.update(&ir, 0.2);
        assert_eq!(playing(&state), Some(b));
    }

    #[test]
    fn nothing_takes_over_behind_a_stationary_entry() {
        let (ir, a, b) = two();
        let mut state = AnimationState::new();
        state.set_animation(0, a, true, 0.0, 0.0, 0.0);
        state.add_animation(0, b, true, 1.0, 0.0, 0.0);
        for _ in 0..10 {
            state.update(&ir, 1.0);
        }
        assert_eq!(playing(&state), Some(a));
    }

    #[test]
    fn a_zero_length_entry_carries_the_whole_step() {
        let ir = ir_with(vec![Animation::new("empty"), rotate_anim("b", 1.0, 45.0)]);
        let (e, b) = (index_of(&ir, "empty"), index_of(&ir, "b"));
        let mut state = AnimationState::new();
        state.set_animation(0, e, true, 2.0, 0.0, 0.0);
        state.add_animation(0, b, true, 1.0, 0.0, 0.0);
        state.update(&ir, 0.4);
        assert!((state.track(0).unwrap().current().unwrap().time - 0.4).abs() < 1e-5);
    }

    #[test]
    fn the_outgoing_events_fire_when_a_large_step_covers_the_whole_mix() {
        use a2d_core::ir::ids::EventId;
        use a2d_core::ir::spine::{EventData, EventKey};
        let mut ir = SpineIr {
            bones: vec![Bone::new("root", None)],
            animations: vec![
                Animation {
                    name: "a".into(),
                    duration: 1.0,
                    timelines: vec![Timeline::Event {
                        keys: vec![EventKey {
                            time: 0.95,
                            event: EventId(0),
                            int_value: 0,
                            float_value: 0.0,
                            string_value: None,
                            volume: 1.0,
                            balance: 0.0,
                        }],
                    }],
                },
                rotate_anim("b", 1.0, 45.0),
            ],
            events: vec![EventData {
                name: "step".into(),
                int_value: 0,
                float_value: 0.0,
                string_value: String::new(),
                audio_path: None,
                volume: 1.0,
                balance: 0.0,
            }],
            ..Default::default()
        };
        ir.rebuild_derived();
        let ir = Arc::new(ir);
        let (a, b) = (index_of(&ir, "a"), index_of(&ir, "b"));
        for mix in [0.1_f32, 0.0] {
            let mut pose = SkeletonPose::new(ir.clone());
            let mut state = AnimationState::new();
            state.set_animation(0, a, false, 1.0, 0.0, 0.0);
            state.add_animation(0, b, true, 1.0, 0.0, mix);
            state.update(&ir, 0.9);
            state.apply(&mut pose);
            assert!(state.events.is_empty());
            state.update(&ir, 0.3);
            state.apply(&mut pose);
            assert_eq!(playing(&state), Some(b));
            let names: Vec<_> = state.events.iter().map(|e| e.name.as_str()).collect();
            assert_eq!(names, vec!["step"], "mix {mix}");
            state.update(&ir, 0.1);
            state.apply(&mut pose);
            assert!(state.events.is_empty(), "and only once");
        }
    }

    /// `a` (1s, forward) then a backwards `b` (1s) carrying an event at 0.5.
    fn eventful_backwards() -> (Arc<SpineIr>, usize, usize) {
        use a2d_core::ir::ids::EventId;
        use a2d_core::ir::spine::{EventData, EventKey};
        let mut ir = SpineIr {
            bones: vec![Bone::new("root", None)],
            animations: vec![
                rotate_anim("a", 1.0, 90.0),
                Animation {
                    name: "b".into(),
                    duration: 1.0,
                    timelines: vec![
                        Timeline::BoneRotate {
                            bone: BoneId(0),
                            keys: vec![
                                ScalarKey { time: 0.0, value: 0.0, interp: Interpolation::Linear },
                                ScalarKey { time: 1.0, value: 100.0, interp: Interpolation::Linear },
                            ],
                        },
                        Timeline::Event {
                            keys: vec![EventKey {
                                time: 0.5,
                                event: EventId(0),
                                int_value: 0,
                                float_value: 0.0,
                                string_value: None,
                                volume: 1.0,
                                balance: 0.0,
                            }],
                        },
                    ],
                },
            ],
            events: vec![EventData {
                name: "step".into(),
                int_value: 0,
                float_value: 0.0,
                string_value: String::new(),
                audio_path: None,
                volume: 1.0,
                balance: 0.0,
            }],
            ..Default::default()
        };
        ir.rebuild_derived();
        let ir = Arc::new(ir);
        let (a, b) = (index_of(&ir, "a"), index_of(&ir, "b"));
        (ir, a, b)
    }

    #[test]
    fn a_backwards_entry_takes_over_from_its_end_with_the_overshoot() {
        let (ir, a, b) = eventful_backwards();
        for looping in [false, true] {
            let mut pose = SkeletonPose::new(ir.clone());
            let mut state = AnimationState::new();
            state.set_animation(0, a, false, 1.0, 0.0, 0.0);
            state.add_animation(0, b, looping, -1.0, 0.0, 0.0);
            state.update(&ir, 0.8);
            state.apply(&mut pose);
            // 0.2 s past the end of `a`: the backwards entry is 0.2 in from its end.
            state.update(&ir, 0.4);
            state.apply(&mut pose);
            assert_eq!(playing(&state), Some(b));
            assert!((state.track(0).unwrap().current().unwrap().time - 0.8).abs() < 1e-5, "loop {looping}");
            assert!(
                (pose.bones[0].local.rotation - 80.0).abs() < 1e-3,
                "loop {looping}: {}",
                pose.bones[0].local.rotation
            );
            assert!(state.events.is_empty(), "no events on the takeover frame, loop {looping}");
            // Playing on, the event at 0.5 fires only when a forward window
            // contains it, which a backwards one never does.
            state.update(&ir, 0.2);
            state.apply(&mut pose);
            assert!(state.events.is_empty(), "loop {looping}");
            assert!((pose.bones[0].local.rotation - 60.0).abs() < 1e-3);
        }
    }

    #[test]
    fn a_once_entrys_last_events_fire_once_when_a_large_step_ends_it() {
        use a2d_core::ir::ids::EventId;
        use a2d_core::ir::spine::{EventData, EventKey};
        let mut ir = SpineIr {
            bones: vec![Bone::new("root", None)],
            animations: vec![Animation {
                name: "a".into(),
                duration: 1.0,
                timelines: vec![Timeline::Event {
                    keys: vec![EventKey {
                        time: 0.95,
                        event: EventId(0),
                        int_value: 0,
                        float_value: 0.0,
                        string_value: None,
                        volume: 1.0,
                        balance: 0.0,
                    }],
                }],
            }],
            events: vec![EventData {
                name: "end".into(),
                int_value: 0,
                float_value: 0.0,
                string_value: String::new(),
                audio_path: None,
                volume: 1.0,
                balance: 0.0,
            }],
            ..Default::default()
        };
        ir.rebuild_derived();
        let ir = Arc::new(ir);
        let mut pose = SkeletonPose::new(ir.clone());
        let mut state = AnimationState::new();
        state.set_animation(0, 0, false, 1.0, 0.0, 0.0);
        state.update(&ir, 0.9);
        state.apply(&mut pose);
        assert!(state.events.is_empty());
        state.update(&ir, 0.3);
        state.apply(&mut pose);
        assert!(playing(&state).is_none());
        assert_eq!(state.events.len(), 1);
        state.update(&ir, 0.1);
        state.apply(&mut pose);
        assert!(state.events.is_empty(), "and only once");
    }

    #[test]
    fn a_handover_crossing_scales_the_overshoot_by_the_outgoing_speed() {
        let (ir, a, b) = two();
        let mut state = AnimationState::new();
        state.set_animation(0, a, false, 2.0, 0.0, 0.0);
        state.add_animation_at(0, b, true, 1.0, 0.0, 0.5, Some(0.5));
        // 0.3 s at twice the speed is 0.6 of the animation: 0.1 past the 0.5
        // moment, which took 0.05 s of wall time.
        state.update(&ir, 0.3);
        let t = state.track(0).unwrap();
        assert_eq!(t.current().unwrap().animation, b);
        assert!((t.current().unwrap().time - 0.05).abs() < 1e-5, "got {}", t.current().unwrap().time);
        assert!((t.mix_time - 0.05).abs() < 1e-5);
        assert!((t.mixing_from.as_ref().unwrap().animation_time(1.0) - 0.6).abs() < 1e-5);
    }

    #[test]
    fn a_handover_at_zero_takes_over_on_the_first_step() {
        let (ir, a, b) = two();
        let mut state = AnimationState::new();
        state.set_animation(0, a, true, 1.0, 0.0, 0.0);
        state.add_animation_at(0, b, true, 1.0, 0.0, 0.0, Some(0.0));
        state.update(&ir, 0.1);
        assert_eq!(playing(&state), Some(b));
    }

    #[test]
    fn a_once_entrys_overshoot_is_carried_into_the_queued_entry() {
        let (ir, a, b) = two();
        let mut state = AnimationState::new();
        state.set_animation(0, a, false, 1.0, 0.0, 0.0);
        state.add_animation(0, b, true, 2.0, 0.0, 0.0);
        state.update(&ir, 1.25);
        let e = state.track(0).unwrap().current().unwrap();
        assert_eq!(e.animation, b);
        assert!((e.time - 0.5).abs() < 1e-5, "0.25 s past the end at twice the speed, got {}", e.time);
    }

    #[test]
    fn a_takeover_behind_a_looping_entry_is_the_same_whatever_the_step_size() {
        let (ir, a, b) = two();
        let run = |steps: &[f32]| {
            let mut pose = SkeletonPose::new(ir.clone());
            let mut state = AnimationState::new();
            state.set_animation(0, a, true, 1.0, 0.0, 0.0);
            state.add_animation_at(0, b, true, 1.0, 0.0, 0.4, Some(0.7));
            for dt in steps {
                state.update(&ir, *dt);
            }
            state.apply(&mut pose);
            pose.bones[0].local.rotation
        };
        let coarse = run(&[1.0]);
        let fine = run(&[0.125; 8]);
        assert!((coarse - fine).abs() < 1e-3, "{coarse} vs {fine}");
    }

    #[test]
    fn a_queued_entry_does_not_wait_forever_behind_a_zero_length_loop() {
        let ir = ir_with(vec![Animation::new("empty"), rotate_anim("b", 1.0, 45.0)]);
        let (e, b) = (index_of(&ir, "empty"), index_of(&ir, "b"));
        let mut state = AnimationState::new();
        state.set_animation(0, e, true, 1.0, 0.0, 0.0);
        state.add_animation(0, b, true, 1.0, 0.0, 0.0);
        state.update(&ir, 0.1);
        assert_eq!(playing(&state), Some(b));
    }
}
