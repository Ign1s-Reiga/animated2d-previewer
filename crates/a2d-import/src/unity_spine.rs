//! Finding a Spine rig inside a Unity AssetBundle.
//!
//! Like the Cubism importer beside it, this is discovery only: it says what is
//! in the bundle and hands back the payloads. Decoding them belongs to
//! `a2d-spine`, and nothing downstream learns that the assets came out of Unity
//! (spec §2).
//!
//! # What a Spine rig looks like once Unity has packed it
//!
//! Considerably less is lost than on the Cubism side. The spine-unity runtime
//! does not convert anything: it keeps the skeleton and the atlas as
//! `TextAsset`s, byte for byte as the Spine editor exported them, and points
//! `SkeletonDataAsset` and `SpineAtlasAsset` at them. Verified against a real
//! 2022.3 bundle, which held:
//!
//! * `TextAsset` `<name>.skel` — the binary skeleton, unmodified. Our existing
//!   decoder reads it with no Unity-specific handling at all.
//! * `TextAsset` `<name>.atlas` — the atlas, as text.
//! * `Texture2D` — the atlas page.
//! * `MonoScript` entries naming `SkeletonDataAsset`, `SpineAtlasAsset` and
//!   `SkeletonAnimation`, which is what marks the bundle as a Spine one.
//!
//! So the reconstruction is genuinely just extraction. That is worth stating
//! plainly, because it means a bug here can only be a discovery bug — the
//! bytes handed on are the editor's own.
//!
//! # Scope
//!
//! Spec §9 limits this importer to **standing / idle models**. Nothing here
//! filters by that yet, because the bundles seen hold one rig each. A bundle
//! holding several is reported by [`discover_spine_bundle`], and
//! [`import_spine_bundle`] refuses it as ambiguous instead of choosing; when
//! one turns up, this is where the choice belongs, not downstream.
//!
//! # Import
//!
//! [`discover_spine_bundle`] turns a bundle into [`SpineRig`]s, which hold only
//! format-level payloads (skeleton bytes, atlas text, decoded pages), and
//! [`import_rig`] hands those to the same decoder loose files use. The result is
//! a package with `sourceGame` `unity_spine`; nothing in it is Unity-shaped.

use std::collections::BTreeMap;

use a2d_core::ir::atlas::Atlas;
use a2d_core::ir::spine::SpineIr;
use a2d_core::{DecodeError, Degradation, LoadReport};
use a2d_pack::{Package, TextureFile};
use a2d_unity::{Bundle, ClassId, Endian, Inventory, ObjectInfo, Reader, SerializedFile};

use crate::detect::{classify, AssetKind};
use crate::games::Importer;
use crate::generic::{normalize_asset_stem, pair_atlas};
use crate::png::encode_png;
/// A `TextAsset` payload and how it was found.
#[derive(Debug, Clone)]
pub struct SpineTextAsset {
    /// The asset's own name, which keeps its original extension.
    pub name: String,
    /// Path the asset was authored under, when the bundle still records it.
    pub asset_path: Option<String>,
    pub bytes: Vec<u8>,
}

/// A `Texture2D` listed in a bundle.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SpineTextureEntry {
    pub name: String,
    pub asset_path: Option<String>,
}

/// One decoded atlas page: straight RGBA, top row first.
#[derive(Clone, PartialEq, Eq)]
pub struct PageImage {
    pub width: u32,
    pub height: u32,
    pub rgba: Vec<u8>,
}

impl std::fmt::Debug for PageImage {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "PageImage({}x{}, {} bytes)", self.width, self.height, self.rgba.len())
    }
}

/// One character found in a bundle: format-level payloads, no Unity objects.
#[derive(Debug, Clone)]
pub struct SpineRig {
    /// Character name, from the skeleton asset's normalised stem.
    pub name: String,
    /// The skeleton asset's own name.
    pub skeleton_name: String,
    /// Path the skeleton was authored under, when the bundle records it.
    pub skeleton_path: Option<String>,
    pub skeleton: Vec<u8>,
    pub skeleton_version: String,
    /// The paired atlas asset's name, or `None` when the bundle holds none.
    pub atlas_name: Option<String>,
    /// The atlas text.
    pub atlas: Option<Vec<u8>>,
    /// Each page the atlas names, with the image that supplies it. A page the
    /// bundle has no (decodable) texture for is `None`.
    pub pages: Vec<(String, Option<PageImage>)>,
}

/// Everything found in one bundle.
#[derive(Debug, Clone)]
pub struct SpineInventory {
    /// The exact player revision the bundle was built with.
    pub unity_revision: String,
    /// Total objects in the serialized file.
    pub object_count: usize,
    /// Every rig, each with the atlas and pages it was paired with.
    pub rigs: Vec<SpineRig>,
    pub textures: Vec<SpineTextureEntry>,
    /// spine-unity component classes present, which is what marks the bundle.
    pub components: Vec<String>,
    /// `TextAsset`s that are neither a skeleton nor an atlas.
    pub other_text_assets: Vec<String>,
}

/// The spine-unity classes that identify a bundle as holding a rig.
const SPINE_COMPONENTS: &[&str] =
    &["SkeletonDataAsset", "SpineAtlasAsset", "SkeletonAnimation", "SkeletonGraphic", "SkeletonRenderer"];

/// Everything one pass over a bundle finds, before any choice between
/// candidates is made.
struct Scan {
    unity_revision: String,
    file: SerializedFile,
    object_count: usize,
    components: Vec<String>,
    /// Path id and name of every `Texture2D`; none is decoded yet.
    texture_objects: Vec<(i64, SpineTextureEntry)>,
    skeletons: Vec<(SpineTextAsset, AssetKind)>,
    atlases: Vec<SpineTextAsset>,
    other_text_assets: Vec<String>,
}

/// Parses a bundle and sorts its `TextAsset`s by what their bytes are.
fn scan_bundle(bytes: &[u8], report: &mut LoadReport) -> Result<Scan, DecodeError> {
    let bundle = Bundle::parse(bytes)?;
    let node = bundle.nodes.iter().find(|n| n.is_serialized()).ok_or_else(|| DecodeError::Reconstruction {
        game: "unity_spine".into(),
        message: format!(
            "the bundle holds no serialized file, only {:?}",
            bundle.nodes.iter().map(|n| &n.path).collect::<Vec<_>>()
        ),
    })?;
    let file = SerializedFile::parse(bundle.node_data(node)?)?;
    let inventory = Inventory::build(&file);

    let components: Vec<String> = SPINE_COMPONENTS
        .iter()
        .filter(|name| inventory.by_script(name).next().is_some())
        .map(|name| (*name).to_string())
        .collect();

    // Every `TextAsset`, classified by content. The names do carry `.skel` and
    // `.atlas`, but spec §10 forbids deciding on an extension alone, and the
    // content check is what makes the version report trustworthy anyway.
    let (mut skeletons, mut atlases, mut other_text_assets) = (Vec::new(), Vec::new(), Vec::new());
    for object in inventory.by_class(ClassId::TEXT_ASSET) {
        let payload = match read_text_asset(&file, object) {
            Ok(payload) => payload,
            Err(e) => {
                report.warn(Degradation::Note(format!(
                    "a TextAsset in this bundle could not be read and was skipped: {e}"
                )));
                continue;
            }
        };
        match classify(&payload.bytes) {
            kind @ AssetKind::SpineSkeleton { .. } => skeletons.push((payload, kind)),
            AssetKind::SpineAtlas => atlases.push(payload),
            _ => other_text_assets.push(payload.name),
        }
    }

    let texture_objects = inventory
        .by_class(ClassId::TEXTURE_2D)
        .map(|o| {
            (
                o.path_id,
                SpineTextureEntry {
                    name: o.name.clone().unwrap_or_else(|| format!("texture#{}", o.path_id)),
                    asset_path: o.asset_path.clone(),
                },
            )
        })
        .collect();

    Ok(Scan {
        unity_revision: bundle.unity_revision.clone(),
        object_count: inventory.objects.len(),
        file,
        components,
        texture_objects,
        skeletons,
        atlases,
        other_text_assets,
    })
}

/// Whether a bundle holds a Spine skeleton.
///
/// A skeleton alone is enough, because that is all [`import_spine_bundle`]
/// needs: without an atlas the rig imports in a reduced state and says so.
/// Errors are the bundle's own parse or corruption errors, never `false`. So is
/// an unreadable `TextAsset` when no skeleton was found: the unreadable one may
/// have been it, so "no Spine rig here" cannot be said. (When a skeleton was
/// found the unreadable asset is noted by the inspection that follows.)
pub(crate) fn has_spine_rig(bytes: &[u8]) -> Result<bool, DecodeError> {
    let mut report = LoadReport::new();
    let scan = scan_bundle(bytes, &mut report)?;
    if !scan.skeletons.is_empty() {
        return Ok(true);
    }
    if report.is_empty() {
        return Ok(false);
    }
    Err(DecodeError::Reconstruction {
        game: "unity_spine".into(),
        message: format!(
            "no Spine skeleton was found, but not every TextAsset could be read, so one may have been it (use --game unity_cubism or --game unity_spine to choose): {}",
            report.warnings().iter().map(ToString::to_string).collect::<Vec<_>>().join("; ")
        ),
    })
}

/// The file stem of a name: its last path component, with one extension
/// removed.
fn file_stem(name: &str) -> &str {
    let last = name.rsplit(['/', '\\']).next().unwrap_or(name);
    match last.rsplit_once('.') {
        Some((head, _)) if !head.is_empty() => head,
        _ => last,
    }
}

/// The texture an atlas page names.
///
/// An exact stem match is tried first and the case-insensitive one only when
/// that finds nothing, so two pages that differ only by case each find their
/// own. Two candidates at the winning tier is an error, never a pick.
fn find_texture<'a>(
    textures: &'a [(i64, SpineTextureEntry)],
    page: &str,
) -> Result<Option<&'a (i64, SpineTextureEntry)>, DecodeError> {
    let wanted = file_stem(page);
    for exact in [true, false] {
        let hits: Vec<&(i64, SpineTextureEntry)> = textures
            .iter()
            .filter(|(_, t)| {
                let stem = file_stem(&t.name);
                if exact {
                    stem == wanted
                } else {
                    stem.eq_ignore_ascii_case(wanted)
                }
            })
            .collect();
        match hits.as_slice() {
            [] => continue,
            [one] => return Ok(Some(one)),
            many => {
                return Err(DecodeError::Ambiguous {
                    candidates: many.iter().map(|(_, t)| format!("Texture2D {}", t.name)).collect(),
                })
            }
        }
    }
    Ok(None)
}

/// Finds every rig in a scanned bundle and what each one is paired with.
fn build_inventory(scan: Scan, report: &mut LoadReport) -> Result<SpineInventory, DecodeError> {
    if scan.skeletons.is_empty() {
        return Err(DecodeError::MissingSkeleton(format!(
            "expected a Spine skeleton among the TextAssets of this bundle, found none \
             ({} atlases and {} other TextAssets, {} Texture2Ds, {} objects)",
            scan.atlases.len(),
            scan.other_text_assets.len(),
            scan.texture_objects.len(),
            scan.object_count
        )));
    }

    let mut atlases: BTreeMap<String, SpineTextAsset> = BTreeMap::new();
    for atlas in scan.atlases {
        let stem = normalize_asset_stem(&atlas.name);
        if let Some(previous) = atlases.insert(stem.clone(), atlas) {
            return Err(DecodeError::Ambiguous {
                candidates: vec![format!("{} (atlas)", previous.name), format!("{stem} (a second atlas of that name)")],
            });
        }
    }

    let mut rigs = Vec::with_capacity(scan.skeletons.len());
    for (skeleton, kind) in scan.skeletons {
        let AssetKind::SpineSkeleton { version, .. } = kind else { continue };
        let stem = normalize_asset_stem(&skeleton.name);
        let atlas = pair_atlas(&stem, &atlases)?;
        if let Some(atlas) = &atlas {
            if !atlases.contains_key(&stem) {
                report.warn(Degradation::Note(format!(
                    "skeleton {:?} has no atlas of its name; using the bundle's only atlas {:?}",
                    skeleton.name, atlas.name
                )));
            }
        }

        let mut pages = Vec::new();
        if let Some(atlas) = &atlas {
            let (parsed, _) = a2d_spine::parse_atlas(&String::from_utf8_lossy(&atlas.bytes))?;
            for page in &parsed.pages {
                // Only the texture this page names is decoded, so an unrelated
                // one in a format not read here cannot fail the import.
                let image = match find_texture(&scan.texture_objects, &page.name)? {
                    None => None,
                    Some((path_id, entry)) => match a2d_unity::read_texture_object(&scan.file, *path_id) {
                        Ok(t) => Some(PageImage { width: t.width, height: t.height, rgba: t.rgba }),
                        Err(e) => {
                            report.warn(Degradation::Note(format!(
                                "texture page {:?} (Texture2D {:?}) could not be decoded and is treated as missing: {e}",
                                page.name, entry.name
                            )));
                            None
                        }
                    },
                };
                pages.push((page.name.clone(), image));
            }
        }
        rigs.push(SpineRig {
            name: stem,
            skeleton_name: skeleton.name,
            skeleton_path: skeleton.asset_path,
            skeleton: skeleton.bytes,
            skeleton_version: version,
            atlas_name: atlas.as_ref().map(|a| a.name.clone()),
            atlas: atlas.map(|a| a.bytes),
            pages,
        });
    }
    rigs.sort_by(|a, b| a.name.cmp(&b.name));

    Ok(SpineInventory {
        unity_revision: scan.unity_revision,
        object_count: scan.object_count,
        rigs,
        textures: scan.texture_objects.into_iter().map(|(_, t)| t).collect(),
        components: scan.components,
        other_text_assets: scan.other_text_assets,
    })
}

/// Reads a bundle and reports every Spine rig in it, with the atlas and pages
/// each was paired with.
///
/// Mirrors loose-file discovery: skeletons are recognised by content, an atlas
/// is paired by stem (or is the bundle's only one), and each atlas page is
/// resolved to a `Texture2D` by file stem. Two candidates for one slot is an
/// error, never a pick. A bundle with no skeleton is an error that says what
/// was searched rather than an empty success.
pub fn inspect_spine_bundle(bytes: &[u8], report: &mut LoadReport) -> Result<SpineInventory, DecodeError> {
    build_inventory(scan_bundle(bytes, report)?, report)
}

/// Finds every Spine rig in a bundle.
pub fn discover_spine_bundle(bytes: &[u8], report: &mut LoadReport) -> Result<Vec<SpineRig>, DecodeError> {
    Ok(inspect_spine_bundle(bytes, report)?.rigs)
}

/// The one rig of a bundle holding exactly one; several is `Ambiguous`.
///
/// This is the single place the one-character rule lives: a package holds one
/// character, so `import` and `preview` both ask for it here.
pub fn discover_single_rig(bytes: &[u8], report: &mut LoadReport) -> Result<SpineRig, DecodeError> {
    let mut rigs = discover_spine_bundle(bytes, report)?;
    if rigs.len() == 1 {
        return Ok(rigs.remove(0));
    }
    Err(DecodeError::Ambiguous { candidates: rigs.iter().map(|r| r.name.clone()).collect() })
}

/// Decodes a discovered rig's skeleton and atlas into the Spine IR.
///
/// Both go to the same decoder loose files use. A missing atlas or page size
/// degrades the load and is reported through `report`.
pub fn decode_rig(rig: &SpineRig, report: &mut LoadReport) -> Result<SpineIr, DecodeError> {
    let mut atlas = match &rig.atlas {
        None => {
            report.warn(Degradation::MissingReference { kind: "atlas".into(), name: format!("{}.atlas", rig.name) });
            Atlas::default()
        }
        Some(bytes) => {
            let (atlas, atlas_report) = a2d_spine::parse_atlas(&String::from_utf8_lossy(bytes))?;
            report.absorb(atlas_report);
            atlas
        }
    };
    // Older atlas dialects omit the page size; the texture knows it.
    for page in &mut atlas.pages {
        if page.size.is_some() {
            continue;
        }
        let image = rig.pages.iter().find(|(name, _)| *name == page.name).and_then(|(_, t)| t.as_ref());
        match image {
            Some(t) => page.size = Some((t.width, t.height)),
            None => report.warn(Degradation::MissingReference {
                kind: "page size (atlas omitted it and the texture is not in the bundle)".into(),
                name: page.name.clone(),
            }),
        }
    }

    let (ir, _) = a2d_spine::decode_skeleton(&rig.skeleton, atlas, report)?;
    Ok(ir)
}

/// Flat, safe file names for the pages of one package, in page order.
///
/// A page is stored as a file under `textures/`, and the package finds it by
/// the page's name, so a name with a folder in it (`images/hero.tga`) cannot be
/// used as is. Names that are already plain are kept; the rest become the last
/// path component's stem plus `.png` (the pages are re-encoded as PNG anyway),
/// with anything unsafe replaced (Windows-forbidden characters, trailing dots,
/// reserved device names) and a counter added if two collide, compared without
/// case.
fn flat_page_names(names: &[&str]) -> Vec<String> {
    let mut out: Vec<String> = Vec::with_capacity(names.len());
    for name in names {
        let plain = a2d_pack::package::safe_file_name(name).map(|n| n == *name).unwrap_or(false);
        let mut candidate = if plain {
            (*name).to_string()
        } else {
            let stem: String = file_stem(name)
                .chars()
                .map(|c| if c.is_ascii_alphanumeric() || matches!(c, '_' | '-' | '.') { c } else { '_' })
                .collect();
            let stem = stem.trim_matches(['_', '.']);
            let mut flat = format!("{}.png", if stem.is_empty() { "page" } else { stem });
            // A device name (`CON.png`) cannot be a file on Windows.
            if a2d_pack::package::safe_file_name(&flat).is_err() {
                flat = format!("page_{flat}");
            }
            flat
        };
        // Compared without case: `Hero.png` and `hero.png` are one file on
        // Windows and macOS, so they must not both be kept.
        let base = candidate.rsplit_once('.').map(|(b, _)| b.to_string()).unwrap_or_else(|| candidate.clone());
        let mut n = 2;
        while out.iter().any(|o| o.to_lowercase() == candidate.to_lowercase()) {
            candidate = format!("{base}_{n}.png");
            n += 1;
        }
        out.push(candidate);
    }
    out
}

/// Decodes one discovered rig into a package.
///
/// Decoding is [`decode_rig`]; the pages are re-encoded as PNG so the package
/// holds nothing Unity-shaped. A missing atlas or page degrades the load and is
/// reported, as for loose files; the report is also recorded in the manifest.
pub(crate) fn import_rig(rig: &SpineRig, report: &mut LoadReport) -> Result<Package, DecodeError> {
    let mut ir = decode_rig(rig, report)?;
    // The atlas names pages the way the source did; the package finds a page's
    // file by that name, so both take the flat name.
    let names = flat_page_names(&rig.pages.iter().map(|(n, _)| n.as_str()).collect::<Vec<_>>());
    for (page, name) in ir.atlas.pages.iter_mut().zip(&names) {
        page.name = name.clone();
    }
    let mut package = Package::from_spine(ir, &rig.name);
    package.manifest.source_game = Importer::UnitySpine.as_str().to_string();

    for ((page, image), name) in rig.pages.iter().zip(&names) {
        match image {
            Some(t) => {
                let png = encode_png(t.width, t.height, &t.rgba, page)?;
                package.textures.push(TextureFile { file: name.clone(), bytes: png });
            }
            None => report.warn(Degradation::MissingReference { kind: "texture page".into(), name: page.clone() }),
        }
    }
    package.manifest.import_warnings = report.warnings().iter().map(|w| w.to_string()).collect();
    Ok(package)
}

/// Reconstructs a loadable package from a bundle holding one Spine rig.
///
/// A bundle holding several is ambiguous: this returns an error naming them
/// rather than choosing one.
pub fn import_spine_bundle(bytes: &[u8], report: &mut LoadReport) -> Result<Package, DecodeError> {
    let rig = discover_single_rig(bytes, report)?;
    import_rig(&rig, report)
}

/// Reads one `TextAsset`: a name, then the payload as a length-prefixed blob.
fn read_text_asset(file: &SerializedFile, object: &ObjectInfo) -> Result<SpineTextAsset, DecodeError> {
    let entry =
        file.objects.iter().find(|o| o.path_id == object.path_id).ok_or_else(|| {
            DecodeError::corrupt(format!("TextAsset {} vanished from the object table", object.path_id))
        })?;
    let raw = file.object_data(entry)?;
    let mut reader = Reader::new(raw, Endian::Little);
    let name = reader.string()?;
    let len = reader.i32()?;
    if len < 0 {
        return Err(DecodeError::corrupt(format!("TextAsset {name:?} declares {len} bytes")));
    }
    let payload = reader.bytes(len as usize)?.to_vec();
    Ok(SpineTextAsset { name, asset_path: object.asset_path.clone(), bytes: payload })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::test_fixtures::{atlas_text, complete_rig, pixels, SKELETON_JSON};
    use a2d_unity::testing::BundleBuilder;

    fn discover(builder: &BundleBuilder) -> Result<Vec<SpineRig>, DecodeError> {
        discover_spine_bundle(&builder.build(), &mut LoadReport::new())
    }

    fn notes(report: &LoadReport) -> Vec<String> {
        report.warnings().iter().map(ToString::to_string).collect()
    }

    #[test]
    fn a_complete_rig_is_discovered_and_imports_into_a_loadable_package() {
        let rigs = discover(&complete_rig()).expect("discover");
        assert_eq!(rigs.len(), 1);
        assert_eq!(rigs[0].name, "hero");
        assert_eq!(rigs[0].skeleton_version, "4.1.00");
        assert_eq!(rigs[0].atlas_name.as_deref(), Some("hero.atlas"));
        assert_eq!(rigs[0].pages.len(), 1);
        assert!(rigs[0].pages[0].1.is_some(), "the page resolves to the Texture2D named for it");

        let mut report = LoadReport::new();
        let package = import_spine_bundle(&complete_rig().build(), &mut report).expect("import");
        assert_eq!(package.manifest.source_game, "unity_spine");
        assert_eq!(package.manifest.display_name, "hero");
        assert_eq!(package.manifest.default_animation.as_deref(), Some("idle"));
        assert_eq!(package.textures.len(), 1);
        assert_eq!(package.textures[0].file, "hero.png");
        assert!(package.validate().is_empty(), "{:?}", package.validate());
        assert!(package.manifest.import_warnings.is_empty(), "{:?}", package.manifest.import_warnings);

        // The package survives a trip through disk, which is what `preview` does.
        let dir = std::env::temp_dir().join(format!("a2d-unity-spine-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        package.write_to(&dir).expect("write");
        let back = Package::read_from(&dir).expect("read");
        assert!(back.model.as_spine().is_some());
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn the_page_pixels_survive_the_trip_top_row_first() {
        let package = import_spine_bundle(&complete_rig().build(), &mut LoadReport::new()).expect("import");
        let png = &package.textures[0].bytes;
        let decoder = png::Decoder::new(std::io::Cursor::new(png));
        let mut reader = decoder.read_info().expect("png header");
        let mut buf = vec![0; reader.output_buffer_size().expect("size")];
        let info = reader.next_frame(&mut buf).expect("frame");
        assert_eq!((info.width, info.height), (2, 2));
        // Unity stores the bottom row first; the package holds the top row first.
        assert_eq!(&buf[..8], &pixels()[8..], "the first row of the PNG is the last Unity stored");
    }

    #[test]
    fn a_bundle_with_no_skeleton_says_what_it_searched_and_is_not_an_empty_success() {
        let builder = BundleBuilder::new()
            .text_asset("hero.atlas", atlas_text("hero.png").as_bytes())
            .text_asset("readme", b"hello")
            .texture("hero", 2, 2, &pixels());
        let err = discover(&builder).unwrap_err();
        assert!(matches!(err, DecodeError::MissingSkeleton(_)), "{err}");
        let text = err.to_string();
        assert!(text.contains("expected a Spine skeleton") && text.contains("found none"), "{text}");
        assert!(text.contains("1 atlases") && text.contains("1 other TextAssets"), "{text}");
    }

    #[test]
    fn a_skeleton_alone_is_a_spine_bundle_and_imports_reduced() {
        let builder = BundleBuilder::new().text_asset("hero.skel", SKELETON_JSON.as_bytes());
        assert!(has_spine_rig(&builder.build()).expect("scan"), "detection must agree with import");
        let rigs = discover(&builder).expect("discover");
        assert!(rigs[0].atlas.is_none() && rigs[0].atlas_name.is_none());
        let mut report = LoadReport::new();
        let package = import_rig(&rigs[0], &mut report).expect("import");
        let warnings = notes(&report);
        assert!(warnings.iter().any(|w| w.contains("atlas") && w.contains("hero.atlas")), "{warnings:?}");
        assert_eq!(package.manifest.import_warnings, warnings, "the bundle-level notes travel with the package");
    }

    #[test]
    fn a_bundle_with_only_an_atlas_is_not_a_spine_bundle() {
        let builder = BundleBuilder::new().text_asset("hero.atlas", atlas_text("hero.png").as_bytes());
        assert!(!has_spine_rig(&builder.build()).expect("scan"));
    }

    #[test]
    fn a_missing_texture_page_is_reported_by_name() {
        let builder = BundleBuilder::new()
            .text_asset("hero.skel", SKELETON_JSON.as_bytes())
            .text_asset("hero.atlas", atlas_text("hero.png").as_bytes());
        let rigs = discover(&builder).expect("discover");
        assert_eq!(rigs[0].pages, vec![("hero.png".to_string(), None)]);
        let mut report = LoadReport::new();
        let package = import_rig(&rigs[0], &mut report).expect("import");
        assert!(package.textures.is_empty());
        let warnings = package.manifest.import_warnings.join("\n");
        assert!(warnings.contains("texture page") && warnings.contains("hero.png"), "{warnings}");
        assert!(!package.validate().is_empty(), "validate must also flag the page");
    }

    #[test]
    fn two_skeletons_are_two_rigs_and_the_single_rig_rule_refuses_to_pick() {
        let builder = complete_rig().text_asset("zed.skel", SKELETON_JSON.as_bytes());
        let rigs = discover(&builder).expect("discover");
        assert_eq!(rigs.iter().map(|r| r.name.as_str()).collect::<Vec<_>>(), ["hero", "zed"]);
        // `zed` has no atlas of its own and there is only one atlas, so it shares it.
        assert!(rigs[1].atlas.is_some());

        for err in [
            import_spine_bundle(&builder.build(), &mut LoadReport::new()).unwrap_err(),
            discover_single_rig(&builder.build(), &mut LoadReport::new()).unwrap_err(),
        ] {
            match err {
                DecodeError::Ambiguous { candidates } => assert_eq!(candidates, ["hero", "zed"]),
                other => panic!("expected Ambiguous, got {other}"),
            }
        }
    }

    #[test]
    fn using_the_only_atlas_under_another_name_is_noted() {
        let builder = BundleBuilder::new()
            .text_asset("hero.skel", SKELETON_JSON.as_bytes())
            .text_asset("sheet.atlas", atlas_text("sheet.png").as_bytes())
            .texture("sheet", 2, 2, &pixels());
        let mut report = LoadReport::new();
        let rigs = discover_spine_bundle(&builder.build(), &mut report).expect("discover");
        assert_eq!(rigs[0].atlas_name.as_deref(), Some("sheet.atlas"));
        let text = notes(&report).join("\n");
        assert!(
            text.contains("hero.skel") && text.contains("no atlas of its name") && text.contains("sheet.atlas"),
            "{text}"
        );

        // A pairing by stem needs no such note.
        let mut report = LoadReport::new();
        discover_spine_bundle(&complete_rig().build(), &mut report).expect("discover");
        assert!(notes(&report).is_empty());
    }

    #[test]
    fn several_atlases_pair_by_stem_and_an_unmatched_skeleton_is_ambiguous() {
        let two_atlases = BundleBuilder::new()
            .text_asset("a.skel", SKELETON_JSON.as_bytes())
            .text_asset("a.atlas", atlas_text("a.png").as_bytes())
            .text_asset("b.atlas", atlas_text("b.png").as_bytes())
            .texture("a", 2, 2, &pixels())
            .texture("b", 2, 2, &pixels());
        let rigs = discover(&two_atlases).expect("a pairs with a.atlas by stem");
        assert_eq!(rigs[0].pages[0].0, "a.png");

        let unmatched = BundleBuilder::new()
            .text_asset("c.skel", SKELETON_JSON.as_bytes())
            .text_asset("a.atlas", atlas_text("a.png").as_bytes())
            .text_asset("b.atlas", atlas_text("b.png").as_bytes());
        let err = discover(&unmatched).unwrap_err();
        assert!(matches!(err, DecodeError::Ambiguous { .. }), "{err}");
    }

    #[test]
    fn two_textures_for_one_page_are_ambiguous() {
        let builder = complete_rig().texture("hero", 2, 2, &pixels());
        let err = discover(&builder).unwrap_err();
        assert!(matches!(err, DecodeError::Ambiguous { .. }), "{err}");
    }

    #[test]
    fn a_page_name_with_a_directory_and_an_unlisted_extension_still_finds_its_texture() {
        let builder = BundleBuilder::new()
            .text_asset("hero.skel", SKELETON_JSON.as_bytes())
            .text_asset("hero.atlas", atlas_text("images/hero.tga").as_bytes())
            .texture("hero", 2, 2, &pixels());
        let rigs = discover(&builder).expect("discover");
        assert_eq!(rigs[0].pages[0].0, "images/hero.tga");
        assert!(rigs[0].pages[0].1.is_some());
        assert_eq!(file_stem("a\\b/c.d.png"), "c.d");
        assert_eq!(file_stem(".hidden"), ".hidden");
    }

    #[test]
    fn a_page_with_a_folder_in_its_name_is_stored_flat_and_the_package_loads() {
        let builder = BundleBuilder::new()
            .text_asset("hero.skel", SKELETON_JSON.as_bytes())
            .text_asset("hero.atlas", atlas_text("images/hero.tga").as_bytes())
            .texture("hero", 2, 2, &pixels());
        let package = import_spine_bundle(&builder.build(), &mut LoadReport::new()).expect("import");
        assert_eq!(package.textures[0].file, "hero.png");
        assert_eq!(package.manifest.textures[0].file, "hero.png");
        let ir = package.model.as_spine().expect("spine");
        assert_eq!(ir.atlas.pages[0].name, "hero.png", "the atlas and the file agree, which is how a page is found");

        let dir = std::env::temp_dir().join(format!("a2d-flat-page-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        package.write_to(&dir).expect("a flat name writes");
        let back = Package::read_from(&dir).expect("reads back");
        assert_eq!(back.textures.len(), 1);
        assert!(back.validate().is_empty(), "{:?}", back.validate());
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn flat_page_names_keep_plain_names_and_never_collide() {
        assert_eq!(flat_page_names(&["a.png", "b2.png"]), ["a.png", "b2.png"]);
        assert_eq!(
            flat_page_names(&["x/hero.png", "y/hero.png", "hero.png"]),
            ["hero.png", "hero_2.png", "hero_3.png"]
        );
        assert_eq!(flat_page_names(&["..", "a:b.png"]), ["page.png", "a_b.png"]);
        // Case-only differences are collisions, and Windows-unsafe names are not plain.
        assert_eq!(flat_page_names(&["Hero.png", "hero.png"]), ["Hero.png", "hero_2.png"]);
        assert_eq!(flat_page_names(&["a?b.png", "x*y<z>.png", "q|r\"s.png"]), ["a_b.png", "x_y_z.png", "q_r_s.png"]);
        assert_eq!(flat_page_names(&["page.", "dir/name. "]), ["page.png", "name.png"]);
        assert_eq!(
            flat_page_names(&["CON.png", "nul", "com1.tga", "LPT9.png"]),
            ["page_CON.png", "page_nul.png", "page_com1.png", "page_LPT9.png"]
        );
    }

    #[test]
    fn an_unreadable_text_asset_is_noted_when_a_skeleton_is_found() {
        let builder = complete_rig().broken_text_asset("damaged");
        assert!(has_spine_rig(&builder.build()).expect("scan"));
        let mut report = LoadReport::new();
        inspect_spine_bundle(&builder.build(), &mut report).expect("inspect");
        assert!(notes(&report).join("\n").contains("could not be read"), "{:?}", notes(&report));
    }

    #[test]
    fn an_unreadable_text_asset_with_no_skeleton_is_not_read_as_no_spine_rig() {
        let builder = BundleBuilder::new().broken_text_asset("damaged").text_asset("readme", b"hello");
        let err = has_spine_rig(&builder.build()).unwrap_err();
        assert!(err.to_string().contains("could not be read"), "{err}");
    }
    #[test]
    fn pages_that_differ_only_by_case_each_find_their_own_texture() {
        let atlas = format!("{}{}", atlas_text("Hero.png"), atlas_text("hero.png"));
        let red = [255, 0, 0, 255].repeat(4);
        let blue = [0, 0, 255, 255].repeat(4);
        let builder = BundleBuilder::new()
            .text_asset("hero.skel", SKELETON_JSON.as_bytes())
            .text_asset("hero.atlas", atlas.as_bytes())
            .texture("Hero", 2, 2, &red)
            .texture("hero", 2, 2, &blue);
        let rigs = discover(&builder).expect("exact matches come before the case-blind tier");
        assert_eq!(rigs[0].pages.len(), 2);
        let first = rigs[0].pages[0].1.as_ref().expect("Hero.png");
        let second = rigs[0].pages[1].1.as_ref().expect("hero.png");
        assert_eq!(&first.rgba[..4], &[255, 0, 0, 255]);
        assert_eq!(&second.rgba[..4], &[0, 0, 255, 255]);

        // Through a package on disk each page keeps its own pixels: the two
        // names differ only by case, which one file system would merge.
        let package = import_spine_bundle(&builder.build(), &mut LoadReport::new()).expect("import");
        let dir = std::env::temp_dir().join(format!("a2d-case-pages-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        package.write_to(&dir).expect("distinct flat names write");
        let back = Package::read_from(&dir).expect("read");
        let _ = std::fs::remove_dir_all(&dir);
        assert_eq!(back.textures.len(), 2);
        let first_pixel = |file: &str| {
            let bytes = &back.textures.iter().find(|t| t.file == file).expect("page present").bytes;
            let mut reader = png::Decoder::new(std::io::Cursor::new(bytes)).read_info().expect("png");
            let mut buf = vec![0; reader.output_buffer_size().expect("size")];
            reader.next_frame(&mut buf).expect("frame");
            buf[..4].to_vec()
        };
        let pages: Vec<String> = back.manifest.textures.iter().map(|t| t.file.clone()).collect();
        assert_eq!(pages, ["Hero.png", "hero_2.png"]);
        assert_eq!(first_pixel("Hero.png"), [255, 0, 0, 255], "red stays red");
        assert_eq!(first_pixel("hero_2.png"), [0, 0, 255, 255]);
    }

    #[test]
    fn a_page_falls_back_to_a_case_blind_match_only_when_nothing_matches_exactly() {
        let builder = BundleBuilder::new()
            .text_asset("hero.skel", SKELETON_JSON.as_bytes())
            .text_asset("hero.atlas", atlas_text("hero.png").as_bytes())
            .texture("HERO", 2, 2, &pixels());
        assert!(discover(&builder).expect("discover")[0].pages[0].1.is_some());
    }

    #[test]
    fn an_unrelated_texture_that_cannot_be_decoded_does_not_fail_the_import() {
        let builder = complete_rig().texture_with_format("unrelated", 2, 2, 999, &[0; 16]);
        let mut report = LoadReport::new();
        let package = import_spine_bundle(&builder.build(), &mut report).expect("import");
        assert_eq!(package.textures.len(), 1);
        assert!(package.manifest.import_warnings.is_empty(), "{:?}", package.manifest.import_warnings);
    }

    #[test]
    fn a_page_whose_texture_cannot_be_decoded_is_a_degradation_naming_page_and_reason() {
        let builder = BundleBuilder::new()
            .text_asset("hero.skel", SKELETON_JSON.as_bytes())
            .text_asset("hero.atlas", atlas_text("hero.png").as_bytes())
            .texture_with_format("hero", 2, 2, 999, &[0; 16]);
        let mut report = LoadReport::new();
        let package = import_spine_bundle(&builder.build(), &mut report).expect("the rig still imports");
        assert!(package.textures.is_empty());
        let text = package.manifest.import_warnings.join("\n");
        assert!(text.contains("hero.png") && text.contains("could not be decoded") && text.contains("999"), "{text}");
        assert!(text.contains("texture page"), "the page is also reported missing: {text}");
    }

    #[test]
    fn an_atlas_without_page_sizes_takes_them_from_the_textures() {
        let without_size = atlas_text("hero.png").replace("size: 2,2\n", "");
        assert!(!without_size.contains("size: 2,2"));
        let builder = BundleBuilder::new()
            .text_asset("hero.skel", SKELETON_JSON.as_bytes())
            .text_asset("hero.atlas", without_size.as_bytes())
            .texture("hero", 2, 2, &pixels());
        let mut report = LoadReport::new();
        let package = import_spine_bundle(&builder.build(), &mut report).expect("import");
        assert_eq!(package.manifest.textures[0].size, Some([2, 2]));

        // And with no texture to measure, the gap is reported, not guessed.
        let builder = BundleBuilder::new()
            .text_asset("hero.skel", SKELETON_JSON.as_bytes())
            .text_asset("hero.atlas", without_size.as_bytes());
        let package = import_spine_bundle(&builder.build(), &mut LoadReport::new()).expect("import");
        assert!(
            package.manifest.import_warnings.join("\n").contains("page size"),
            "{:?}",
            package.manifest.import_warnings
        );
    }

    #[test]
    fn inspection_lists_every_rig_with_its_paired_atlas() {
        let builder = complete_rig().text_asset("zed.skel", SKELETON_JSON.as_bytes());
        let inventory = inspect_spine_bundle(&builder.build(), &mut LoadReport::new()).expect("inspect");
        assert_eq!(inventory.rigs.len(), 2);
        assert!(inventory.rigs.iter().all(|r| r.atlas_name.as_deref() == Some("hero.atlas")));
        assert_eq!(inventory.textures.len(), 1);
    }

    #[test]
    fn classification_is_by_content_and_not_by_name() {
        // A skeleton under a name that says nothing, and an atlas likewise.
        let builder = BundleBuilder::new()
            .text_asset("blob1", SKELETON_JSON.as_bytes())
            .text_asset("blob2", atlas_text("blob1.png").as_bytes())
            .texture("blob1", 2, 2, &pixels());
        let rigs = discover(&builder).expect("discover");
        assert_eq!(rigs[0].name, "blob1");
        assert!(rigs[0].atlas.is_some());
    }
}
