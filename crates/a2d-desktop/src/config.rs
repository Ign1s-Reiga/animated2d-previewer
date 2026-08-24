//! Configuration persistence.
//!
//! Spec §13 asks the viewer to remember its last position and model. That means
//! reading a file written by an older build, possibly hand-edited, possibly
//! from a machine with a different display layout — so every value is clamped
//! on load and a malformed file degrades to defaults with a report rather than
//! stopping the viewer from opening.

use std::path::{Path, PathBuf};

use a2d_core::{DecodeError, LoadReport};
use serde::{Deserialize, Serialize};

/// Current config layout version. Bump when a field changes meaning.
pub const CONFIG_VERSION: u32 = 1;

/// Scale limits. Below the minimum a character is invisible; above the maximum
/// the window would exceed any real display.
pub const MIN_SCALE: f32 = 0.1;
pub const MAX_SCALE: f32 = 8.0;

/// Window size limits, in logical pixels.
pub const MIN_WINDOW: u32 = 64;
pub const MAX_WINDOW: u32 = 8192;

/// How the window itself should be set up.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct WindowConfig {
    /// Last outer position in physical pixels. `None` lets the OS place it.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub position: Option<(i32, i32)>,
    pub size: (u32, u32),
    pub always_on_top: bool,
    /// When true, clicks pass through to whatever is behind the character.
    pub click_through: bool,
}

impl Default for WindowConfig {
    fn default() -> Self {
        WindowConfig { position: None, size: (480, 640), always_on_top: true, click_through: false }
    }
}

/// Per-character settings, remembered across sessions.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ModelConfig {
    /// Path to the `.a2dpack` directory.
    pub package: PathBuf,
    /// Animation to start on. `None` uses the package's default.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub animation: Option<String>,
    pub scale: f32,
    /// Offset within the window, in logical pixels from the centre.
    pub offset: (f32, f32),
    /// Mirrors the character horizontally.
    #[serde(default)]
    pub flip_x: bool,
}

impl ModelConfig {
    pub fn new(package: impl Into<PathBuf>) -> ModelConfig {
        ModelConfig { package: package.into(), animation: None, scale: 1.0, offset: (0.0, 0.0), flip_x: false }
    }

    /// Display name, derived from the package directory's file stem.
    pub fn display_name(&self) -> String {
        self.package
            .file_stem()
            .and_then(|s| s.to_str())
            .map(str::to_string)
            .unwrap_or_else(|| self.package.display().to_string())
    }
}

/// The whole persisted configuration.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Config {
    pub version: u32,
    pub window: WindowConfig,
    /// Every package the viewer knows about, in the order they were added.
    #[serde(default)]
    pub models: Vec<ModelConfig>,
    /// Index into `models`. Out-of-range values are clamped on load.
    #[serde(default)]
    pub active: usize,
}

impl Default for Config {
    fn default() -> Self {
        Config { version: CONFIG_VERSION, window: WindowConfig::default(), models: Vec::new(), active: 0 }
    }
}

impl Config {
    /// The active model, if there is one.
    pub fn active_model(&self) -> Option<&ModelConfig> {
        self.models.get(self.active)
    }

    pub fn active_model_mut(&mut self) -> Option<&mut ModelConfig> {
        self.models.get_mut(self.active)
    }

    /// Adds a package, or selects it if already known.
    ///
    /// Returns its index. Paths are compared as given: normalising them would
    /// need the filesystem, and a package that has moved is a new entry rather
    /// than a silent rebinding of the old one's settings.
    pub fn add_or_select(&mut self, package: &Path) -> usize {
        if let Some(at) = self.models.iter().position(|m| m.package == package) {
            self.active = at;
            return at;
        }
        self.models.push(ModelConfig::new(package));
        self.active = self.models.len() - 1;
        self.active
    }

    /// Clamps every value into a usable range, reporting what it changed.
    ///
    /// Called on load, so a hand-edited or stale file cannot produce a window
    /// nobody can see or a scale that renders nothing.
    pub fn sanitize(&mut self, report: &mut LoadReport) {
        let (w, h) = self.window.size;
        let clamped = (w.clamp(MIN_WINDOW, MAX_WINDOW), h.clamp(MIN_WINDOW, MAX_WINDOW));
        if clamped != self.window.size {
            report.note(format!("window size {w}x{h} clamped to {}x{}", clamped.0, clamped.1));
            self.window.size = clamped;
        }

        for model in &mut self.models {
            if !model.scale.is_finite() || model.scale < MIN_SCALE || model.scale > MAX_SCALE {
                let was = model.scale;
                model.scale = if model.scale.is_finite() { model.scale.clamp(MIN_SCALE, MAX_SCALE) } else { 1.0 };
                report.note(format!("scale {was} for {:?} clamped to {}", model.display_name(), model.scale));
            }
            if !model.offset.0.is_finite() || !model.offset.1.is_finite() {
                report.note(format!("non-finite offset for {:?} reset", model.display_name()));
                model.offset = (0.0, 0.0);
            }
        }

        if !self.models.is_empty() && self.active >= self.models.len() {
            report.note(format!("active model index {} is out of range, using the first", self.active));
            self.active = 0;
        }
    }

    /// Serialises to deterministic pretty JSON with a trailing newline.
    pub fn to_json(&self) -> Result<String, DecodeError> {
        for model in &self.models {
            if !model.scale.is_finite() || !model.offset.0.is_finite() || !model.offset.1.is_finite() {
                return Err(DecodeError::corrupt(format!(
                    "config cannot be serialised: {:?} has a non-finite scale or offset",
                    model.display_name()
                )));
            }
        }
        let mut json = serde_json::to_string_pretty(self)
            .map_err(|e| DecodeError::corrupt(format!("config cannot be serialised: {e}")))?;
        json.push('\n');
        Ok(json)
    }

    pub fn from_json(text: &str) -> Result<(Config, LoadReport), DecodeError> {
        let mut config: Config = serde_json::from_str(text)
            .map_err(|e| DecodeError::corrupt_at(format!("config is not readable: {e}"), e.line() as u64))?;
        if config.version > CONFIG_VERSION {
            return Err(DecodeError::UnsupportedFormat(format!(
                "config version {} is newer than this build supports ({CONFIG_VERSION})",
                config.version
            )));
        }
        let mut report = LoadReport::new();
        config.sanitize(&mut report);
        Ok((config, report))
    }

    /// Loads from `path`, falling back to defaults when it does not exist.
    ///
    /// A missing config is the normal first-run case, not an error. A *corrupt*
    /// one is reported and the defaults are used, because refusing to start
    /// would leave the user with no way to fix it from inside the app.
    pub fn load_from(path: &Path) -> (Config, LoadReport) {
        let mut report = LoadReport::new();
        let Ok(text) = std::fs::read_to_string(path) else {
            return (Config::default(), report);
        };
        match Config::from_json(&text) {
            Ok((config, load_report)) => {
                report.absorb(load_report);
                (config, report)
            }
            Err(e) => {
                report.note(format!("{} could not be read ({e}); using defaults", path.display()));
                (Config::default(), report)
            }
        }
    }

    pub fn save_to(&self, path: &Path) -> Result<(), DecodeError> {
        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent).map_err(|e| DecodeError::io(parent.display().to_string(), e))?;
        }
        std::fs::write(path, self.to_json()?).map_err(|e| DecodeError::io(path.display().to_string(), e))
    }

    /// Loads from the platform config path.
    pub fn load() -> (Config, LoadReport) {
        match config_path() {
            Some(path) => Config::load_from(&path),
            None => {
                let mut report = LoadReport::new();
                report.note("no writable config directory was found; settings will not persist");
                (Config::default(), report)
            }
        }
    }

    /// Saves to the platform config path.
    pub fn save(&self) -> Result<(), DecodeError> {
        let path = config_path().ok_or_else(|| DecodeError::corrupt("no writable config directory was found"))?;
        self.save_to(&path)
    }
}

/// The directory settings live in.
///
/// Resolved from environment variables rather than a crate: the rules are three
/// lines per platform, and a dependency here would be more surface than the
/// problem deserves.
pub fn config_dir() -> Option<PathBuf> {
    let base = if cfg!(windows) {
        std::env::var_os("APPDATA").map(PathBuf::from)
    } else if cfg!(target_os = "macos") {
        std::env::var_os("HOME").map(|h| PathBuf::from(h).join("Library").join("Application Support"))
    } else {
        std::env::var_os("XDG_CONFIG_HOME")
            .map(PathBuf::from)
            .or_else(|| std::env::var_os("HOME").map(|h| PathBuf::from(h).join(".config")))
    }?;
    Some(base.join("animated2d"))
}

pub fn config_path() -> Option<PathBuf> {
    Some(config_dir()?.join("config.json"))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn sample() -> Config {
        Config {
            version: CONFIG_VERSION,
            window: WindowConfig {
                position: Some((100, 200)),
                size: (480, 640),
                always_on_top: true,
                click_through: false,
            },
            models: vec![
                ModelConfig {
                    package: PathBuf::from("hero.a2dpack"),
                    animation: Some("idle".into()),
                    scale: 1.5,
                    offset: (10.0, -20.0),
                    flip_x: true,
                },
                ModelConfig::new("villain.a2dpack"),
            ],
            active: 1,
        }
    }

    #[test]
    fn a_config_round_trips_through_json() {
        let config = sample();
        let (parsed, report) = Config::from_json(&config.to_json().unwrap()).unwrap();
        assert_eq!(parsed, config);
        assert!(report.is_empty(), "{report}");
    }

    #[test]
    fn serialisation_is_byte_stable() {
        assert_eq!(sample().to_json().unwrap(), sample().to_json().unwrap());
    }

    #[test]
    fn json_uses_camel_case_throughout() {
        let json = sample().to_json().unwrap();
        for key in ["\"alwaysOnTop\"", "\"clickThrough\"", "\"flipX\""] {
            assert!(json.contains(key), "missing {key} in:\n{json}");
        }
        assert!(!json.contains("always_on_top"), "{json}");
    }

    #[test]
    fn a_missing_file_is_the_first_run_case_not_an_error() {
        let (config, report) = Config::load_from(Path::new("no/such/config.json"));
        assert_eq!(config, Config::default());
        assert!(report.is_empty(), "a missing config is normal: {report}");
    }

    #[test]
    fn a_corrupt_file_falls_back_to_defaults_and_says_so() {
        let dir = std::env::temp_dir().join(format!("a2d-cfg-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join("corrupt.json");
        std::fs::write(&path, "{ this is not json").unwrap();

        let (config, report) = Config::load_from(&path);
        assert_eq!(config, Config::default());
        assert!(report.to_string().contains("using defaults"), "{report}");
        let _ = std::fs::remove_file(&path);
    }

    #[test]
    fn a_newer_version_is_refused_rather_than_misread() {
        let mut config = sample();
        config.version = CONFIG_VERSION + 1;
        let err = Config::from_json(&config.to_json().unwrap()).unwrap_err();
        assert!(err.to_string().contains("newer"), "{err}");
    }

    #[test]
    fn a_save_and_load_round_trip_preserves_settings() {
        let dir = std::env::temp_dir().join(format!("a2d-cfg-rt-{}", std::process::id()));
        let path = dir.join("config.json");
        let _ = std::fs::remove_dir_all(&dir);

        sample().save_to(&path).expect("save should create the directory");
        let (loaded, report) = Config::load_from(&path);
        assert_eq!(loaded, sample());
        assert!(report.is_empty(), "{report}");
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn an_absurd_window_size_is_clamped_and_reported() {
        let mut config =
            Config { window: WindowConfig { size: (1, 999_999), ..Default::default() }, ..Default::default() };
        let mut report = LoadReport::new();
        config.sanitize(&mut report);
        assert_eq!(config.window.size, (MIN_WINDOW, MAX_WINDOW));
        assert!(report.to_string().contains("clamped"), "{report}");
    }

    #[test]
    fn an_out_of_range_scale_is_clamped() {
        let mut config =
            Config { models: vec![ModelConfig { scale: 100.0, ..ModelConfig::new("a") }], ..Default::default() };
        let mut report = LoadReport::new();
        config.sanitize(&mut report);
        assert_eq!(config.models[0].scale, MAX_SCALE);
        assert!(report.to_string().contains("clamped"), "{report}");
    }

    #[test]
    fn a_non_finite_scale_is_reset_rather_than_clamped_to_a_bound() {
        for bad in [f32::NAN, f32::INFINITY, f32::NEG_INFINITY] {
            let mut config =
                Config { models: vec![ModelConfig { scale: bad, ..ModelConfig::new("a") }], ..Default::default() };
            let mut report = LoadReport::new();
            config.sanitize(&mut report);
            assert!(config.models[0].scale.is_finite(), "for {bad}");
            assert!((MIN_SCALE..=MAX_SCALE).contains(&config.models[0].scale));
        }
    }

    #[test]
    fn a_non_finite_offset_is_reset() {
        let mut config = Config {
            models: vec![ModelConfig { offset: (f32::NAN, 5.0), ..ModelConfig::new("a") }],
            ..Default::default()
        };
        let mut report = LoadReport::new();
        config.sanitize(&mut report);
        assert_eq!(config.models[0].offset, (0.0, 0.0));
        assert!(report.to_string().contains("offset"), "{report}");
    }

    #[test]
    fn an_out_of_range_active_index_falls_back_to_the_first_model() {
        let mut config = Config { models: vec![ModelConfig::new("a")], active: 7, ..Default::default() };
        let mut report = LoadReport::new();
        config.sanitize(&mut report);
        assert_eq!(config.active, 0);
        assert!(report.to_string().contains("out of range"), "{report}");
    }

    #[test]
    fn an_active_index_with_no_models_is_left_alone() {
        // Nothing to clamp to; the viewer shows an empty state instead.
        let mut config = Config { models: Vec::new(), active: 3, ..Default::default() };
        let mut report = LoadReport::new();
        config.sanitize(&mut report);
        assert!(config.active_model().is_none());
        assert!(report.is_empty(), "{report}");
    }

    #[test]
    fn a_non_finite_value_is_refused_on_save_rather_than_written_as_null() {
        let config =
            Config { models: vec![ModelConfig { scale: f32::NAN, ..ModelConfig::new("a") }], ..Default::default() };
        let err = config.to_json().unwrap_err();
        assert!(err.to_string().contains("non-finite"), "{err}");
    }

    #[test]
    fn adding_a_package_selects_it() {
        let mut config = Config::default();
        assert_eq!(config.add_or_select(Path::new("a.a2dpack")), 0);
        assert_eq!(config.add_or_select(Path::new("b.a2dpack")), 1);
        assert_eq!(config.active, 1);
        assert_eq!(config.active_model().unwrap().package, PathBuf::from("b.a2dpack"));
    }

    #[test]
    fn adding_a_known_package_selects_it_without_duplicating() {
        let mut config = Config::default();
        config.add_or_select(Path::new("a.a2dpack"));
        config.add_or_select(Path::new("b.a2dpack"));
        assert_eq!(config.add_or_select(Path::new("a.a2dpack")), 0);
        assert_eq!(config.models.len(), 2, "re-adding must not duplicate");
        assert_eq!(config.active, 0);
    }

    #[test]
    fn re_adding_a_package_keeps_its_remembered_settings() {
        let mut config = Config::default();
        config.add_or_select(Path::new("a.a2dpack"));
        config.active_model_mut().unwrap().scale = 2.0;
        config.add_or_select(Path::new("b.a2dpack"));
        config.add_or_select(Path::new("a.a2dpack"));
        assert_eq!(config.active_model().unwrap().scale, 2.0);
    }

    #[test]
    fn a_display_name_comes_from_the_package_stem() {
        assert_eq!(ModelConfig::new("some/dir/hero.a2dpack").display_name(), "hero");
        assert_eq!(ModelConfig::new("hero").display_name(), "hero");
    }

    #[test]
    fn a_minimal_config_parses_with_defaults_filled_in() {
        let json = r#"{"version":1,"window":{"size":[300,300],"alwaysOnTop":false,"clickThrough":true}}"#;
        let (config, report) = Config::from_json(json).unwrap();
        assert!(config.models.is_empty());
        assert_eq!(config.active, 0);
        assert!(config.window.click_through);
        assert_eq!(config.window.position, None);
        assert!(report.is_empty(), "{report}");
    }

    #[test]
    fn the_config_path_sits_under_an_animated2d_directory() {
        // The exact base varies by platform and environment; what must hold is
        // that settings are namespaced rather than dropped in a shared folder.
        if let Some(path) = config_path() {
            assert!(path.ends_with("animated2d/config.json") || path.ends_with("animated2d\\config.json"), "{path:?}");
        }
    }
}
