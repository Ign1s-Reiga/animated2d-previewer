//! Game-specific importers.
//!
//! This module is the only place in the workspace where a game's name may
//! influence behaviour (spec §2). Everything here reduces to a generic source
//! package; nothing downstream learns which game an asset came from beyond the
//! informational `sourceGame` string in the manifest.

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
    /// AEONS ECHO — Spine assets exported as `.skel.bytes` / `.atlas.txt`.
    AeonsEcho,
    /// 放置少女 — Cubism inside Unity AssetBundles.
    DeposeGirls,
    /// NIKKE — lobby and standing models only.
    Nikke,
}

impl Importer {
    pub fn as_str(self) -> &'static str {
        match self {
            Importer::Generic => "generic",
            Importer::AeonsEcho => "aeons_echo",
            Importer::DeposeGirls => "depose_girls",
            Importer::Nikke => "nikke",
        }
    }

    pub fn parse(s: &str) -> Option<Importer> {
        Some(match s {
            "generic" => Importer::Generic,
            "aeons_echo" | "aeons-echo" => Importer::AeonsEcho,
            "depose_girls" | "depose-girls" => Importer::DeposeGirls,
            "nikke" => Importer::Nikke,
            _ => return None,
        })
    }

    pub fn all() -> [Importer; 4] {
        [Importer::Generic, Importer::AeonsEcho, Importer::DeposeGirls, Importer::Nikke]
    }
}

/// Guesses which importer suits a path, from the assets actually present.
///
/// Only used to pick a default; `inspect` and `import` both accept an explicit
/// choice, and an unclear guess falls back to [`Importer::Generic`] rather than
/// asserting a game.
pub fn guess_importer(path: &Path) -> Importer {
    let dir = if path.is_dir() { path.to_path_buf() } else { path.parent().unwrap_or(Path::new(".")).to_path_buf() };
    let Ok(entries) = std::fs::read_dir(&dir) else { return Importer::Generic };

    let mut has_unity = false;
    let mut has_aeons_naming = false;
    for entry in entries.flatten().take(256) {
        let file = entry.path();
        let Some(name) = file.file_name().and_then(|n| n.to_str()) else { continue };
        // AEONS ECHO's tell is the doubled suffix, not the file's content.
        if name.ends_with(".skel.bytes") || name.ends_with(".atlas.txt") {
            has_aeons_naming = true;
        }
        if let Ok(head) = read_head(&file, 64) {
            if matches!(classify(&head), AssetKind::UnityBundle { .. }) {
                has_unity = true;
            }
        }
    }

    match (has_aeons_naming, has_unity) {
        (true, _) => Importer::AeonsEcho,
        // A Unity bundle could be any of the Unity-packaged games, so the
        // importer is not guessed from that alone.
        (false, true) => Importer::Generic,
        (false, false) => Importer::Generic,
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
        // AEONS ECHO's `.skel.bytes` / `.atlas.txt` naming is already handled by
        // the generic stem normalisation, so discovery is shared. The importer
        // stays a distinct choice because it labels the package's `sourceGame`
        // and is where any future layout quirk belongs.
        Importer::Generic | Importer::AeonsEcho => generic::discover(path),
        Importer::Nikke => Err(unimplemented_importer(
            "nikke",
            "lobby model discovery inside NIKKE bundles is not implemented; \
             extract the skeleton, atlas and textures first and import them with `--game generic`",
        )),
        Importer::DeposeGirls => Err(unimplemented_importer(
            "depose_girls",
            "Cubism reconstruction from Unity AssetBundles is not implemented \
             (it depends on the unresolved Cubism Core decision)",
        )),
    }
}

/// Imports one discovered character.
pub fn import(importer: Importer, set: &generic::SpineSourceSet) -> Result<(Package, LoadReport), DecodeError> {
    generic::import(set, importer.as_str())
}

fn unimplemented_importer(game: &str, detail: &str) -> DecodeError {
    DecodeError::Reconstruction { game: game.to_string(), message: detail.to_string() }
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

    #[test]
    fn hyphenated_spellings_are_accepted() {
        assert_eq!(Importer::parse("aeons-echo"), Some(Importer::AeonsEcho));
        assert_eq!(Importer::parse("depose-girls"), Some(Importer::DeposeGirls));
    }

    #[test]
    fn an_unknown_importer_name_is_rejected() {
        assert_eq!(Importer::parse("genshin"), None);
        assert_eq!(Importer::parse(""), None);
    }

    #[test]
    fn the_unimplemented_importers_say_so_precisely() {
        let err = discover(Importer::Nikke, Path::new(".")).unwrap_err();
        assert!(matches!(err, DecodeError::Reconstruction { .. }), "{err}");
        assert!(err.to_string().contains("nikke"), "{err}");
        assert!(err.to_string().contains("--game generic"), "{err}");

        let err = discover(Importer::DeposeGirls, Path::new(".")).unwrap_err();
        assert!(err.to_string().contains("Cubism Core"), "{err}");
    }

    #[test]
    fn guessing_on_a_missing_directory_falls_back_to_generic() {
        assert_eq!(guess_importer(Path::new("no/such/place")), Importer::Generic);
    }
}
