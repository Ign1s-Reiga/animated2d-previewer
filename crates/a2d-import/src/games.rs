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
use a2d_unity::Bundle;

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
/// A Unity bundle is deliberately not looked into here: its container says
/// nothing about which ecosystem is inside, and the one place that decides is
/// [`identify_bundle`]. A bundle therefore comes back as [`Importer::Generic`],
/// which asserts nothing.
fn guess_from_file(path: &Path) -> Importer {
    let name = path.file_name().and_then(|n| n.to_str()).unwrap_or_default();
    // The tell here is the doubled suffix, not the file's content.
    if name.ends_with(".skel.bytes") || name.ends_with(".atlas.txt") {
        return Importer::SpineBytes;
    }
    Importer::Generic
}

/// Which importer a Unity bundle's contents call for, when it can be told.
///
/// Spine is recognised by a skeleton, identified by content (an atlas is not
/// required, since import reports a missing one and carries on); Cubism by a
/// `CubismMoc`. `Ok(None)` means both inspections succeeded and found neither.
/// Anything else that is not a clean answer is an error rather than a guess
/// (spec §10): the bundle's own parse error when it cannot be read, both names
/// when it satisfies both shapes, and a failed inspection when the other
/// importer cannot be ruled out.
pub fn detect_bundle(bytes: &[u8]) -> Result<Option<Importer>, DecodeError> {
    // The container's own errors come first, whichever importer is asked.
    Bundle::parse(bytes)?;
    let spine = crate::unity_spine::has_spine_rig(bytes);
    let cubism = crate::inspect_bundle(bytes, &mut LoadReport::new()).map(|i| i.moc.is_some());
    decide_bundle(spine, cubism)
}

/// [`detect_bundle`], with "neither shape" made an error too.
pub fn identify_bundle(bytes: &[u8]) -> Result<Importer, DecodeError> {
    detect_bundle(bytes)?.ok_or_else(|| DecodeError::Reconstruction {
        game: "unity".into(),
        message: "expected a Spine skeleton among the TextAssets, or a CubismMoc, in this bundle; found neither \
                  (name an importer with --game if the layout is one this build does not recognise)"
            .into(),
    })
}

/// The decision over the two inspections, apart from running them.
fn decide_bundle(
    spine: Result<bool, DecodeError>,
    cubism: Result<bool, DecodeError>,
) -> Result<Option<Importer>, DecodeError> {
    let both = || vec![Importer::UnitySpine.as_str().to_string(), Importer::UnityCubism.as_str().to_string()];
    match (spine, cubism) {
        (Ok(true), Ok(true)) => Err(DecodeError::Ambiguous { candidates: both() }),
        (Ok(true), Ok(false)) => Ok(Some(Importer::UnitySpine)),
        (Ok(false), Ok(true)) => Ok(Some(Importer::UnityCubism)),
        (Ok(false), Ok(false)) => Ok(None),
        // One shape was found and the other importer could not look: the
        // other cannot be ruled out, so neither is picked.
        (Ok(true), Err(e)) => Err(unruled_out(Importer::UnitySpine, Importer::UnityCubism, &e)),
        (Err(e), Ok(true)) => Err(unruled_out(Importer::UnityCubism, Importer::UnitySpine, &e)),
        (Ok(false), Err(e)) | (Err(e), Ok(false)) | (Err(e), Err(_)) => Err(e),
    }
}

fn unruled_out(found: Importer, failed: Importer, error: &DecodeError) -> DecodeError {
    DecodeError::Reconstruction {
        game: "unity".into(),
        message: format!(
            "this bundle looks like {}, but {} could not inspect it ({error}), so it cannot be ruled out; \
             use --game unity_cubism or --game unity_spine to choose",
            found.as_str(),
            failed.as_str()
        ),
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
///
/// Only the loose-file importers produce a [`generic::SpineSourceSet`]. A Unity
/// bundle holds its assets as objects rather than as files, so the bundle
/// importers go through [`import_bundle_file`] instead.
pub fn discover(importer: Importer, path: &Path) -> Result<Vec<generic::SpineSourceSet>, DecodeError> {
    match importer {
        // The `.skel.bytes` / `.atlas.txt` naming is already handled by the
        // generic stem normalisation, so discovery is shared. The importer stays
        // a distinct choice because it labels the package's `sourceGame` and is
        // where any future layout quirk belongs.
        Importer::Generic | Importer::SpineBytes => generic::discover(path),
        Importer::UnitySpine | Importer::UnityCubism => Err(DecodeError::Reconstruction {
            game: importer.as_str().to_string(),
            message: "a Unity bundle holds objects, not loose files, so it has no file set to discover; \
                      import it whole with `import_bundle_file`"
                .to_string(),
        }),
    }
}

/// Imports one discovered character.
pub fn import(importer: Importer, set: &generic::SpineSourceSet) -> Result<(Package, LoadReport), DecodeError> {
    generic::import(set, importer.as_str())
}

/// Reconstructs a package from a Unity bundle on disk, with the importer that
/// understands its contents.
///
/// A bundle holding more than one character is refused as ambiguous, since one
/// package holds one.
pub fn import_bundle_file(importer: Importer, path: &Path) -> Result<(Package, LoadReport), DecodeError> {
    if !path.is_file() {
        return Err(DecodeError::UnsupportedFormat(format!(
            "{} is not a Unity bundle (it is not a file)",
            path.display()
        )));
    }
    let bytes = std::fs::read(path).map_err(|e| DecodeError::io(path.display().to_string(), e))?;
    if !matches!(classify(&bytes), AssetKind::UnityBundle { .. }) {
        return Err(DecodeError::UnsupportedFormat(format!("{} is not a Unity bundle", path.display())));
    }
    let mut report = LoadReport::new();
    let package = match importer {
        Importer::UnityCubism => crate::import_bundle(&bytes, &mut report)?,
        Importer::UnitySpine => crate::import_spine_bundle(&bytes, &mut report)?,
        Importer::Generic | Importer::SpineBytes => {
            return Err(DecodeError::UnsupportedFormat(format!(
                "the {} importer reads loose files, not Unity bundles",
                importer.as_str()
            )))
        }
    };
    Ok((package, report))
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
    fn bundle_importers_have_no_loose_file_set_and_say_so() {
        for importer in [Importer::UnitySpine, Importer::UnityCubism] {
            let err = discover(importer, Path::new(".")).unwrap_err();
            assert!(matches!(err, DecodeError::Reconstruction { .. }), "{err}");
            assert!(err.to_string().contains(importer.as_str()), "{err}");
            assert!(err.to_string().contains("import_bundle_file"), "{err}");
        }
    }

    fn bundle_file(tag: &str, bytes: &[u8]) -> std::path::PathBuf {
        let dir = std::env::temp_dir().join(format!("a2d-games-{tag}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).expect("temp dir");
        let path = dir.join("bundle");
        std::fs::write(&path, bytes).expect("bundle");
        path
    }

    #[test]
    fn a_spine_bundle_is_identified_and_imported_as_unity_spine() {
        let bytes = crate::test_fixtures::complete_rig().build();
        assert_eq!(identify_bundle(&bytes).expect("identify"), Importer::UnitySpine);

        let path = bundle_file("spine", &bytes);
        assert_eq!(guess_importer(&path), Importer::Generic, "a bundle is identified only by `identify_bundle`");
        let (package, _) = import_bundle_file(Importer::UnitySpine, &path).expect("import");
        assert_eq!(package.manifest.source_game, "unity_spine");
        let _ = std::fs::remove_dir_all(path.parent().expect("dir"));
    }

    #[test]
    fn a_skeleton_with_no_atlas_still_identifies_as_spine() {
        let bytes = a2d_unity::testing::BundleBuilder::new()
            .text_asset("hero.skel", crate::test_fixtures::SKELETON_JSON.as_bytes())
            .build();
        assert_eq!(identify_bundle(&bytes).expect("identify"), Importer::UnitySpine);
    }

    #[test]
    fn a_bundle_of_neither_shape_is_an_error_from_the_strict_form_and_none_from_the_lenient_one() {
        let bytes = a2d_unity::testing::BundleBuilder::new().text_asset("notes.txt", b"hello").build();
        assert_eq!(detect_bundle(&bytes).expect("detect"), None);
        let err = identify_bundle(&bytes).unwrap_err().to_string();
        assert!(
            err.contains("expected a Spine skeleton") && err.contains("CubismMoc") && err.contains("found neither"),
            "{err}"
        );
    }

    #[test]
    fn bytes_that_are_not_a_bundle_report_the_containers_own_error() {
        let err = detect_bundle(b"definitely not a bundle at all").unwrap_err();
        assert!(!matches!(err, DecodeError::Ambiguous { .. }), "{err}");
    }

    fn corrupt() -> DecodeError {
        DecodeError::corrupt("synthetic".to_string())
    }

    #[test]
    fn every_combination_of_the_two_inspections_has_a_decided_outcome() {
        use Importer::{UnityCubism as C, UnitySpine as S};
        let d = decide_bundle;
        // Both inspected cleanly.
        assert_eq!(d(Ok(true), Ok(false)).unwrap(), Some(S));
        assert_eq!(d(Ok(false), Ok(true)).unwrap(), Some(C));
        assert_eq!(d(Ok(false), Ok(false)).unwrap(), None);
        match d(Ok(true), Ok(true)).unwrap_err() {
            DecodeError::Ambiguous { candidates } => assert_eq!(candidates, ["unity_spine", "unity_cubism"]),
            other => panic!("expected Ambiguous, got {other}"),
        }
        // A failure is never read as "found nothing".
        assert!(matches!(d(Err(corrupt()), Err(corrupt())).unwrap_err(), DecodeError::Corrupt { .. }));
        assert!(d(Ok(false), Err(corrupt())).is_err());
        assert!(d(Err(corrupt()), Ok(false)).is_err());
        // A shape found while the other cannot be ruled out names both.
        for err in [d(Ok(true), Err(corrupt())).unwrap_err(), d(Err(corrupt()), Ok(true)).unwrap_err()] {
            let text = err.to_string();
            assert!(
                text.contains("unity_spine") && text.contains("unity_cubism") && text.contains("cannot be ruled out"),
                "{text}"
            );
        }
    }

    #[test]
    fn a_loose_importer_refuses_a_bundle_and_a_bundle_importer_refuses_other_files() {
        let path = bundle_file("refuse", &crate::test_fixtures::complete_rig().build());
        let err = import_bundle_file(Importer::SpineBytes, &path).unwrap_err();
        assert!(err.to_string().contains("loose files"), "{err}");
        let plain = path.with_file_name("plain.txt");
        std::fs::write(&plain, b"not a bundle").expect("plain");
        let err = import_bundle_file(Importer::UnitySpine, &plain).unwrap_err();
        assert!(matches!(err, DecodeError::UnsupportedFormat(_)), "{err}");
        // A directory is named as not being a bundle, not reported as a raw IO error.
        let err = import_bundle_file(Importer::UnitySpine, path.parent().expect("dir")).unwrap_err();
        assert!(
            matches!(err, DecodeError::UnsupportedFormat(_)) && err.to_string().contains("is not a Unity bundle"),
            "{err}"
        );
        let _ = std::fs::remove_dir_all(path.parent().expect("dir"));
    }

    #[test]
    fn guessing_on_a_missing_directory_falls_back_to_generic() {
        assert_eq!(guess_importer(Path::new("no/such/place")), Importer::Generic);
    }
}
