//! The generic Spine importer: asset discovery and package reconstruction.
//!
//! Importers do discovery and reconstruction only (spec §9). This one contains
//! no game-specific knowledge; the per-game importers wrap it with their own
//! naming rules and layout expectations.

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

use a2d_core::ir::spine::SpineIr;
use a2d_core::{DecodeError, Degradation, LoadReport};
use a2d_pack::{Package, TextureFile};

use crate::detect::{classify, AssetKind};

/// Extensions that are stripped when grouping files into one character.
///
/// Spec §9.2 asks for `.skel.bytes` → `.skel` and `.atlas.txt` → `.atlas`
/// normalisation; stripping repeatedly generalises that to any stacking of
/// these suffixes without hard-coding each combination.
const STRIPPABLE: [&str; 9] = ["bytes", "txt", "skel", "atlas", "json", "png", "webp", "jpg", "jpeg"];

/// Reduces a file name to the character name it belongs to.
///
/// `hero.skel.bytes`, `hero.atlas.txt` and `hero.png` all reduce to `hero`.
pub fn normalize_asset_stem(file_name: &str) -> String {
    let mut stem = file_name;
    // Two rounds is enough for every stacked suffix real exports produce, and
    // bounding it stops `a.png.png.png` from eroding the whole name.
    for _ in 0..2 {
        let Some((head, ext)) = stem.rsplit_once('.') else { break };
        if head.is_empty() || !STRIPPABLE.contains(&ext.to_ascii_lowercase().as_str()) {
            break;
        }
        stem = head;
    }
    stem.to_string()
}

/// One character's source files, already identified by content.
#[derive(Debug, Clone)]
pub struct SpineSourceSet {
    /// Character name, from the normalised stem.
    pub name: String,
    pub skeleton: PathBuf,
    pub skeleton_version: String,
    pub atlas: Option<PathBuf>,
    /// Texture pages the atlas names, paired with the file that supplies them.
    /// A page with no file present is `None` and reported by `validate`.
    pub textures: Vec<(String, Option<PathBuf>)>,
}

/// Finds every Spine character reachable from `path`.
///
/// `path` may be a directory or a single file; a file is treated as "this
/// character, in this directory".
pub fn discover(path: &Path) -> Result<Vec<SpineSourceSet>, DecodeError> {
    let (dir, only_stem) = if path.is_dir() {
        (path.to_path_buf(), None)
    } else {
        let dir = path.parent().unwrap_or(Path::new(".")).to_path_buf();
        let name = path
            .file_name()
            .and_then(|n| n.to_str())
            .ok_or_else(|| DecodeError::corrupt(format!("{} has no usable file name", path.display())))?;
        (dir, Some(normalize_asset_stem(name)))
    };

    let files = scan_dir(&dir)?;
    let mut skeletons: Vec<(String, PathBuf, String)> = Vec::new();
    let mut atlases: BTreeMap<String, PathBuf> = BTreeMap::new();

    for (file_path, name) in &files {
        let Ok(bytes) = std::fs::read(file_path) else { continue };
        match classify(&bytes) {
            AssetKind::SpineSkeleton { version, .. } => {
                skeletons.push((normalize_asset_stem(name), file_path.clone(), version));
            }
            AssetKind::SpineAtlas => {
                atlases.insert(normalize_asset_stem(name), file_path.clone());
            }
            _ => {}
        }
    }

    if let Some(stem) = &only_stem {
        skeletons.retain(|(s, _, _)| s == stem);
        if skeletons.is_empty() {
            return Err(DecodeError::MissingSkeleton(format!(
                "{} is not a Spine skeleton and no skeleton named {stem:?} sits beside it",
                path.display()
            )));
        }
    }
    if skeletons.is_empty() {
        return Err(DecodeError::MissingSkeleton(format!("no Spine skeleton found in {}", dir.display())));
    }

    let mut out = Vec::with_capacity(skeletons.len());
    for (stem, skeleton, version) in skeletons {
        let atlas = pair_atlas(&stem, &atlases)?;
        let textures = match &atlas {
            None => Vec::new(),
            Some(atlas_path) => texture_pages(atlas_path, &dir, &files)?,
        };
        out.push(SpineSourceSet { name: stem, skeleton, skeleton_version: version, atlas, textures });
    }
    out.sort_by(|a, b| a.name.cmp(&b.name));
    Ok(out)
}

/// Chooses the atlas for a skeleton.
///
/// An exact stem match wins. Failing that, a directory holding exactly one
/// atlas is unambiguous enough to use. Two or more candidates is an error, not
/// a coin flip (spec §15).
fn pair_atlas(stem: &str, atlases: &BTreeMap<String, PathBuf>) -> Result<Option<PathBuf>, DecodeError> {
    if let Some(exact) = atlases.get(stem) {
        return Ok(Some(exact.clone()));
    }
    match atlases.len() {
        0 => Ok(None),
        1 => Ok(atlases.values().next().cloned()),
        _ => Err(DecodeError::Ambiguous { candidates: atlases.keys().map(|k| format!("{k}.atlas")).collect() }),
    }
}

/// Resolves each page the atlas names to a file on disk.
fn texture_pages(
    atlas_path: &Path,
    dir: &Path,
    files: &[(PathBuf, String)],
) -> Result<Vec<(String, Option<PathBuf>)>, DecodeError> {
    let text = std::fs::read_to_string(atlas_path).map_err(|e| DecodeError::io(atlas_path.display().to_string(), e))?;
    let (atlas, _) = a2d_spine::parse_atlas(&text)?;

    Ok(atlas
        .pages
        .iter()
        .map(|page| {
            let direct = dir.join(&page.name);
            if direct.is_file() {
                return (page.name.clone(), Some(direct));
            }
            // Exports do occasionally disagree with the atlas about case, or
            // append a suffix such as `.png.bytes`.
            let wanted = normalize_asset_stem(&page.name).to_ascii_lowercase();
            let found = files.iter().find(|(path, name)| {
                normalize_asset_stem(name).to_ascii_lowercase() == wanted
                    && std::fs::read(path).map(|b| matches!(classify(&b), AssetKind::Texture(_))).unwrap_or(false)
            });
            (page.name.clone(), found.map(|(path, _)| path.clone()))
        })
        .collect())
}

fn scan_dir(dir: &Path) -> Result<Vec<(PathBuf, String)>, DecodeError> {
    let entries = std::fs::read_dir(dir).map_err(|e| DecodeError::io(dir.display().to_string(), e))?;
    let mut out = Vec::new();
    for entry in entries.flatten() {
        let path = entry.path();
        if !path.is_file() {
            continue;
        }
        let Some(name) = path.file_name().and_then(|n| n.to_str()) else { continue };
        out.push((path.clone(), name.to_string()));
    }
    // Sorted so discovery is deterministic regardless of filesystem order.
    out.sort_by(|a, b| a.1.cmp(&b.1));
    Ok(out)
}

/// Decodes a discovered character into a package.
pub fn import(set: &SpineSourceSet, source_game: &str) -> Result<(Package, LoadReport), DecodeError> {
    let mut report = LoadReport::new();
    let ir = decode_ir(set, &mut report)?;

    let mut package = Package::from_spine(ir, &set.name);
    package.manifest.source_game = source_game.to_string();

    for (page, path) in &set.textures {
        match path {
            Some(path) => {
                let bytes = std::fs::read(path).map_err(|e| DecodeError::io(path.display().to_string(), e))?;
                package.textures.push(TextureFile { file: page.clone(), bytes });
            }
            None => report.warn(Degradation::MissingReference { kind: "texture page".into(), name: page.clone() }),
        }
    }

    package.manifest.import_warnings = report.warnings().iter().map(|w| w.to_string()).collect();
    Ok((package, report))
}

/// Decodes just the IR, without reading texture bytes. Used by `inspect`.
pub fn decode_ir(set: &SpineSourceSet, report: &mut LoadReport) -> Result<SpineIr, DecodeError> {
    let atlas = match &set.atlas {
        None => {
            report.warn(Degradation::MissingReference { kind: "atlas".into(), name: format!("{}.atlas", set.name) });
            a2d_core::ir::atlas::Atlas::default()
        }
        Some(path) => {
            let text = std::fs::read_to_string(path).map_err(|e| DecodeError::io(path.display().to_string(), e))?;
            let (mut atlas, atlas_report) = a2d_spine::parse_atlas(&text)?;
            report.absorb(atlas_report);
            fill_missing_page_sizes(&mut atlas, set, report);
            atlas
        }
    };

    let bytes = std::fs::read(&set.skeleton).map_err(|e| DecodeError::io(set.skeleton.display().to_string(), e))?;
    let (ir, _) = a2d_spine::decode_skeleton(&bytes, atlas, report)?;
    Ok(ir)
}

/// Fills in page sizes that the atlas omitted, by reading the image header.
///
/// UV computation needs the page size, and older atlas dialects leave it out.
fn fill_missing_page_sizes(atlas: &mut a2d_core::ir::atlas::Atlas, set: &SpineSourceSet, report: &mut LoadReport) {
    for page in &mut atlas.pages {
        if page.size.is_some() {
            continue;
        }
        let path = set.textures.iter().find(|(name, _)| *name == page.name).and_then(|(_, p)| p.as_ref());
        let size = path.and_then(|p| std::fs::read(p).ok()).and_then(|bytes| png_size(&bytes));
        match size {
            Some(size) => page.size = Some(size),
            None => report.warn(Degradation::MissingReference {
                kind: "page size (atlas omitted it and the image could not be measured)".into(),
                name: page.name.clone(),
            }),
        }
    }
}

/// Reads width and height from a PNG's `IHDR` chunk.
///
/// Only PNG is handled: it is what every target export uses, and pulling in a
/// full image decoder here would put decoding in the wrong layer.
pub fn png_size(bytes: &[u8]) -> Option<(u32, u32)> {
    const PNG: &[u8; 8] = b"\x89PNG\r\n\x1a\n";
    if bytes.len() < 24 || &bytes[..8] != PNG || &bytes[12..16] != b"IHDR" {
        return None;
    }
    let width = u32::from_be_bytes([bytes[16], bytes[17], bytes[18], bytes[19]]);
    let height = u32::from_be_bytes([bytes[20], bytes[21], bytes[22], bytes[23]]);
    (width > 0 && height > 0).then_some((width, height))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn compound_suffixes_normalise_to_the_character_name() {
        assert_eq!(normalize_asset_stem("hero.skel.bytes"), "hero");
        assert_eq!(normalize_asset_stem("hero.atlas.txt"), "hero");
        assert_eq!(normalize_asset_stem("hero.json.txt"), "hero");
        assert_eq!(normalize_asset_stem("hero.png"), "hero");
        assert_eq!(normalize_asset_stem("hero.skel"), "hero");
        assert_eq!(normalize_asset_stem("hero.atlas"), "hero");
    }

    #[test]
    fn normalisation_is_case_insensitive_about_the_extension() {
        assert_eq!(normalize_asset_stem("hero.SKEL.BYTES"), "hero");
        assert_eq!(normalize_asset_stem("hero.PNG"), "hero");
    }

    #[test]
    fn unknown_extensions_are_left_alone() {
        assert_eq!(normalize_asset_stem("hero.moc3"), "hero.moc3");
        assert_eq!(normalize_asset_stem("hero"), "hero");
        assert_eq!(normalize_asset_stem("hero.v2.skel"), "hero.v2");
    }

    #[test]
    fn a_dotfile_is_not_eroded_to_nothing() {
        assert_eq!(normalize_asset_stem(".png"), ".png");
    }

    #[test]
    fn stripping_is_bounded() {
        // Three stacked suffixes strip only two, which keeps the name non-empty.
        assert_eq!(normalize_asset_stem("a.png.png.png"), "a.png");
    }

    #[test]
    fn png_dimensions_are_read_from_the_header() {
        let mut png = b"\x89PNG\r\n\x1a\n".to_vec();
        png.extend_from_slice(&13u32.to_be_bytes());
        png.extend_from_slice(b"IHDR");
        png.extend_from_slice(&1024u32.to_be_bytes());
        png.extend_from_slice(&2048u32.to_be_bytes());
        png.extend_from_slice(&[8, 6, 0, 0, 0]);
        assert_eq!(png_size(&png), Some((1024, 2048)));
    }

    #[test]
    fn a_non_png_has_no_readable_size() {
        assert_eq!(png_size(b"not a png at all, really not"), None);
        assert_eq!(png_size(&[]), None);
    }

    #[test]
    fn a_zero_sized_png_header_is_rejected() {
        let mut png = b"\x89PNG\r\n\x1a\n".to_vec();
        png.extend_from_slice(&13u32.to_be_bytes());
        png.extend_from_slice(b"IHDR");
        png.extend_from_slice(&0u32.to_be_bytes());
        png.extend_from_slice(&0u32.to_be_bytes());
        png.extend_from_slice(&[8, 6, 0, 0, 0]);
        assert_eq!(png_size(&png), None);
    }

    fn atlases(names: &[&str]) -> BTreeMap<String, PathBuf> {
        names.iter().map(|n| ((*n).to_string(), PathBuf::from(format!("{n}.atlas")))).collect()
    }

    #[test]
    fn an_exact_stem_match_wins() {
        let found = pair_atlas("hero", &atlases(&["hero", "villain"])).unwrap();
        assert_eq!(found, Some(PathBuf::from("hero.atlas")));
    }

    #[test]
    fn a_lone_atlas_is_used_even_without_a_stem_match() {
        let found = pair_atlas("skeleton", &atlases(&["hero"])).unwrap();
        assert_eq!(found, Some(PathBuf::from("hero.atlas")));
    }

    #[test]
    fn two_candidate_atlases_are_ambiguous_rather_than_guessed() {
        let err = pair_atlas("skeleton", &atlases(&["hero", "villain"])).unwrap_err();
        assert!(matches!(err, DecodeError::Ambiguous { .. }), "{err}");
        assert!(err.to_string().contains("hero.atlas"), "{err}");
        assert!(err.to_string().contains("villain.atlas"), "{err}");
    }

    #[test]
    fn no_atlas_at_all_is_not_an_error_here() {
        // The missing atlas is reported during decode, so a skeleton-only
        // directory can still be inspected.
        assert_eq!(pair_atlas("hero", &atlases(&[])).unwrap(), None);
    }
}
