//! Smoke tests that drive the real `animated2d` binary as a subprocess.
//!
//! Two things cannot be reached from an in-process test: a `winit` event loop
//! refuses to be created off the main thread, and the settings-on-quit path
//! only runs when the viewer actually shuts down. Both are covered here by
//! launching the built binary with `--exit-after`, which takes exactly the same
//! shutdown path as quitting by hand.
//!
//! `A2D_CONFIG_DIR` points the viewer at a scratch directory, so these never
//! touch the real configuration.
//!
//! Each launch costs a few seconds of device creation, so the assertions are
//! deliberately gathered into as few runs as possible rather than spread one
//! per test.
//!
//! They skip when there is no GPU or no desktop session — a headless CI box has
//! neither — unless `A2D_REQUIRE_GPU` is set, which turns the skip into a
//! failure.

use std::path::{Path, PathBuf};
use std::process::{Command, Output};
use std::sync::atomic::{AtomicU32, Ordering};
use std::sync::Mutex;

use a2d_desktop::{Config, CONFIG_DIR_ENV};

#[path = "support/mod.rs"]
mod support;

use support::{Fixture, TempDir};

static COUNTER: AtomicU32 = AtomicU32::new(0);

/// Only one viewer runs at a time.
///
/// The harness would otherwise start these at once, and several windows
/// competing for the GPU and the focus makes every one of them slower.
static ONE_AT_A_TIME: Mutex<()> = Mutex::new(());

/// How long each launch spends drawing.
///
/// The deadline starts once the window is up, so this is real drawing time, not
/// a budget that start-up has to fit inside.
const DRAW_FOR: &str = "1";

fn temp_dir(label: &str) -> TempDir {
    let n = COUNTER.fetch_add(1, Ordering::Relaxed);
    TempDir::new(std::env::temp_dir().join(format!("a2d-proc-{}-{}-{}", label, std::process::id(), n)))
}

/// Reasons a machine legitimately cannot run the viewer.
///
/// Distinguished from a real failure so a headless box skips rather than
/// reporting a bug that is not there.
fn is_environmental(text: &str) -> bool {
    [
        "no suitable GPU adapter",
        "could not create an event loop",
        "could not create a window",
        "could not create a window surface",
        "the surface offers no usable texture format",
    ]
    .iter()
    .any(|needle| text.contains(needle))
}

/// Builds a fixture package and returns its directory, the package and the
/// scratch config directory.
fn prepare(label: &str) -> (TempDir, PathBuf, PathBuf) {
    let dir = temp_dir(label);
    Fixture::spine_json().write_to(dir.path());
    let package = dir.path().join("hero.a2dpack");

    let mut sink = Vec::new();
    a2d_cli::import(&mut sink, dir.path(), &package, None).expect("fixture should import");

    let config_dir = dir.path().join("config");
    (dir, package, config_dir)
}

/// Runs the viewer for [`DRAW_FOR`] with settings isolated to `config_dir`.
fn run_viewer(package: &Path, config_dir: &Path) -> Output {
    // A poisoned lock means another test panicked, not that the guard is
    // unusable; taking it anyway keeps the real failure visible instead of
    // burying it under lock errors.
    let _guard = ONE_AT_A_TIME.lock().unwrap_or_else(|e| e.into_inner());
    Command::new(env!("CARGO_BIN_EXE_animated2d"))
        .args(["preview", &package.display().to_string(), "--exit-after", DRAW_FOR])
        .env(CONFIG_DIR_ENV, config_dir)
        .output()
        .expect("the animated2d binary should be runnable")
}

/// Runs the viewer and returns its stdout, or `None` when the machine cannot
/// open a window at all.
fn run_or_skip(package: &Path, config_dir: &Path) -> Option<String> {
    let output = run_viewer(package, config_dir);
    let stdout = String::from_utf8_lossy(&output.stdout).into_owned();
    let stderr = String::from_utf8_lossy(&output.stderr).into_owned();

    if !output.status.success() {
        if is_environmental(&format!("{stdout}{stderr}")) && std::env::var("A2D_REQUIRE_GPU").is_err() {
            eprintln!("skipping: this machine cannot open a viewer window\n{stderr}");
            return None;
        }
        panic!("the viewer exited with {:?}\n--- stdout ---\n{stdout}\n--- stderr ---\n{stderr}", output.status);
    }
    Some(stdout)
}

/// Reads the `Presented N frames` line the viewer prints on the way out.
fn presented_frames(stdout: &str) -> u64 {
    let line = stdout
        .lines()
        .find(|l| l.starts_with("Presented "))
        .unwrap_or_else(|| panic!("expected a frame count in:\n{stdout}"));
    line.split_whitespace()
        .nth(1)
        .and_then(|n| n.parse().ok())
        .unwrap_or_else(|| panic!("could not read a frame count from {line:?}"))
}

#[test]
fn the_viewer_draws_a_window_and_saves_its_settings_on_the_way_out() {
    // Both gaps an in-process test cannot reach, in one launch: that a window
    // was created and frames actually reached the screen, and that quitting
    // wrote settings that load again cleanly.
    let (_dir, package, config_dir) = prepare("draw");
    let config_file = config_dir.join("config.json");
    assert!(!config_file.exists(), "the scratch config should start empty");

    let Some(stdout) = run_or_skip(&package, &config_dir) else { return };

    assert!(stdout.contains("Opening"), "{stdout}");
    let frames = presented_frames(&stdout);
    assert!(frames > 0, "the window opened but never drew:\n{stdout}");

    assert!(config_file.is_file(), "quitting should have written {}", config_file.display());
    let (config, report) = Config::load_from(&config_file);
    assert!(report.is_empty(), "the written config should reload cleanly: {report}");

    let model = config.active_model().unwrap_or_else(|| panic!("no model was recorded in {config:?}"));
    assert_eq!(model.package, package, "the opened package should be remembered");
    assert_eq!(model.animation.as_deref(), Some("idle"), "the playing animation should be remembered");
    assert!(config.window.size.0 > 0 && config.window.size.1 > 0, "a window size should be recorded");
    assert!(config.window.position.is_some(), "a window position should be recorded");
}

#[test]
fn a_second_run_restores_what_the_first_one_saved() {
    // Persistence is only worth anything if it survives a restart, which needs
    // two real processes sharing one config directory.
    let (_dir, package, config_dir) = prepare("restore");
    let config_file = config_dir.join("config.json");

    let Some(_) = run_or_skip(&package, &config_dir) else { return };
    let (first, _) = Config::load_from(&config_file);

    let Some(stdout) = run_or_skip(&package, &config_dir) else { return };
    let (second, _) = Config::load_from(&config_file);

    assert!(presented_frames(&stdout) > 0, "the restored run should draw too:\n{stdout}");
    assert_eq!(second.models.len(), 1, "a second run must not duplicate the package");
    assert_eq!(second.active_model().map(|m| &m.package), first.active_model().map(|m| &m.package));
    assert_eq!(second.active_model().map(|m| m.scale), first.active_model().map(|m| m.scale));
    assert_eq!(second.window.size, first.window.size);
}

#[test]
fn an_out_of_range_config_is_repaired_rather_than_refused() {
    // A stale or hand-edited file must not stop the viewer opening: it is
    // clamped, the repair is reported, and the run proceeds. `Config` unit
    // tests cover the clamping itself; what only a real run proves is that the
    // repaired values are ones a window can actually be built from.
    let (_dir, package, config_dir) = prepare("repair");
    std::fs::create_dir_all(&config_dir).expect("config dir should be creatable");
    let config_file = config_dir.join("config.json");
    std::fs::write(
        &config_file,
        r#"{"version":1,"window":{"size":[1,999999],"alwaysOnTop":true,"clickThrough":false},
            "models":[{"package":"hero.a2dpack","scale":9999.0,"offset":[0.0,0.0]}],"active":42}"#,
    )
    .expect("config should be writable");

    let Some(stdout) = run_or_skip(&package, &config_dir) else { return };
    assert!(stdout.contains("clamped"), "the repair should be reported:\n{stdout}");
    assert!(presented_frames(&stdout) > 0, "the repaired config should still draw:\n{stdout}");

    let (config, _) = Config::load_from(&config_file);
    assert!(config.window.size.0 >= 64 && config.window.size.1 <= 8192, "{:?}", config.window.size);
    assert!(config.active < config.models.len(), "the active index should be in range");
}

#[test]
fn a_package_that_does_not_exist_fails_with_a_clear_message() {
    let dir = temp_dir("missing");
    let output = run_viewer(&dir.path().join("nope.a2dpack"), &dir.path().join("config"));
    assert!(!output.status.success(), "opening a missing package should fail");

    let text = format!("{}{}", String::from_utf8_lossy(&output.stdout), String::from_utf8_lossy(&output.stderr));
    assert!(text.contains("no models were loaded"), "{text}");
}

#[test]
fn a_nonsense_exit_deadline_is_refused_before_anything_opens() {
    let dir = temp_dir("bad-flag");
    let output = Command::new(env!("CARGO_BIN_EXE_animated2d"))
        .args(["preview", "whatever.a2dpack", "--exit-after", "-5"])
        .env(CONFIG_DIR_ENV, dir.path())
        .output()
        .expect("the animated2d binary should be runnable");

    assert!(!output.status.success());
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(stderr.contains("positive number of seconds"), "{stderr}");
}
