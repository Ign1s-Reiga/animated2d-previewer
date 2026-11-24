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
    /// When true, the character looks toward the mouse pointer. Off by default,
    /// and absent from configs written before the option existed.
    #[serde(default)]
    pub follow_cursor: bool,
}

impl Default for WindowConfig {
    fn default() -> Self {
        WindowConfig {
            position: None,
            size: (480, 640),
            always_on_top: true,
            click_through: false,
            follow_cursor: false,
        }
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

    /// Drops remembered models that `is_dead` says can never be opened again.
    ///
    /// A dead entry would otherwise be reported as unloadable on every start,
    /// forever. The predicate must be conservative (see [`is_dead_entry`]):
    /// forgetting loses the model's scale, offset and animation. An entry that
    /// is merely unreachable or fails to load stays, with its warning. The
    /// active selection follows its model, or falls back to the first. Reports
    /// once, naming every path. Returns whether anything was dropped, i.e.
    /// whether the config needs saving.
    pub fn forget_dead_models(&mut self, is_dead: impl Fn(&Path) -> bool, report: &mut LoadReport) -> bool {
        let active = self.active_model().map(|m| m.package.clone());
        let mut forgotten = Vec::new();
        self.models.retain(|m| {
            let dead = is_dead(&m.package);
            if dead {
                forgotten.push(m.package.display().to_string());
            }
            !dead
        });
        if forgotten.is_empty() {
            return false;
        }
        self.active = active.and_then(|a| self.models.iter().position(|m| m.package == a)).unwrap_or(0);
        report.note(format!(
            "forgot {} remembered model(s) that no longer exist or are not packages: {}",
            forgotten.len(),
            forgotten.join(", ")
        ));
        true
    }

    /// Rewrites relative entries to absolute ones, once, at load.
    ///
    /// Loaded models carry absolute paths, so a relative entry would never
    /// match its own model: the selector would list it twice and its settings
    /// would be dropped. An entry is migrated when `is_package` says the path
    /// it resolves to from the current directory is a package; any other
    /// relative entry is left as it is, since it may belong to another start
    /// directory. Entries that become equal are merged, keeping the active
    /// one's settings, else the first's. The active pointer follows its
    /// model. Returns whether anything changed, i.e. whether to save.
    pub fn migrate_relative_entries(&mut self, is_package: impl Fn(&Path) -> bool) -> bool {
        let active_before = self.active;
        let mut changed = false;
        let mut merged: Vec<ModelConfig> = Vec::with_capacity(self.models.len());
        let mut new_active = None;
        for (index, model) in std::mem::take(&mut self.models).into_iter().enumerate() {
            let mut model = model;
            if !model.package.is_absolute() {
                let absolute = absolute_path(&model.package);
                if absolute != model.package && is_package(&absolute) {
                    model.package = absolute;
                    changed = true;
                }
            }
            let is_active = index == active_before;
            match merged.iter().position(|m| m.package == model.package) {
                Some(at) => {
                    changed = true;
                    if is_active {
                        merged[at] = model;
                        new_active = Some(at);
                    }
                }
                None => {
                    if is_active {
                        new_active = Some(merged.len());
                    }
                    merged.push(model);
                }
            }
        }
        self.models = merged;
        // An out-of-range pointer is `sanitize`'s to repair, not ours.
        if let Some(at) = new_active {
            self.active = at;
        }
        changed
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
        // Temp file then rename, so a crash mid-write leaves the old config
        // intact rather than a truncated one.
        let json = self.to_json()?;
        let mut temp = path.as_os_str().to_owned();
        // The pid keeps two processes saving at once from sharing a temp file.
        temp.push(format!(".{}.tmp", std::process::id()));
        let temp = PathBuf::from(temp);
        std::fs::write(&temp, json).map_err(|e| DecodeError::io(temp.display().to_string(), e))?;
        std::fs::rename(&temp, path).map_err(|e| {
            let _ = std::fs::remove_file(&temp);
            DecodeError::io(path.display().to_string(), e)
        })
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

/// Makes a path absolute without touching the filesystem, so a remembered
/// entry means the same place whatever directory the viewer starts from. Falls
/// back to the path as given if the platform cannot say.
pub fn absolute_path(path: &Path) -> PathBuf {
    std::path::absolute(path).unwrap_or_else(|_| path.to_path_buf())
}

/// Whether `dir` holds a package manifest: the test for migrating a relative
/// entry to the absolute path it resolves to.
pub fn has_manifest(dir: &Path) -> bool {
    dir.join("manifest.json").is_file()
}

/// Whether a remembered entry is *definitely* never coming back as a package.
///
/// Deliberately conservative, because a wrong "dead" deletes the user's
/// per-model settings. True only for an absolute path that is
///
/// - a regular file (a raw bundle remembered by an older build),
/// - missing while its parent directory exists and is not empty (deleted; an
///   empty parent may be the mount point of a volume that is not mounted),
/// - a directory with no `manifest.json`, or
/// - a package whose readable manifest names a model type the viewer cannot
///   open.
///
/// Relative paths (they depend on the start directory), unreachable roots
/// (an unmounted drive, a network share), permission errors, and packages
/// whose manifest merely fails to parse are all left alone: the load warning
/// is the right outcome for those.
pub fn is_dead_entry(path: &Path) -> bool {
    use std::io::ErrorKind::NotFound;

    if !path.is_absolute() {
        return false;
    }
    match std::fs::metadata(path) {
        Ok(meta) if meta.is_file() => true,
        Ok(meta) if meta.is_dir() => {
            let manifest = path.join("manifest.json");
            match std::fs::read_to_string(&manifest) {
                Ok(text) => a2d_pack::Manifest::from_json(&text)
                    .is_ok_and(|m| !crate::viewer::LoadedModel::can_open(m.model_type)),
                Err(e) => e.kind() == NotFound,
            }
        }
        Ok(_) => false,
        // Deleted, as opposed to on a volume that is not mounted: the parent
        // must exist *and hold something*. An unmounted volume can leave its
        // mount point (or a reused drive letter) behind as an empty directory,
        // and that case is kept. The remaining limitation is a mount point that
        // holds other files; it is accepted.
        Err(e) if e.kind() == NotFound => path.parent().is_some_and(|parent| {
            std::fs::metadata(parent).is_ok_and(|m| m.is_dir())
                && std::fs::read_dir(parent).is_ok_and(|mut entries| entries.next().is_some())
        }),
        Err(_) => false,
    }
}

/// Environment variable that overrides where settings are kept.
///
/// Exists for portable installs — running from a USB stick and keeping settings
/// beside the binary — and it is what lets a test drive the real viewer without
/// writing into the user's actual configuration.
pub const CONFIG_DIR_ENV: &str = "A2D_CONFIG_DIR";

/// The directory settings live in.
pub fn config_dir() -> Option<PathBuf> {
    config_dir_with(std::env::var_os(CONFIG_DIR_ENV))
}

/// The resolution rule, with the override passed in rather than read.
///
/// Taking the override as an argument keeps this testable: mutating the
/// process environment from a test is both `unsafe` and racy against every
/// other test running in parallel.
///
/// The platform bases are resolved from environment variables rather than a
/// crate — the rules are three lines each, and a dependency would be more
/// surface than the problem deserves.
fn config_dir_with(override_dir: Option<std::ffi::OsString>) -> Option<PathBuf> {
    if let Some(dir) = override_dir {
        // An empty value is a mistake rather than a request to use the working
        // directory, so it falls through to the platform default.
        if !dir.is_empty() {
            return Some(PathBuf::from(dir));
        }
    }
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
                follow_cursor: true,
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
    fn missing_models_are_forgotten_once_and_reported_in_one_note() {
        let mut config = sample();
        config.models.push(ModelConfig::new("gone.a2dpack"));
        config.models.insert(0, ModelConfig::new("also-gone.a2dpack"));
        config.active = 2; // villain
        let mut report = LoadReport::new();
        let changed = config.forget_dead_models(|p| p.to_string_lossy().contains("gone"), &mut report);
        assert!(changed);
        assert_eq!(config.models.len(), 2);
        assert_eq!(config.active_model().unwrap().package, PathBuf::from("villain.a2dpack"));
        assert_eq!(report.len(), 1, "{report}");
        let text = report.to_string();
        assert!(text.contains("forgot 2 remembered model(s) that no longer exist or are not packages"), "{text}");
        assert!(text.contains("also-gone.a2dpack") && text.contains("gone.a2dpack"), "{text}");
    }

    fn scratch(label: &str) -> PathBuf {
        let dir = std::env::temp_dir().join(format!("a2d-cfg-{label}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        dir
    }

    fn manifest_of(model_type: a2d_pack::ModelType) -> String {
        a2d_pack::Manifest::new(model_type, "x").to_json().unwrap()
    }

    #[test]
    fn only_definitely_dead_entries_are_forgotten() {
        let dir = absolute_path(&scratch("dead"));
        let file = dir.join("bundle.bin");
        let bare = dir.join("bare.a2dpack");
        let spine = dir.join("spine.a2dpack");
        let cubism = dir.join("cubism.a2dpack");
        let corrupt = dir.join("corrupt.a2dpack");
        for d in [&bare, &spine, &cubism, &corrupt] {
            std::fs::create_dir_all(d).unwrap();
        }
        std::fs::write(&file, b"not a package").unwrap();
        std::fs::write(spine.join("manifest.json"), manifest_of(a2d_pack::ModelType::Spine)).unwrap();
        std::fs::write(cubism.join("manifest.json"), manifest_of(a2d_pack::ModelType::Cubism)).unwrap();
        std::fs::write(corrupt.join("manifest.json"), "{ not valid, but present }").unwrap();

        assert!(is_dead_entry(&file), "a regular file is not a package");
        assert!(is_dead_entry(&bare), "a directory without a manifest is not a package");
        assert!(is_dead_entry(&dir.join("deleted.a2dpack")), "missing under an existing parent is deleted");
        assert!(is_dead_entry(&cubism), "a model type the viewer cannot open will never load");
        assert!(!is_dead_entry(&spine));
        assert!(!is_dead_entry(&corrupt), "a corrupt manifest is a real problem to warn about");

        let mut config = Config::default();
        for p in [&file, &bare, &spine, &cubism, &corrupt] {
            config.add_or_select(p);
        }
        let mut report = LoadReport::new();
        assert!(config.forget_dead_models(is_dead_entry, &mut report));
        let kept: Vec<_> = config.models.iter().map(|m| m.package.clone()).collect();
        assert_eq!(kept, vec![spine, corrupt]);
        assert_eq!(report.len(), 1, "{report}");
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn a_missing_package_under_an_empty_parent_is_kept_as_possibly_unmounted() {
        let dir = absolute_path(&scratch("mountpoint"));
        assert!(!is_dead_entry(&dir.join("pkg.a2dpack")), "an empty parent may be an unmounted volume");
        std::fs::write(dir.join("other"), b"x").unwrap();
        assert!(is_dead_entry(&dir.join("pkg.a2dpack")), "a parent holding other things means it was deleted");
        let _ = std::fs::remove_dir_all(&dir);
    }

    fn settled(path: &str, scale: f32) -> ModelConfig {
        ModelConfig { scale, ..ModelConfig::new(path) }
    }

    #[test]
    fn a_relative_entry_that_is_a_package_here_becomes_absolute() {
        let here = absolute_path(Path::new("hero.a2dpack"));
        let mut config = Config {
            models: vec![settled("hero.a2dpack", 2.0), settled("elsewhere.a2dpack", 3.0)],
            active: 0,
            ..Config::default()
        };
        // Only the first resolves to a package from this directory.
        let changed = config.migrate_relative_entries(|p| p == here);
        assert!(changed);
        assert_eq!(config.models[0].package, here);
        assert_eq!(config.models[0].scale, 2.0, "settings travel with the entry");
        assert_eq!(config.models[1].package, PathBuf::from("elsewhere.a2dpack"), "others are left alone");
        assert_eq!(config.active, 0);
        assert!(!config.migrate_relative_entries(|p| p == here), "a second pass changes nothing");
    }

    #[test]
    fn entries_equal_after_migration_are_merged_keeping_the_active_ones_settings() {
        let here = absolute_path(Path::new("hero.a2dpack"));
        let mut config = Config {
            models: vec![settled("villain.a2dpack", 1.0), settled("hero.a2dpack", 2.0), ModelConfig::new(here.clone())],
            active: 1,
            ..Config::default()
        };
        let mut abs = settled("x", 9.0);
        abs.package = here.clone();
        config.models[2] = abs;
        assert!(config.migrate_relative_entries(|p| p == here));
        assert_eq!(config.models.len(), 2);
        assert_eq!(config.models[1].package, here);
        assert_eq!(config.models[1].scale, 2.0, "the active entry's settings win");
        assert_eq!(config.active, 1);

        // Active is the later duplicate: pointer follows the merged entry.
        let mut config = Config {
            models: vec![ModelConfig::new(here.clone()), settled("hero.a2dpack", 4.0)],
            active: 1,
            ..Config::default()
        };
        assert!(config.migrate_relative_entries(|p| p == here));
        assert_eq!(config.models.len(), 1);
        assert_eq!((config.active, config.models[0].scale), (0, 4.0));

        // Neither active: the first wins.
        let mut config = Config {
            models: vec![
                settled("villain.a2dpack", 1.0),
                ModelConfig { scale: 5.0, ..ModelConfig::new(here.clone()) },
                settled("hero.a2dpack", 4.0),
            ],
            active: 0,
            ..Config::default()
        };
        assert!(config.migrate_relative_entries(|p| p == here));
        assert_eq!(config.models.len(), 2);
        assert_eq!(config.models[1].scale, 5.0);
    }

    #[test]
    fn relative_and_unreachable_entries_are_kept() {
        // Relative: depends on where the viewer was started.
        assert!(!is_dead_entry(Path::new("hero.a2dpack")));
        // Unreachable root: the parent does not exist either (unmounted drive).
        let dir = absolute_path(&scratch("unreach"));
        let off = dir.join("no-such-drive").join("pkg.a2dpack");
        assert!(!is_dead_entry(&off));
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn absolute_path_is_absolute_and_stable() {
        let abs = absolute_path(Path::new("hero.a2dpack"));
        assert!(abs.is_absolute());
        assert_eq!(absolute_path(&abs), abs);
    }

    #[test]
    fn forgetting_the_active_model_falls_back_to_the_first() {
        let mut config = sample();
        config.active = 1;
        let mut report = LoadReport::new();
        config.forget_dead_models(|p| p == Path::new("villain.a2dpack"), &mut report);
        assert_eq!(config.active, 0);
        assert_eq!(config.models.len(), 1);
    }

    #[test]
    fn nothing_missing_changes_nothing_and_says_nothing() {
        let mut config = sample();
        let before = config.clone();
        let mut report = LoadReport::new();
        assert!(!config.forget_dead_models(|_| false, &mut report));
        assert_eq!(config, before);
        assert!(report.is_empty());
    }

    #[test]
    fn saving_leaves_no_temp_file_and_replaces_the_old_config() {
        let dir = std::env::temp_dir().join(format!("a2d-cfg-atomic-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join("config.json");
        std::fs::write(&path, "stale").unwrap();
        sample().save_to(&path).unwrap();
        assert_eq!(Config::load_from(&path).0, sample());
        let leftovers = std::fs::read_dir(&dir)
            .unwrap()
            .filter_map(Result::ok)
            .filter(|e| e.file_name().to_string_lossy().ends_with(".tmp"))
            .count();
        assert_eq!(leftovers, 0, "no temp file should remain");
        let _ = std::fs::remove_dir_all(&dir);
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
        assert!(!config.window.follow_cursor, "a config from before the option existed loads as off");
        assert_eq!(config.window.position, None);
        assert!(report.is_empty(), "{report}");
    }

    #[test]
    fn follow_cursor_defaults_off_and_survives_a_json_round_trip() {
        assert!(!Config::default().window.follow_cursor);
        let json = serde_json::to_string(&sample()).unwrap();
        let (back, report) = Config::from_json(&json).unwrap();
        assert!(back.window.follow_cursor);
        assert!(report.is_empty(), "{report}");
    }

    #[test]
    fn an_override_directory_is_used_verbatim() {
        let dir = PathBuf::from("/somewhere/portable");
        assert_eq!(config_dir_with(Some(dir.clone().into_os_string())), Some(dir));
    }

    #[test]
    fn an_empty_override_falls_through_to_the_platform_default() {
        // Otherwise an unset-but-present variable would drop settings into the
        // working directory, wherever that happened to be.
        let empty = config_dir_with(Some(std::ffi::OsString::new()));
        assert_ne!(empty, Some(PathBuf::new()));
        assert_eq!(empty, config_dir_with(None));
    }

    #[test]
    fn the_config_path_sits_under_an_animated2d_directory() {
        // The exact base varies by platform and environment; what must hold is
        // that settings are namespaced rather than dropped in a shared folder.
        if let Some(path) = config_dir_with(None).map(|d| d.join("config.json")) {
            assert!(path.ends_with("animated2d/config.json") || path.ends_with("animated2d\\config.json"), "{path:?}");
        }
    }
}
