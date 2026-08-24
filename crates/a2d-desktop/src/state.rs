//! Viewer state: what is shown, how it is placed, and what the user is doing.
//!
//! Deliberately free of `winit` and of any GPU type. Dragging, scaling,
//! selection and click-through are where the behaviour a user actually notices
//! lives, and keeping them here means they can be tested without opening a
//! window — which on a headless machine is the difference between tested and
//! not.

use std::path::PathBuf;

use a2d_core::Vec2;

use crate::config::{Config, ModelConfig, MAX_SCALE, MIN_SCALE};

/// A character the viewer knows how to show.
#[derive(Debug, Clone, PartialEq)]
pub struct ModelEntry {
    pub package: PathBuf,
    pub display_name: String,
    /// Animation names, in the order the selector cycles through them.
    pub animations: Vec<String>,
}

/// A drag in progress.
#[derive(Debug, Clone, Copy, PartialEq)]
struct Drag {
    /// Cursor position within the window when the drag began, in physical
    /// pixels. The window moves so that the cursor stays over this same point.
    grab: Vec2,
}

/// What the viewer should do next, decided by the state and carried out by the
/// platform layer.
#[derive(Debug, Clone, Copy, PartialEq)]
pub enum Action {
    /// Move the window's outer position by this delta, in physical pixels.
    MoveWindow(Vec2),
    /// Nothing to do.
    None,
}

/// The mutable state of the viewer.
#[derive(Debug, Clone, PartialEq)]
pub struct ViewerState {
    models: Vec<ModelEntry>,
    active: usize,
    animation: usize,
    paused: bool,
    scale: f32,
    offset: Vec2,
    flip_x: bool,
    always_on_top: bool,
    click_through: bool,
    drag: Option<Drag>,
    /// Set whenever something changed that the config should remember.
    dirty: bool,
}

impl Default for ViewerState {
    fn default() -> Self {
        ViewerState {
            models: Vec::new(),
            active: 0,
            animation: 0,
            paused: false,
            scale: 1.0,
            offset: Vec2::ZERO,
            flip_x: false,
            always_on_top: true,
            click_through: false,
            drag: None,
            dirty: false,
        }
    }
}

impl ViewerState {
    /// Builds from a config plus the models that were actually loadable.
    ///
    /// `models` is what loaded, not what the config listed: a package that has
    /// been deleted since last run should not leave a selector entry that
    /// cannot be selected.
    pub fn from_config(config: &Config, models: Vec<ModelEntry>) -> ViewerState {
        let mut state = ViewerState {
            always_on_top: config.window.always_on_top,
            click_through: config.window.click_through,
            models,
            ..ViewerState::default()
        };

        // Re-find the active package by path, since the loadable set may be a
        // subset of what the config listed and indices would not line up.
        if let Some(active) = config.active_model() {
            if let Some(at) = state.models.iter().position(|m| m.package == active.package) {
                state.active = at;
            }
            state.scale = active.scale.clamp(MIN_SCALE, MAX_SCALE);
            state.offset = Vec2::new(active.offset.0, active.offset.1);
            state.flip_x = active.flip_x;
            if let Some(name) = &active.animation {
                state.select_animation_by_name(name);
            }
        }
        state.dirty = false;
        state
    }

    /// Folds the current state back into a config for saving.
    pub fn write_into(&self, config: &mut Config) {
        config.window.always_on_top = self.always_on_top;
        config.window.click_through = self.click_through;
        let Some(model) = self.active_model() else { return };

        let at = config.add_or_select(&model.package);
        let entry = ModelConfig {
            package: model.package.clone(),
            animation: self.animation_name().map(str::to_string),
            scale: self.scale,
            offset: (self.offset.x, self.offset.y),
            flip_x: self.flip_x,
        };
        config.models[at] = entry;
    }

    pub fn models(&self) -> &[ModelEntry] {
        &self.models
    }

    pub fn active_index(&self) -> usize {
        self.active
    }

    pub fn active_model(&self) -> Option<&ModelEntry> {
        self.models.get(self.active)
    }

    pub fn animation_index(&self) -> usize {
        self.animation
    }

    pub fn animation_name(&self) -> Option<&str> {
        self.active_model()?.animations.get(self.animation).map(String::as_str)
    }

    pub fn is_paused(&self) -> bool {
        self.paused
    }

    pub fn scale(&self) -> f32 {
        self.scale
    }

    pub fn offset(&self) -> Vec2 {
        self.offset
    }

    pub fn flip_x(&self) -> bool {
        self.flip_x
    }

    pub fn always_on_top(&self) -> bool {
        self.always_on_top
    }

    pub fn click_through(&self) -> bool {
        self.click_through
    }

    pub fn is_dragging(&self) -> bool {
        self.drag.is_some()
    }

    /// Whether anything changed that the config should be told about.
    pub fn is_dirty(&self) -> bool {
        self.dirty
    }

    pub fn clear_dirty(&mut self) {
        self.dirty = false;
    }

    // ------------------------------------------------------------ selection

    /// Selects a model by index. Out-of-range indices are ignored.
    pub fn select_model(&mut self, index: usize) -> bool {
        if index >= self.models.len() || index == self.active {
            return false;
        }
        self.active = index;
        // A different character has different animations, so the current index
        // would otherwise point at an unrelated one.
        self.animation = 0;
        self.dirty = true;
        true
    }

    /// Moves to the next model, wrapping. No-op with fewer than two models.
    pub fn next_model(&mut self) -> bool {
        if self.models.len() < 2 {
            return false;
        }
        self.select_model((self.active + 1) % self.models.len())
    }

    pub fn previous_model(&mut self) -> bool {
        if self.models.len() < 2 {
            return false;
        }
        self.select_model((self.active + self.models.len() - 1) % self.models.len())
    }

    pub fn select_animation(&mut self, index: usize) -> bool {
        let count = self.active_model().map_or(0, |m| m.animations.len());
        if index >= count || index == self.animation {
            return false;
        }
        self.animation = index;
        self.dirty = true;
        true
    }

    /// Selects an animation by name. Returns false when the model has no such
    /// animation, leaving the current selection alone.
    pub fn select_animation_by_name(&mut self, name: &str) -> bool {
        let Some(at) = self.active_model().and_then(|m| m.animations.iter().position(|a| a == name)) else {
            return false;
        };
        self.animation = at;
        self.dirty = true;
        true
    }

    pub fn next_animation(&mut self) -> bool {
        let count = self.active_model().map_or(0, |m| m.animations.len());
        if count < 2 {
            return false;
        }
        self.select_animation((self.animation + 1) % count)
    }

    // ------------------------------------------------------------ playback

    pub fn toggle_pause(&mut self) -> bool {
        self.paused = !self.paused;
        self.paused
    }

    pub fn set_paused(&mut self, paused: bool) {
        self.paused = paused;
    }

    // ------------------------------------------------------------ placement

    /// Sets the scale, clamped into the usable range.
    pub fn set_scale(&mut self, scale: f32) {
        let clamped = if scale.is_finite() { scale.clamp(MIN_SCALE, MAX_SCALE) } else { self.scale };
        if clamped != self.scale {
            self.scale = clamped;
            self.dirty = true;
        }
    }

    /// Multiplies the scale, as a scroll wheel would.
    pub fn zoom_by(&mut self, factor: f32) {
        if factor.is_finite() && factor > 0.0 {
            self.set_scale(self.scale * factor);
        }
    }

    pub fn set_offset(&mut self, offset: Vec2) {
        if offset.is_finite() && offset != self.offset {
            self.offset = offset;
            self.dirty = true;
        }
    }

    pub fn toggle_flip(&mut self) -> bool {
        self.flip_x = !self.flip_x;
        self.dirty = true;
        self.flip_x
    }

    pub fn toggle_always_on_top(&mut self) -> bool {
        self.always_on_top = !self.always_on_top;
        self.dirty = true;
        self.always_on_top
    }

    /// Toggles click-through.
    ///
    /// Turning it on cancels any drag in progress: once clicks pass through,
    /// no further mouse events arrive and the drag could never be ended.
    pub fn toggle_click_through(&mut self) -> bool {
        self.click_through = !self.click_through;
        if self.click_through {
            self.drag = None;
        }
        self.dirty = true;
        self.click_through
    }

    // ------------------------------------------------------------ dragging

    /// Begins dragging the window from a cursor position inside it.
    ///
    /// Ignored while click-through is on, since the window receives no clicks.
    pub fn begin_drag(&mut self, cursor: Vec2) -> bool {
        if self.click_through || !cursor.is_finite() {
            return false;
        }
        self.drag = Some(Drag { grab: cursor });
        true
    }

    /// Continues a drag.
    ///
    /// The window moves by the cursor's displacement from where it was grabbed.
    /// Once the window has moved, the cursor sits back over the grab point, so
    /// the grab does not need updating — which is what keeps the character from
    /// sliding out from under the pointer.
    pub fn drag_to(&mut self, cursor: Vec2) -> Action {
        let Some(drag) = self.drag else { return Action::None };
        if !cursor.is_finite() {
            return Action::None;
        }
        let delta = cursor - drag.grab;
        if delta == Vec2::ZERO {
            return Action::None;
        }
        Action::MoveWindow(delta)
    }

    pub fn end_drag(&mut self) -> bool {
        self.drag.take().is_some()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::config::WindowConfig;

    fn entry(name: &str, animations: &[&str]) -> ModelEntry {
        ModelEntry {
            package: PathBuf::from(format!("{name}.a2dpack")),
            display_name: name.to_string(),
            animations: animations.iter().map(|a| a.to_string()).collect(),
        }
    }

    fn state() -> ViewerState {
        ViewerState {
            models: vec![entry("hero", &["idle", "walk", "wave"]), entry("villain", &["idle", "laugh"])],
            ..ViewerState::default()
        }
    }

    #[test]
    fn a_fresh_state_shows_the_first_model_and_animation() {
        let s = state();
        assert_eq!(s.active_model().unwrap().display_name, "hero");
        assert_eq!(s.animation_name(), Some("idle"));
        assert!(!s.is_paused());
        assert!(!s.is_dirty());
    }

    #[test]
    fn an_empty_state_has_nothing_selected() {
        let mut s = ViewerState::default();
        assert!(s.active_model().is_none());
        assert_eq!(s.animation_name(), None);
        assert!(!s.next_model());
        assert!(!s.next_animation());
    }

    #[test]
    fn cycling_models_wraps() {
        let mut s = state();
        assert!(s.next_model());
        assert_eq!(s.active_index(), 1);
        assert!(s.next_model());
        assert_eq!(s.active_index(), 0, "should wrap back to the first");
        assert!(s.previous_model());
        assert_eq!(s.active_index(), 1);
    }

    #[test]
    fn switching_model_resets_the_animation_selection() {
        // The new character's animation list is unrelated, so keeping the index
        // would land on something arbitrary.
        let mut s = state();
        s.next_animation();
        s.next_animation();
        assert_eq!(s.animation_index(), 2);
        s.next_model();
        assert_eq!(s.animation_index(), 0);
        assert_eq!(s.animation_name(), Some("idle"));
    }

    #[test]
    fn a_single_model_does_not_cycle() {
        let mut s = ViewerState { models: vec![entry("hero", &["idle"])], ..ViewerState::default() };
        assert!(!s.next_model());
        assert!(!s.previous_model());
        assert_eq!(s.active_index(), 0);
    }

    #[test]
    fn cycling_animations_wraps_within_the_active_model() {
        let mut s = state();
        assert!(s.next_animation());
        assert_eq!(s.animation_name(), Some("walk"));
        s.next_animation();
        assert_eq!(s.animation_name(), Some("wave"));
        s.next_animation();
        assert_eq!(s.animation_name(), Some("idle"), "should wrap");
    }

    #[test]
    fn selecting_an_animation_by_name_finds_it() {
        let mut s = state();
        assert!(s.select_animation_by_name("wave"));
        assert_eq!(s.animation_index(), 2);
    }

    #[test]
    fn selecting_an_unknown_animation_leaves_the_selection_alone() {
        let mut s = state();
        s.select_animation_by_name("wave");
        assert!(!s.select_animation_by_name("backflip"));
        assert_eq!(s.animation_name(), Some("wave"));
    }

    #[test]
    fn out_of_range_selections_are_ignored() {
        let mut s = state();
        assert!(!s.select_model(9));
        assert!(!s.select_animation(9));
        assert_eq!(s.active_index(), 0);
        assert_eq!(s.animation_index(), 0);
    }

    #[test]
    fn pause_toggles() {
        let mut s = state();
        assert!(s.toggle_pause());
        assert!(s.is_paused());
        assert!(!s.toggle_pause());
        assert!(!s.is_paused());
    }

    #[test]
    fn scale_is_clamped_into_the_usable_range() {
        let mut s = state();
        s.set_scale(1000.0);
        assert_eq!(s.scale(), MAX_SCALE);
        s.set_scale(0.0);
        assert_eq!(s.scale(), MIN_SCALE);
    }

    #[test]
    fn a_non_finite_scale_is_rejected_rather_than_applied() {
        let mut s = state();
        s.set_scale(2.0);
        for bad in [f32::NAN, f32::INFINITY] {
            s.set_scale(bad);
            assert_eq!(s.scale(), 2.0, "for {bad}");
        }
    }

    #[test]
    fn zooming_multiplies_and_stays_in_range() {
        let mut s = state();
        s.zoom_by(2.0);
        assert_eq!(s.scale(), 2.0);
        s.zoom_by(0.5);
        assert_eq!(s.scale(), 1.0);
        // Repeated zoom-in must saturate rather than run away.
        for _ in 0..50 {
            s.zoom_by(2.0);
        }
        assert_eq!(s.scale(), MAX_SCALE);
    }

    #[test]
    fn a_nonsense_zoom_factor_is_ignored() {
        let mut s = state();
        for bad in [0.0, -1.0, f32::NAN] {
            s.zoom_by(bad);
            assert_eq!(s.scale(), 1.0, "for {bad}");
        }
    }

    #[test]
    fn dragging_moves_the_window_by_the_cursor_displacement() {
        let mut s = state();
        assert!(s.begin_drag(Vec2::new(100.0, 50.0)));
        assert!(s.is_dragging());
        assert_eq!(s.drag_to(Vec2::new(110.0, 70.0)), Action::MoveWindow(Vec2::new(10.0, 20.0)));
        assert!(s.end_drag());
        assert!(!s.is_dragging());
    }

    #[test]
    fn the_grab_point_is_not_updated_during_a_drag() {
        // The window moves under the cursor, so the cursor returns to the grab
        // point. Advancing the grab would make the character accelerate away.
        let mut s = state();
        s.begin_drag(Vec2::new(100.0, 100.0));
        assert_eq!(s.drag_to(Vec2::new(105.0, 100.0)), Action::MoveWindow(Vec2::new(5.0, 0.0)));
        assert_eq!(s.drag_to(Vec2::new(105.0, 100.0)), Action::MoveWindow(Vec2::new(5.0, 0.0)));
    }

    #[test]
    fn moving_without_a_drag_does_nothing() {
        let mut s = state();
        assert_eq!(s.drag_to(Vec2::new(10.0, 10.0)), Action::None);
        assert!(!s.end_drag(), "ending a drag that never began is a no-op");
    }

    #[test]
    fn a_zero_displacement_produces_no_action() {
        let mut s = state();
        s.begin_drag(Vec2::new(10.0, 10.0));
        assert_eq!(s.drag_to(Vec2::new(10.0, 10.0)), Action::None);
    }

    #[test]
    fn a_non_finite_cursor_is_ignored() {
        let mut s = state();
        assert!(!s.begin_drag(Vec2::new(f32::NAN, 0.0)));
        s.begin_drag(Vec2::new(10.0, 10.0));
        assert_eq!(s.drag_to(Vec2::new(f32::NAN, 10.0)), Action::None);
    }

    #[test]
    fn click_through_prevents_dragging() {
        let mut s = state();
        s.toggle_click_through();
        assert!(!s.begin_drag(Vec2::new(10.0, 10.0)));
        assert!(!s.is_dragging());
    }

    #[test]
    fn enabling_click_through_cancels_a_drag_in_progress() {
        // Otherwise the drag could never end: no further clicks would arrive.
        let mut s = state();
        s.begin_drag(Vec2::new(10.0, 10.0));
        s.toggle_click_through();
        assert!(!s.is_dragging());
    }

    #[test]
    fn toggles_report_their_new_value() {
        let mut s = state();
        assert!(s.toggle_flip());
        assert!(s.flip_x());
        assert!(!s.toggle_always_on_top(), "the default is on, so toggling turns it off");
        assert!(!s.always_on_top());
    }

    #[test]
    fn changes_mark_the_state_dirty_so_settings_get_saved() {
        let mut s = state();
        assert!(!s.is_dirty());
        s.set_scale(2.0);
        assert!(s.is_dirty());
        s.clear_dirty();
        assert!(!s.is_dirty());
        s.next_model();
        assert!(s.is_dirty());
    }

    #[test]
    fn a_no_op_change_does_not_mark_the_state_dirty() {
        let mut s = state();
        s.set_scale(1.0);
        assert!(!s.is_dirty(), "setting the value it already had is not a change");
        s.select_model(0);
        assert!(!s.is_dirty());
    }

    #[test]
    fn state_is_restored_from_a_config() {
        let mut config =
            Config { window: WindowConfig { always_on_top: false, ..Default::default() }, ..Default::default() };
        config.add_or_select(std::path::Path::new("villain.a2dpack"));
        let model = config.active_model_mut().unwrap();
        model.animation = Some("laugh".into());
        model.scale = 2.0;
        model.offset = (5.0, -5.0);
        model.flip_x = true;

        let s = ViewerState::from_config(&config, vec![entry("hero", &["idle"]), entry("villain", &["idle", "laugh"])]);
        assert_eq!(s.active_model().unwrap().display_name, "villain");
        assert_eq!(s.animation_name(), Some("laugh"));
        assert_eq!(s.scale(), 2.0);
        assert_eq!(s.offset(), Vec2::new(5.0, -5.0));
        assert!(s.flip_x());
        assert!(!s.always_on_top());
        assert!(!s.is_dirty(), "restoring is not a change");
    }

    #[test]
    fn a_config_naming_a_model_that_no_longer_loads_falls_back_to_the_first() {
        let mut config = Config::default();
        config.add_or_select(std::path::Path::new("deleted.a2dpack"));
        let s = ViewerState::from_config(&config, vec![entry("hero", &["idle"])]);
        assert_eq!(s.active_index(), 0);
        assert_eq!(s.active_model().unwrap().display_name, "hero");
    }

    #[test]
    fn a_config_naming_an_animation_that_no_longer_exists_falls_back() {
        let mut config = Config::default();
        config.add_or_select(std::path::Path::new("hero.a2dpack"));
        config.active_model_mut().unwrap().animation = Some("removed".into());
        let s = ViewerState::from_config(&config, vec![entry("hero", &["idle", "walk"])]);
        assert_eq!(s.animation_name(), Some("idle"));
    }

    #[test]
    fn state_writes_back_into_a_config_for_saving() {
        let mut s = state();
        s.next_model();
        s.select_animation_by_name("laugh");
        s.set_scale(1.5);
        s.set_offset(Vec2::new(3.0, 4.0));
        s.toggle_flip();
        s.toggle_always_on_top();

        let mut config = Config::default();
        s.write_into(&mut config);

        let model = config.active_model().unwrap();
        assert_eq!(model.package, PathBuf::from("villain.a2dpack"));
        assert_eq!(model.animation.as_deref(), Some("laugh"));
        assert_eq!(model.scale, 1.5);
        assert_eq!(model.offset, (3.0, 4.0));
        assert!(model.flip_x);
        assert!(!config.window.always_on_top);
    }

    #[test]
    fn a_config_round_trips_through_state_unchanged() {
        let models = vec![entry("hero", &["idle", "walk", "wave"]), entry("villain", &["idle", "laugh"])];
        let mut config = Config::default();
        config.add_or_select(std::path::Path::new("hero.a2dpack"));
        let model = config.active_model_mut().unwrap();
        model.animation = Some("wave".into());
        model.scale = 1.25;
        model.offset = (2.0, -3.0);

        let state = ViewerState::from_config(&config, models);
        let mut round_tripped = Config::default();
        state.write_into(&mut round_tripped);

        assert_eq!(round_tripped.active_model(), config.active_model());
    }

    #[test]
    fn writing_back_with_no_models_leaves_the_model_list_alone() {
        let s = ViewerState::default();
        let mut config = Config::default();
        s.write_into(&mut config);
        assert!(config.models.is_empty());
    }
}
