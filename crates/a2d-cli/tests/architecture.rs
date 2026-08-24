//! Executable form of the layering rules in CLAUDE.md §3.
//!
//! Those rules say dependency direction is "enforced by review". Review is
//! fallible and a violation is cheap to introduce, so it is enforced here
//! instead: this test reads the workspace manifests and the sources, and fails
//! on the two invariants the whole design rests on.
//!
//! 1. Game-specific knowledge must not leak downstream of `importers/`.
//! 2. Source-version-specific knowledge must not leak downstream of `formats/`.

use std::collections::BTreeSet;
use std::path::{Path, PathBuf};

fn workspace_root() -> PathBuf {
    // CARGO_MANIFEST_DIR is `<root>/crates/a2d-cli`.
    Path::new(env!("CARGO_MANIFEST_DIR")).parent().and_then(Path::parent).expect("workspace root").to_path_buf()
}

fn crate_dir(name: &str) -> PathBuf {
    workspace_root().join("crates").join(name)
}

/// Workspace crates listed in a crate's `[dependencies]`.
fn dependencies_of(name: &str) -> BTreeSet<String> {
    let manifest = crate_dir(name).join("Cargo.toml");
    let text =
        std::fs::read_to_string(&manifest).unwrap_or_else(|e| panic!("{} should be readable: {e}", manifest.display()));

    let mut found = BTreeSet::new();
    let mut in_deps = false;
    for line in text.lines() {
        let line = line.trim();
        if line.starts_with('[') {
            // Dev-dependencies may point anywhere; only real deps are ranked.
            in_deps = line == "[dependencies]";
            continue;
        }
        if !in_deps || line.is_empty() || line.starts_with('#') {
            continue;
        }
        let Some((key, _)) = line.split_once(['.', ' ', '=']) else { continue };
        let key = key.trim();
        if key.starts_with("a2d-") {
            found.insert(key.to_string());
        }
    }
    found
}

/// The part of a source file that is not its `#[cfg(test)]` module.
///
/// Test fixtures legitimately mention game names and unwrap freely, so every
/// source-level rule below is checked against production code only. By
/// convention the test module is last in each file, so cutting at the first
/// `#[cfg(test)]` is exact — and far more reliable than counting braces across
/// string literals.
fn production_code(text: &str) -> &str {
    match text.find("#[cfg(test)]") {
        Some(at) => &text[..at],
        None => text,
    }
}

/// Every `.rs` file in a crate's `src/`.
fn sources_of(name: &str) -> Vec<(PathBuf, String)> {
    fn walk(dir: &Path, out: &mut Vec<(PathBuf, String)>) {
        let Ok(entries) = std::fs::read_dir(dir) else { return };
        for entry in entries.flatten() {
            let path = entry.path();
            if path.is_dir() {
                walk(&path, out);
            } else if path.extension().is_some_and(|e| e == "rs") {
                if let Ok(text) = std::fs::read_to_string(&path) {
                    out.push((path, text));
                }
            }
        }
    }
    let mut out = Vec::new();
    walk(&crate_dir(name).join("src"), &mut out);
    out.sort_by(|a, b| a.0.cmp(&b.0));
    out
}

#[test]
fn core_depends_on_no_other_workspace_crate() {
    // `a2d-core` is the bottom of the graph. Anything it needed from above
    // would invert the whole dependency direction.
    assert!(dependencies_of("a2d-core").is_empty(), "a2d-core must not depend on any other a2d crate");
}

#[test]
fn dependency_direction_is_one_way() {
    // Each entry: the crate, and what it must never depend on.
    let forbidden: [(&str, &[&str]); 6] = [
        // The renderer is source-format-neutral: it may not learn about any
        // decoder, importer, or Unity.
        ("a2d-render", &["a2d-import", "a2d-spine", "a2d-cubism", "a2d-unity"]),
        // The runtime consumes IR only.
        ("a2d-runtime", &["a2d-import", "a2d-unity", "a2d-render", "a2d-spine", "a2d-cubism"]),
        // Decoders normalise into IR; they do not discover assets.
        ("a2d-spine", &["a2d-import", "a2d-unity", "a2d-render", "a2d-runtime"]),
        ("a2d-cubism", &["a2d-import", "a2d-unity", "a2d-render", "a2d-runtime"]),
        // The package format is below the importers that write it.
        ("a2d-pack", &["a2d-import", "a2d-unity", "a2d-render", "a2d-runtime"]),
        // The desktop host loads packages; it never touches raw assets.
        ("a2d-desktop", &["a2d-import", "a2d-unity", "a2d-spine", "a2d-cubism"]),
    ];

    for (crate_name, banned) in forbidden {
        let deps = dependencies_of(crate_name);
        for name in banned {
            assert!(
                !deps.contains(*name),
                "{crate_name} must not depend on {name} (CLAUDE.md §3 dependency direction)"
            );
        }
    }
}

#[test]
fn no_game_name_appears_downstream_of_the_importers() {
    // Invariant 1. `a2d-import` is where these names are allowed to live; the
    // CLI may print them because it is the user-facing surface.
    let games = ["nikke", "aeons_echo", "aeonsecho", "depose_girls", "deposegirls"];
    let downstream = ["a2d-core", "a2d-spine", "a2d-cubism", "a2d-runtime", "a2d-render", "a2d-pack", "a2d-desktop"];

    for crate_name in downstream {
        for (path, text) in sources_of(crate_name) {
            let lower = production_code(&text).to_lowercase();
            for game in games {
                assert!(
                    !lower.contains(game),
                    "{} mentions {game:?}: game-specific knowledge must not leak downstream of importers/ \
                     (CLAUDE.md §2). If a decoder needs it, the importer should have handled it.",
                    path.display()
                );
            }
        }
    }
}

#[test]
fn no_source_version_branch_appears_downstream_of_the_decoders() {
    // Invariant 2. The runtime and renderer must not be able to tell which
    // Spine version produced the IR, so they may not name one.
    let versions = ["spine 2.", "spine 3.", "spine 4.", "spine-3.", "spine-4.", "3.8.99", "4.1.23", "spineversion"];
    let downstream = ["a2d-runtime", "a2d-render", "a2d-desktop"];

    for crate_name in downstream {
        for (path, text) in sources_of(crate_name) {
            let lower = production_code(&text).to_lowercase();
            for needle in versions {
                assert!(
                    !lower.contains(needle),
                    "{} mentions {needle:?}: source-version knowledge must not leak downstream of formats/ \
                     (CLAUDE.md §2). The decoder is the layer that should have normalised it.",
                    path.display()
                );
            }
        }
    }
}

#[test]
fn the_renderer_neutral_types_live_in_core() {
    // `RenderMesh` is the contract between runtime and renderer. If it moved
    // into a decoder crate, both invariants would be trivially breakable.
    let core = sources_of("a2d-core");
    assert!(
        core.iter().any(|(_, text)| text.contains("pub struct RenderMesh")),
        "RenderMesh must be defined in a2d-core"
    );
    assert!(
        core.iter().any(|(_, text)| text.contains("pub trait AnimatedModel")),
        "AnimatedModel must be defined in a2d-core"
    );
}

#[test]
fn library_crates_avoid_unwrap_on_data_dependent_paths() {
    // Rule §4.13. Test code is exempt: a panicking assertion is the point there.
    let libraries = [
        "a2d-core",
        "a2d-spine",
        "a2d-cubism",
        "a2d-unity",
        "a2d-pack",
        "a2d-runtime",
        "a2d-render",
        "a2d-import",
        "a2d-desktop",
    ];
    let mut offenders = Vec::new();

    for crate_name in libraries {
        for (path, text) in sources_of(crate_name) {
            for (n, line) in production_code(&text).lines().enumerate() {
                let trimmed = line.trim();
                if trimmed.starts_with("//") {
                    continue;
                }
                for bad in [".unwrap()", ".expect(", "panic!("] {
                    if trimmed.contains(bad) {
                        offenders.push(format!("{}:{} {trimmed}", path.display(), n + 1));
                    }
                }
            }
        }
    }

    assert!(
        offenders.is_empty(),
        "library crates must not panic on data-dependent paths (CLAUDE.md §4.13):\n{}",
        offenders.join("\n")
    );
}

#[test]
fn no_public_api_exposes_a_loosely_typed_json_map() {
    // Rule §4.6: `HashMap<String, serde_json::Value>` must not appear in a
    // public runtime or renderer API. Checked across every library crate,
    // because it is a smell everywhere, not only downstream.
    for crate_name in ["a2d-core", "a2d-runtime", "a2d-render", "a2d-pack", "a2d-spine", "a2d-cubism"] {
        for (path, text) in sources_of(crate_name) {
            let collapsed = production_code(&text).replace(char::is_whitespace, "");
            assert!(
                !collapsed.contains("pubHashMap<String,serde_json::Value>")
                    && !collapsed.contains("HashMap<String,Value>"),
                "{} exposes a loosely typed JSON map (CLAUDE.md §4.6)",
                path.display()
            );
        }
    }
}
