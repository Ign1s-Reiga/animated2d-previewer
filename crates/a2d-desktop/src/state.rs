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
    /// The animation the model opens on, when it names one. Selected on a
    /// fresh start and on a model switch; absent or unlisted means the first.
    pub default_animation: Option<String>,
    /// Whether `package` can be opened again from a bare path on the next start.
    ///
    /// False for a model the caller decoded itself (a raw bundle, or a package
    /// only an importer-aware caller can build): its path is not something the
    /// viewer's own package loader understands, so remembering it would only
    /// produce a load failure on every later start.
    pub reopenable: bool,
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
    /// The animation actually showing when it is not the pick: an emotion that
    /// has handed back to the default. `None` while the pick is what shows.
    shown: Option<usize>,
    paused: bool,
    scale: f32,
    offset: Vec2,
    flip_x: bool,
    always_on_top: bool,
    click_through: bool,
    follow_cursor: bool,
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
            shown: None,
            paused: false,
            scale: 1.0,
            offset: Vec2::ZERO,
            flip_x: false,
            always_on_top: true,
            click_through: false,
            follow_cursor: false,
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
        ViewerState::from_config_preferring(config, models, None)
    }

    /// As [`from_config`](ViewerState::from_config), but a model the caller
    /// named (on the command line, say) becomes the active one in preference
    /// to whichever the config remembered. Its remembered settings, if any,
    /// still apply. A `preferred` that is not among `models` is ignored.
    pub fn from_config_preferring(
        config: &Config,
        models: Vec<ModelEntry>,
        preferred: Option<&std::path::Path>,
    ) -> ViewerState {
        let mut state = ViewerState {
            always_on_top: config.window.always_on_top,
            click_through: config.window.click_through,
            follow_cursor: config.window.follow_cursor,
            models,
            ..ViewerState::default()
        };

        // Re-find the active package by path, since the loadable set may be a
        // subset of what the config listed and indices would not line up.
        let wanted = preferred
            .filter(|p| state.models.iter().any(|m| m.package == *p))
            .map(std::path::Path::to_path_buf)
            .or_else(|| config.active_model().map(|m| m.package.clone()));
        if let Some(at) = wanted.as_ref().and_then(|w| state.models.iter().position(|m| m.package == *w)) {
            state.active = at;
        }
        // Precedence: a remembered animation for the model wins (applied
        // below); otherwise the model's default; otherwise the first entry.
        state.animation = state.default_animation_index();
        if let Some(wanted) = wanted {
            if let Some(settings) = config.models.iter().find(|m| m.package == wanted) {
                state.scale = settings.scale.clamp(MIN_SCALE, MAX_SCALE);
                state.offset = Vec2::new(settings.offset.0, settings.offset.1);
                state.flip_x = settings.flip_x;
                if let Some(name) = &settings.animation {
                    state.select_animation_by_name(name);
                }
            }
        }
        state.dirty = false;
        state
    }

    /// Index of the active model's default animation, or 0 when it names none
    /// or names one the list lacks.
    fn default_animation_index(&self) -> usize {
        self.active_model()
            .and_then(|m| {
                let name = m.default_animation.as_deref()?;
                m.animations.iter().position(|a| a == name)
            })
            .unwrap_or(0)
    }

    /// Folds the current state back into a config for saving.
    pub fn write_into(&self, config: &mut Config) {
        config.window.always_on_top = self.always_on_top;
        config.window.click_through = self.click_through;
        config.window.follow_cursor = self.follow_cursor;
        let Some(model) = self.active_model() else { return };
        if !model.reopenable {
            return;
        }

        let at = config.add_or_select(&model.package);
        let entry = ModelConfig {
            package: model.package.clone(),
            // Only a choice that differs from the default is remembered, so
            // later improvements to a model's default still reach it.
            animation: self
                .animation_name()
                .filter(|_| self.animation != self.default_animation_index())
                .map(str::to_string),
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

    /// Index of the animation actually showing: the pick, until a handover moves it.
    pub fn shown_index(&self) -> usize {
        self.shown.unwrap_or(self.animation)
    }

    /// Name of the animation actually showing.
    pub fn shown_name(&self) -> Option<&str> {
        self.active_model()?.animations.get(self.shown_index()).map(String::as_str)
    }

    /// Records what the model reports it is showing. Returns whether that
    /// changed what [`ViewerState::shown_index`] says, so the tray can follow.
    ///
    /// Called every frame, so the common case -- nothing changed -- compares
    /// strings and allocates nothing. Not a user choice: it neither marks the
    /// config dirty nor moves the pick, which is what Tab continues from and
    /// what is remembered.
    pub fn note_shown(&mut self, name: Option<&str>) -> bool {
        let Some(name) = name else { return false };
        if self.shown_name() == Some(name) {
            return false;
        }
        let Some(at) = self.active_model().and_then(|m| m.animations.iter().position(|a| a == name)) else {
            return false;
        };
        self.shown = (at != self.animation).then_some(at);
        true
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

    /// Whether the character should look toward the mouse pointer.
    pub fn follow_cursor(&self) -> bool {
        self.follow_cursor
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
        self.animation = self.default_animation_index();
        self.shown = None;
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
        // Picking what is already picked does nothing, unless it has since handed
        // back to something else: then it is a request to play it again.
        if index >= count || (index == self.animation && self.shown.is_none_or(|s| s == index)) {
            return false;
        }
        self.animation = index;
        self.shown = None;
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
        self.shown = None;
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

    pub fn toggle_follow_cursor(&mut self) -> bool {
        self.follow_cursor = !self.follow_cursor;
        self.dirty = true;
        self.follow_cursor
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
            default_animation: None,
            reopenable: true,
        }
    }

    fn state() -> ViewerState {
        ViewerState {
            models: vec![entry("hero", &["idle", "walk", "wave"]), entry("villain", &["idle", "laugh"])],
            ..ViewerState::default()
        }
    }

    fn with_default(mut e: ModelEntry, name: &str) -> ModelEntry {
        e.default_animation = Some(name.to_string());
        e
    }

    #[test]
    fn a_fresh_start_selects_the_models_default_animation() {
        let models = vec![with_default(entry("hero", &["emote", "idle", "walk"]), "idle")];
        let s = ViewerState::from_config(&Config::default(), models);
        assert_eq!(s.animation_name(), Some("idle"));
        assert!(!s.is_dirty());
    }

    #[test]
    fn switching_model_selects_the_new_models_default() {
        let mut s = state();
        s.models[1] = with_default(entry("villain", &["laugh", "idle"]), "idle");
        assert!(s.select_model(1));
        assert_eq!(s.animation_name(), Some("idle"));
        assert_eq!(s.animation_index(), 1);
    }

    #[test]
    fn a_remembered_animation_beats_the_default() {
        let models = vec![with_default(entry("hero", &["emote", "idle", "walk"]), "idle")];
        let mut config = Config::default();
        let mut model = ModelConfig::new(models[0].package.clone());
        model.animation = Some("walk".into());
        config.models.push(model);
        config.active = 0;
        let s = ViewerState::from_config(&config, models);
        assert_eq!(s.animation_name(), Some("walk"));
    }

    #[test]
    fn a_preferred_model_not_first_opens_on_its_default() {
        let models = vec![entry("hero", &["a", "b", "c"]), with_default(entry("villain", &["laugh", "idle"]), "idle")];
        let s = ViewerState::from_config_preferring(
            &Config::default(),
            models,
            Some(std::path::Path::new("villain.a2dpack")),
        );
        assert_eq!(s.active_index(), 1);
        assert_eq!(s.animation_name(), Some("idle"));
    }

    #[test]
    fn a_config_naming_an_unloadable_package_uses_the_first_models_default() {
        let models = vec![with_default(entry("hero", &["emote", "idle"]), "idle")];
        let mut config = Config::default();
        config.models.push(ModelConfig::new("gone.a2dpack"));
        config.active = 0;
        let s = ViewerState::from_config(&config, models);
        assert_eq!(s.active_index(), 0);
        assert_eq!(s.animation_name(), Some("idle"));
    }

    #[test]
    fn only_a_non_default_animation_is_remembered() {
        let mut s = ViewerState::from_config(
            &Config::default(),
            vec![with_default(entry("hero", &["emote", "idle", "walk"]), "idle")],
        );
        let mut config = Config::default();
        s.write_into(&mut config);
        assert_eq!(config.active_model().unwrap().animation, None);
        s.select_animation_by_name("walk");
        s.write_into(&mut config);
        assert_eq!(config.active_model().unwrap().animation.as_deref(), Some("walk"));
    }

    #[test]
    fn an_absent_or_unknown_default_selects_the_first_animation() {
        let none = ViewerState::from_config(&Config::default(), vec![entry("a", &["emote", "idle"])]);
        assert_eq!(none.animation_index(), 0);
        let unknown = vec![with_default(entry("a", &["emote", "idle"]), "gone")];
        let mut s = ViewerState::from_config(&Config::default(), unknown);
        assert_eq!(s.animation_index(), 0);
        s.models.push(with_default(entry("b", &["x", "y"]), "missing"));
        s.select_animation(1);
        s.select_model(1);
        assert_eq!(s.animation_index(), 0);
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
    fn follow_cursor_is_off_by_default_and_round_trips() {
        let mut s = ViewerState::default();
        assert!(!s.follow_cursor());
        assert!(s.toggle_follow_cursor());
        let mut config = Config::default();
        s.write_into(&mut config);
        assert!(config.window.follow_cursor);
        let back = ViewerState::from_config(&config, Vec::new());
        assert!(back.follow_cursor());
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
    fn a_migrated_relative_entry_matches_its_own_model_and_keeps_its_settings() {
        use crate::config::absolute_path;
        let here = absolute_path(std::path::Path::new("hero.a2dpack"));
        let hero = ModelEntry { package: here.clone(), ..entry("hero", &["idle", "walk"]) };
        let villain = ModelEntry {
            package: absolute_path(std::path::Path::new("villain.a2dpack")),
            ..entry("villain", &["idle"])
        };
        let models = vec![villain.clone(), hero.clone()];

        let mut config = Config::default();
        config.add_or_select(std::path::Path::new("hero.a2dpack"));
        let remembered = config.active_model_mut().unwrap();
        remembered.scale = 2.0;
        remembered.animation = Some("walk".into());
        assert!(config.migrate_relative_entries(|p| p == here));
        assert_eq!(config.models.len(), 1, "one entry, not a relative and an absolute twin");

        // No model named: the remembered (formerly relative) one is active.
        let s = ViewerState::from_config(&config, models.clone());
        assert_eq!(s.active_model().unwrap().display_name, "hero");
        assert_eq!((s.scale(), s.animation_name()), (2.0, Some("walk")));

        // Named on the command line: the same entry, same settings.
        let s = ViewerState::from_config_preferring(&config, models, Some(&here));
        assert_eq!((s.active_model().unwrap().display_name.as_str(), s.scale()), ("hero", 2.0));

        // Saving does not create a second entry.
        s.write_into(&mut config);
        assert_eq!(config.models.len(), 1);
    }

    #[test]
    fn a_named_model_is_active_in_preference_to_the_remembered_one() {
        use std::path::Path;
        let mut config = Config::default();
        config.add_or_select(Path::new("hero.a2dpack"));
        config.active_model_mut().unwrap().scale = 2.0;
        let models = vec![entry("hero", &["idle"]), entry("villain", &["idle", "laugh"])];

        let s = ViewerState::from_config_preferring(&config, models.clone(), Some(Path::new("villain.a2dpack")));
        assert_eq!(s.active_model().unwrap().display_name, "villain");
        assert_eq!(s.scale(), 1.0, "the remembered model's scale belongs to the remembered model");

        let s = ViewerState::from_config_preferring(&config, models, Some(Path::new("gone.a2dpack")));
        assert_eq!(s.active_model().unwrap().display_name, "hero", "an unloadable preference is ignored");
        assert_eq!(s.scale(), 2.0);
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
    fn a_model_that_cannot_be_reopened_from_its_path_is_not_remembered() {
        let mut s = state();
        s.models[0].reopenable = false;
        s.toggle_click_through();
        let mut config = Config::default();
        s.write_into(&mut config);
        assert!(config.models.is_empty(), "{:?}", config.models);
        assert!(config.window.click_through, "window settings are still written");
    }

    #[test]
    fn writing_back_with_no_models_leaves_the_model_list_alone() {
        let s = ViewerState::default();
        let mut config = Config::default();
        s.write_into(&mut config);
        assert!(config.models.is_empty());
    }

    fn emotion_state() -> ViewerState {
        ViewerState {
            models: vec![with_default(entry("hero", &["idle", "smile", "bow"]), "idle")],
            ..ViewerState::default()
        }
    }

    #[test]
    fn the_shown_animation_follows_a_handover_while_the_pick_stays() {
        let mut s = emotion_state();
        s.select_animation_by_name("smile");
        s.clear_dirty();
        assert_eq!(s.shown_name(), Some("smile"));
        assert!(!s.note_shown(Some("smile")), "nothing changed");
        assert!(s.note_shown(Some("idle")), "the emotion handed back to the default");
        assert_eq!(s.shown_name(), Some("idle"));
        assert_eq!(s.shown_index(), 0);
        assert_eq!(s.animation_name(), Some("smile"), "the pick is the user's");
        assert!(!s.is_dirty(), "following playback is not a choice to remember");
        assert!(!s.note_shown(Some("idle")));
        assert!(!s.note_shown(Some("no such animation")));
        assert!(!s.note_shown(None));
    }

    #[test]
    fn next_continues_from_the_pick_not_from_what_it_returned_to() {
        let mut s = emotion_state();
        s.select_animation_by_name("smile");
        s.note_shown(Some("idle"));
        assert!(s.next_animation());
        assert_eq!(s.animation_name(), Some("bow"), "smile, not idle, was the last pick");
        assert_eq!(s.shown_name(), Some("bow"), "a new pick shows as itself at once");
    }

    #[test]
    fn the_config_remembers_the_pick_not_the_returned_to_animation() {
        let mut s = emotion_state();
        s.select_animation_by_name("smile");
        s.note_shown(Some("idle"));
        let mut config = Config::default();
        s.write_into(&mut config);
        assert_eq!(config.active_model().unwrap().animation.as_deref(), Some("smile"));
        // A remembered emotion is picked again on the next start.
        let again = ViewerState::from_config(&config, s.models().to_vec());
        assert_eq!(again.animation_name(), Some("smile"));
    }

    #[test]
    fn a_pick_of_the_default_is_still_stored_as_none() {
        let mut s = emotion_state();
        s.select_animation_by_name("smile");
        s.select_animation_by_name("idle");
        let mut config = Config::default();
        s.write_into(&mut config);
        assert_eq!(config.active_model().unwrap().animation, None);
    }

    #[test]
    fn picking_the_same_emotion_again_replays_it_only_after_it_handed_back() {
        let mut s = emotion_state();
        s.select_animation(1);
        assert!(!s.select_animation(1), "still playing: nothing to do");
        s.note_shown(Some("idle"));
        assert!(s.select_animation(1), "it has returned, so this asks for it again");
        assert_eq!(s.shown_name(), Some("smile"));
    }

    #[test]
    fn switching_model_forgets_what_was_shown() {
        let mut s = state();
        s.select_animation_by_name("walk");
        s.note_shown(Some("idle"));
        s.next_model();
        assert_eq!(s.shown_index(), s.animation_index());
    }
}
