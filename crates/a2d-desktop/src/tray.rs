//! System tray icon and menu.
//!
//! The tray is the only always-available control surface: a frameless window
//! has no title bar to right-click and, in click-through mode, receives no
//! mouse events at all. Everything the keyboard shortcuts do is reachable here
//! too, so turning click-through on can always be undone.

use tray_icon::menu::{CheckMenuItem, Menu, MenuEvent, MenuItem, PredefinedMenuItem, Submenu};
use tray_icon::{TrayIcon, TrayIconBuilder};

use crate::state::ViewerState;

/// Why the tray could not be created.
///
/// The three underlying crates each have their own error type, and a tray that
/// fails is not fatal — the window and its shortcuts still work — so the caller
/// needs one type it can report and move on from.
#[derive(Debug, thiserror::Error)]
pub enum TrayError {
    #[error("tray icon: {0}")]
    Icon(#[from] tray_icon::Error),
    #[error("tray menu: {0}")]
    Menu(#[from] tray_icon::menu::Error),
    #[error("tray icon image: {0}")]
    Image(#[from] tray_icon::BadIcon),
}

/// Something the user asked for.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TrayCommand {
    Quit,
    TogglePause,
    ToggleAlwaysOnTop,
    ToggleClickThrough,
    NextAnimation,
    SelectAnimation(usize),
    NextModel,
    SelectModel(usize),
    ResetPlacement,
}

/// Fixed menu ids. Per-item ids for models and animations are generated.
const ID_QUIT: &str = "quit";
const ID_PAUSE: &str = "pause";
const ID_TOP: &str = "always-on-top";
const ID_CLICK: &str = "click-through";
const ID_RESET: &str = "reset-placement";

/// Maps a menu id back to the command it stands for.
///
/// Kept as a pure function so the id scheme is testable without building a
/// tray, which needs a platform event loop.
pub fn command_for(id: &str) -> Option<TrayCommand> {
    match id {
        ID_QUIT => return Some(TrayCommand::Quit),
        ID_PAUSE => return Some(TrayCommand::TogglePause),
        ID_TOP => return Some(TrayCommand::ToggleAlwaysOnTop),
        ID_CLICK => return Some(TrayCommand::ToggleClickThrough),
        ID_RESET => return Some(TrayCommand::ResetPlacement),
        _ => {}
    }
    if let Some(rest) = id.strip_prefix("model:") {
        return rest.parse().ok().map(TrayCommand::SelectModel);
    }
    if let Some(rest) = id.strip_prefix("animation:") {
        return rest.parse().ok().map(TrayCommand::SelectAnimation);
    }
    None
}

pub fn model_id(index: usize) -> String {
    format!("model:{index}")
}

pub fn animation_id(index: usize) -> String {
    format!("animation:{index}")
}

/// The tray icon and the check items that need updating when state changes.
pub struct Tray {
    _icon: TrayIcon,
    pause: CheckMenuItem,
    always_on_top: CheckMenuItem,
    click_through: CheckMenuItem,
}

impl std::fmt::Debug for Tray {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Tray").finish_non_exhaustive()
    }
}

impl Tray {
    /// Builds the tray from the current state.
    pub fn new(state: &ViewerState) -> Result<Tray, TrayError> {
        let menu = Menu::new();

        // Models, when there is a choice to make.
        if state.models().len() > 1 {
            let models = Submenu::new("Model", true);
            for (i, entry) in state.models().iter().enumerate() {
                models.append(&CheckMenuItem::with_id(
                    model_id(i),
                    &entry.display_name,
                    true,
                    i == state.active_index(),
                    None,
                ))?;
            }
            menu.append(&models)?;
        }

        if let Some(model) = state.active_model() {
            if !model.animations.is_empty() {
                let animations = Submenu::new("Animation", true);
                for (i, name) in model.animations.iter().enumerate() {
                    animations.append(&CheckMenuItem::with_id(
                        animation_id(i),
                        name,
                        true,
                        i == state.animation_index(),
                        None,
                    ))?;
                }
                menu.append(&animations)?;
            }
        }
        menu.append(&PredefinedMenuItem::separator())?;

        let pause = CheckMenuItem::with_id(ID_PAUSE, "Pause", true, state.is_paused(), None);
        let always_on_top = CheckMenuItem::with_id(ID_TOP, "Always on top", true, state.always_on_top(), None);
        let click_through = CheckMenuItem::with_id(ID_CLICK, "Click through", true, state.click_through(), None);
        menu.append(&pause)?;
        menu.append(&always_on_top)?;
        menu.append(&click_through)?;
        menu.append(&MenuItem::with_id(ID_RESET, "Reset size and position", true, None))?;
        menu.append(&PredefinedMenuItem::separator())?;
        menu.append(&MenuItem::with_id(ID_QUIT, "Quit", true, None))?;

        let icon = TrayIconBuilder::new()
            .with_menu(Box::new(menu))
            .with_tooltip("animated2d")
            .with_icon(tray_icon::Icon::from_rgba(icon_rgba(32), 32, 32)?)
            .build()?;

        Ok(Tray { _icon: icon, pause, always_on_top, click_through })
    }

    /// Takes the next pending menu activation, if any.
    pub fn poll(&self) -> Option<TrayCommand> {
        // Non-blocking: this is called once per frame from the event loop.
        MenuEvent::receiver().try_recv().ok().and_then(|event| command_for(event.id.as_ref()))
    }

    /// Brings the check marks back in line with the state.
    pub fn sync(&mut self, state: &ViewerState) {
        self.pause.set_checked(state.is_paused());
        self.always_on_top.set_checked(state.always_on_top());
        self.click_through.set_checked(state.click_through());
    }
}

/// Builds the tray icon as RGBA pixels.
///
/// Drawn rather than embedded so no binary asset has to be committed, and so
/// the icon scales to whatever size a platform asks for. A filled circle with a
/// transparent surround reads clearly at 16px, which is where it will mostly be
/// seen.
pub fn icon_rgba(size: u32) -> Vec<u8> {
    let mut pixels = Vec::with_capacity((size * size * 4) as usize);
    let center = (size as f32 - 1.0) / 2.0;
    let radius = size as f32 * 0.42;

    for y in 0..size {
        for x in 0..size {
            let dx = x as f32 - center;
            let dy = y as f32 - center;
            let distance = (dx * dx + dy * dy).sqrt();
            // One pixel of feathering, so the edge is not jagged.
            let coverage = ((radius - distance) + 0.5).clamp(0.0, 1.0);
            let alpha = (coverage * 255.0).round() as u8;
            // A vertical gradient, so the icon is not a flat disc.
            let shade = 90 + (140.0 * (1.0 - y as f32 / size as f32)) as u8;
            pixels.extend_from_slice(&[shade, shade / 2, 200, alpha]);
        }
    }
    pixels
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn fixed_ids_map_to_their_commands() {
        assert_eq!(command_for(ID_QUIT), Some(TrayCommand::Quit));
        assert_eq!(command_for(ID_PAUSE), Some(TrayCommand::TogglePause));
        assert_eq!(command_for(ID_TOP), Some(TrayCommand::ToggleAlwaysOnTop));
        assert_eq!(command_for(ID_CLICK), Some(TrayCommand::ToggleClickThrough));
        assert_eq!(command_for(ID_RESET), Some(TrayCommand::ResetPlacement));
    }

    #[test]
    fn generated_ids_round_trip_through_their_command() {
        for i in [0usize, 1, 7, 1000] {
            assert_eq!(command_for(&model_id(i)), Some(TrayCommand::SelectModel(i)));
            assert_eq!(command_for(&animation_id(i)), Some(TrayCommand::SelectAnimation(i)));
        }
    }

    #[test]
    fn model_and_animation_ids_do_not_collide() {
        assert_ne!(model_id(3), animation_id(3));
        assert_eq!(command_for(&model_id(3)), Some(TrayCommand::SelectModel(3)));
        assert_eq!(command_for(&animation_id(3)), Some(TrayCommand::SelectAnimation(3)));
    }

    #[test]
    fn an_unknown_id_is_ignored_rather_than_guessed_at() {
        for id in ["", "nonsense", "model:", "model:abc", "animation:-1", "quit2"] {
            assert_eq!(command_for(id), None, "for {id:?}");
        }
    }

    #[test]
    fn the_icon_has_the_expected_size_and_layout() {
        let pixels = icon_rgba(32);
        assert_eq!(pixels.len(), 32 * 32 * 4);
    }

    #[test]
    fn the_icon_is_opaque_in_the_middle_and_transparent_at_the_corners() {
        let size = 32u32;
        let pixels = icon_rgba(size);
        let alpha_at = |x: u32, y: u32| pixels[((y * size + x) * 4 + 3) as usize];

        assert_eq!(alpha_at(size / 2, size / 2), 255, "the centre should be solid");
        assert_eq!(alpha_at(0, 0), 0, "the corners should be clear");
        assert_eq!(alpha_at(size - 1, size - 1), 0);
    }

    #[test]
    fn the_icon_renders_at_every_size_a_platform_might_ask_for() {
        for size in [16u32, 24, 32, 48, 64, 256] {
            let pixels = icon_rgba(size);
            assert_eq!(pixels.len(), (size * size * 4) as usize, "size {size}");
            let centre = ((size / 2 * size + size / 2) * 4 + 3) as usize;
            assert_eq!(pixels[centre], 255, "size {size} should have a solid centre");
        }
    }

    #[test]
    fn a_one_pixel_icon_does_not_panic() {
        // Not a size anyone asks for, but the arithmetic divides by `size`.
        assert_eq!(icon_rgba(1).len(), 4);
    }
}
