//! Source-specific importers.
//!
//! This module is the only place in the workspace where the identity of a
//! source title may influence behaviour (spec §2). Everything here reduces to a
//! generic source package; nothing downstream learns where an asset came from
//! beyond the informational `sourceGame` string in the manifest.
//!
//! Importers are named for the asset shape they reconstruct rather than for the
//! title they were written against. Two titles that ship the same shape share
//! an importer, and the name stays meaningful to someone who has never seen
//! either of them.

use std::path::Path;

use a2d_core::{DecodeError, LoadReport};
use a2d_pack::Package;

use crate::detect::{classify, AssetKind};
use crate::generic;

/// The importers this build knows about.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Importer {
    /// No game-specific handling: plain Spine assets in a folder.
    Generic,
    /// Spine skeletons shipped as `.skel.bytes` / `.atlas.txt` pairs.
    SpineBytes,
    /// Cubism models packed inside Unity AssetBundles.
    UnityCubism,
    /// Spine rigs packed inside Unity AssetBundles; standing models only.
    UnitySpine,
}

impl Importer {
    pub fn as_str(self) -> &'static str {
        match self {
            Importer::Generic => "generic",
            Importer::SpineBytes => "spine_bytes",
            Importer::UnityCubism => "unity_cubism",
            Importer::UnitySpine => "unity_spine",
        }
    }

    pub fn parse(s: &str) -> Option<Importer> {
        Some(match s {
            "generic" => Importer::Generic,
            "spine_bytes" | "spine-bytes" => Importer::SpineBytes,
            "unity_cubism" | "unity-cubism" => Importer::UnityCubism,
            "unity_spine" | "unity-spine" => Importer::UnitySpine,
            _ => return None,
        })
    }

    pub fn all() -> [Importer; 4] {
        [Importer::Generic, Importer::SpineBytes, Importer::UnityCubism, Importer::UnitySpine]
    }
}

/// Guesses which importer suits a path, from the assets actually present.
///
/// Only used to pick a default; `inspect` and `import` both accept an explicit
/// choice, and an unclear guess falls back to [`Importer::Generic`] rather than
/// asserting a source.
pub fn guess_importer(path: &Path) -> Importer {
    // A file identifies itself. Reading the directory it happens to sit in
    // does not, and costs far more than it sounds: a sample folder holds
    // thousands of assets, and opening the head of 256 of them to identify one
    // file took over five minutes on a real one -- long enough to read as a
    // hang rather than as slowness.
    if path.is_file() {
        return guess_from_file(path);
    }
    let dir = path.to_path_buf();
    let Ok(entries) = std::fs::read_dir(&dir) else { return Importer::Generic };

    let mut has_unity = false;
    let mut has_doubled_suffix = false;
    for entry in entries.flatten().take(256) {
        let file = entry.path();
        let Some(name) = file.file_name().and_then(|n| n.to_str()) else { continue };
        // The tell here is the doubled suffix, not the file's content.
        if name.ends_with(".skel.bytes") || name.ends_with(".atlas.txt") {
            has_doubled_suffix = true;
        }
        if let Ok(head) = read_head(&file, 64) {
            if matches!(classify(&head), AssetKind::UnityBundle { .. }) {
                has_unity = true;
            }
        }
    }

    match (has_doubled_suffix, has_unity) {
        (true, _) => Importer::SpineBytes,
        // A Unity bundle could hold either shape, so the importer is not
        // guessed from that alone.
        (false, true) => Importer::Generic,
        (false, false) => Importer::Generic,
    }
}

/// Guesses the importer for a single file, from that file alone.
///
/// A Unity bundle is the only case needing more than its own header, because
/// the container says nothing about which ecosystem is inside it. Looking is
/// affordable here in a way it is not during a directory scan: one bundle,
/// once, on a path the caller named explicitly.
fn guess_from_file(path: &Path) -> Importer {
    let name = path.file_name().and_then(|n| n.to_str()).unwrap_or_default();
    // The tell here is the doubled suffix, not the file's content.
    if name.ends_with(".skel.bytes") || name.ends_with(".atlas.txt") {
        return Importer::SpineBytes;
    }
    let Ok(head) = read_head(path, 64) else { return Importer::Generic };
    if !matches!(classify(&head), AssetKind::UnityBundle { .. }) {
        return Importer::Generic;
    }
    let Ok(bytes) = std::fs::read(path) else { return Importer::Generic };

    // Spine is the decisive check -- a skeleton and an atlas, both recognised
    // by content -- so it is asked first and Cubism is what remains.
    let mut report = LoadReport::new();
    if crate::inspect_spine_bundle(&bytes, &mut report).map(|s| s.is_spine()).unwrap_or(false) {
        return Importer::UnitySpine;
    }
    let mut report = LoadReport::new();
    match crate::inspect_bundle(&bytes, &mut report) {
        Ok(inventory) if inventory.moc.is_some() => Importer::UnityCubism,
        // Neither shape: say so by falling back rather than by asserting one.
        _ => Importer::Generic,
    }
}

fn read_head(path: &Path, n: usize) -> Result<Vec<u8>, std::io::Error> {
    use std::io::Read;
    let mut file = std::fs::File::open(path)?;
    let mut buf = vec![0u8; n];
    let read = file.read(&mut buf)?;
    buf.truncate(read);
    Ok(buf)
}

/// Discovers every character an importer can find at `path`.
pub fn discover(importer: Importer, path: &Path) -> Result<Vec<generic::SpineSourceSet>, DecodeError> {
    match importer {
        // The `.skel.bytes` / `.atlas.txt` naming is already handled by the
        // generic stem normalisation, so discovery is shared. The importer stays
        // a distinct choice because it labels the package's `sourceGame` and is
        // where any future layout quirk belongs.
        Importer::Generic | Importer::SpineBytes => generic::discover(path),
        Importer::UnitySpine => Err(unimplemented_importer(
            "unity_spine",
            "discovering Spine rigs inside Unity bundles is not implemented; \
             extract the skeleton, atlas and textures first and import them with `--game generic`",
        )),
        Importer::UnityCubism => Err(unimplemented_importer(
            "unity_cubism",
            "Cubism reconstruction from Unity AssetBundles is not implemented yet",
        )),
    }
}

/// Imports one discovered character.
pub fn import(importer: Importer, set: &generic::SpineSourceSet) -> Result<(Package, LoadReport), DecodeError> {
    generic::import(set, importer.as_str())
}

fn unimplemented_importer(importer: &str, detail: &str) -> DecodeError {
    DecodeError::Reconstruction { game: importer.to_string(), message: detail.to_string() }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn importer_names_round_trip() {
        for importer in Importer::all() {
            assert_eq!(Importer::parse(importer.as_str()), Some(importer));
        }
    }

    /// A directory holding a decoy that would flip the guess if it were read.
    fn dir_with_a_decoy(tag: &str) -> std::path::PathBuf {
        let dir = std::env::temp_dir().join(format!("a2d-guess-{tag}"));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).expect("temp dir should be creatable");
        // Reading the directory would see this and answer `spine_bytes`.
        std::fs::write(dir.join("decoy.skel.bytes"), b"not read").expect("decoy");
        dir
    }

    #[test]
    fn a_named_file_is_identified_by_itself_and_not_by_its_neighbours() {
        // The cost of the old behaviour was the point: identifying one file by
        // opening up to 256 of its neighbours took over five minutes in a real
        // sample folder. This pins the *behaviour* that fixed it, which is
        // cheap to check -- a neighbour that would change the answer must not
        // change it.
        let dir = dir_with_a_decoy("neighbours");
        let target = dir.join("character.json");
        std::fs::write(&target, b"{}").expect("target");

        assert_eq!(guess_importer(&target), Importer::Generic, "the decoy beside it must not decide this file");
        // The same directory, asked as a directory, may still consult it.
        assert_eq!(guess_importer(&dir), Importer::SpineBytes);
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn a_doubled_suffix_still_names_its_own_importer() {
        let dir = dir_with_a_decoy("suffix");
        let target = dir.join("hero.skel.bytes");
        std::fs::write(&target, b"not a real skeleton").expect("target");
        assert_eq!(guess_importer(&target), Importer::SpineBytes);
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn a_file_that_is_not_a_bundle_falls_back_rather_than_asserting_a_source() {
        let dir = dir_with_a_decoy("plain");
        let target = dir.join("notes.txt");
        std::fs::write(&target, b"nothing to see").expect("target");
        assert_eq!(guess_importer(&target), Importer::Generic);
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn hyphenated_spellings_are_accepted() {
        assert_eq!(Importer::parse("spine-bytes"), Some(Importer::SpineBytes));
        assert_eq!(Importer::parse("unity-cubism"), Some(Importer::UnityCubism));
    }

    #[test]
    fn an_unknown_importer_name_is_rejected() {
        assert_eq!(Importer::parse("not_a_known_shape"), None);
        assert_eq!(Importer::parse(""), None);
    }

    #[test]
    fn the_unimplemented_importers_say_so_precisely() {
        let err = discover(Importer::UnitySpine, Path::new(".")).unwrap_err();
        assert!(matches!(err, DecodeError::Reconstruction { .. }), "{err}");
        assert!(err.to_string().contains("unity_spine"), "{err}");
        assert!(err.to_string().contains("--game generic"), "{err}");

        let err = discover(Importer::UnityCubism, Path::new(".")).unwrap_err();
        assert!(err.to_string().contains("not implemented"), "{err}");
    }

    #[test]
    fn guessing_on_a_missing_directory_falls_back_to_generic() {
        assert_eq!(guess_importer(Path::new("no/such/place")), Importer::Generic);
    }
}
