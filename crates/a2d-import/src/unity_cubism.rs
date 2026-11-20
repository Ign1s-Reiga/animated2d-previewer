//! Finding a Cubism model inside a Unity AssetBundle.
//!
//! This is discovery, not decoding: it says what is in the bundle and where,
//! and hands back the payloads. Turning MOC3 bytes into a model belongs to
//! `a2d-cubism`, and nothing downstream of here learns that the assets came
//! out of Unity at all (spec §2).
//!
//! # What a Cubism model looks like once Unity has packed it
//!
//! The editor's `.model3.json` is gone. What survives, verified against a real
//! 2022.3 bundle, is a set of `MonoBehaviour`s whose C# classes name their
//! roles:
//!
//! * `CubismMoc` — a `byte[]` holding the untouched `.moc3` payload.
//! * `CubismModel`, `CubismParameter`, `CubismPart`, `CubismDrawable` — the
//!   model split across one `GameObject` per element. The drawable and
//!   parameter names live on those objects, not in the MOC3 wrapper.
//! * `CubismFadeMotionData` — one per motion. It keeps the path of the
//!   `.motion3.json` it was built from and the parameter curves themselves, so
//!   it is what `import_bundle` recovers motions from (see `fade_motion`).
//! * `CubismFadeMotionList` — the index tying those to their clips.
//!
//! The same motions also exist as Unity `AnimationClip`s, which are not decoded
//! (the fade data carries the curves in a readable form); textures become
//! `Texture2D`. The bundle's own container table maps each of these back to the
//! path it was authored under.

mod fade_motion;

use a2d_core::ir::cubism::Motion;
use a2d_core::{DecodeError, Degradation, LoadReport};
use a2d_pack::manifest::TextureEntry as PackTextureEntry;
use a2d_pack::package::TextureFile;
use a2d_pack::Package;
use a2d_unity::{behaviour_body, Bundle, ClassId, Inventory, ObjectInfo, Reader, SerializedFile};

/// The `.moc3` payload plus how it was found.
#[derive(Debug, Clone)]
pub struct MocPayload {
    /// Name of the `CubismMoc` object, which is normally the character's.
    pub name: String,
    /// Path the asset was authored under, when the bundle still records it.
    pub asset_path: Option<String>,
    /// Format version from the MOC3 header.
    pub version: u8,
    pub bytes: Vec<u8>,
}

/// One motion, as Unity left it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct MotionEntry {
    pub name: String,
    pub asset_path: Option<String>,
    /// Whether a matching `CubismFadeMotionData` was found beside the clip.
    pub has_fade_data: bool,
}

/// A texture page.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TextureEntry {
    pub name: String,
    pub asset_path: Option<String>,
}

/// Everything found in one bundle.
#[derive(Debug, Clone)]
pub struct CubismInventory {
    /// The exact player revision the bundle was built with.
    pub unity_revision: String,
    /// Total objects in the serialized file.
    pub object_count: usize,
    pub moc: Option<MocPayload>,
    pub textures: Vec<TextureEntry>,
    pub motions: Vec<MotionEntry>,
    /// Names of `AnimatorController` objects, which sequence the motions.
    pub animator_controllers: Vec<String>,
    /// `GameObject` count, which is the size of the model's element hierarchy.
    pub game_objects: usize,
    pub parameters: usize,
    pub parts: usize,
    pub drawables: usize,
    /// Fade motion entries that name their original `.motion3.json`.
    pub fade_sources: Vec<String>,
}

impl CubismInventory {
    /// Whether enough was found to be worth reconstructing.
    pub fn is_cubism(&self) -> bool {
        self.moc.is_some()
    }
}

/// The MOC3 magic and the version byte that follows it.
const MOC3_MAGIC: &[u8; 4] = b"MOC3";

/// Reads a bundle and reports the Cubism assets in it.
pub fn inspect_bundle(bytes: &[u8], report: &mut LoadReport) -> Result<CubismInventory, DecodeError> {
    let bundle = Bundle::parse(bytes)?;
    let node = bundle.nodes.iter().find(|n| n.is_serialized()).ok_or_else(|| DecodeError::Reconstruction {
        game: "unity_cubism".into(),
        message: format!(
            "the bundle holds no serialized file, only {:?}",
            bundle.nodes.iter().map(|n| &n.path).collect::<Vec<_>>()
        ),
    })?;
    let file = SerializedFile::parse(bundle.node_data(node)?)?;
    let inventory = Inventory::build(&file);

    let moc = read_moc(&file, &inventory, report)?;

    let textures = inventory
        .by_class(ClassId::TEXTURE_2D)
        .map(|o| TextureEntry {
            name: o.name.clone().unwrap_or_else(|| format!("texture#{}", o.path_id)),
            asset_path: o.asset_path.clone(),
        })
        .collect();

    // Fade data names the `.motion3.json` a clip came from; matching them lets
    // the report say which motions kept their provenance.
    let fade_sources: Vec<String> =
        inventory.by_script("CubismFadeMotionData").filter_map(|o| motion3_path(&file, o)).collect();

    let motions = inventory
        .by_class(ClassId::ANIMATION_CLIP)
        .map(|o| {
            let name = o.name.clone().unwrap_or_else(|| format!("clip#{}", o.path_id));
            let has_fade_data = fade_sources.iter().any(|p| mentions(p, &name));
            MotionEntry { name, asset_path: o.asset_path.clone(), has_fade_data }
        })
        .collect();

    let animator_controllers = inventory
        .by_class(ClassId::ANIMATOR_CONTROLLER)
        .map(|o| o.name.clone().unwrap_or_else(|| format!("controller#{}", o.path_id)))
        .collect();

    let out = CubismInventory {
        unity_revision: bundle.unity_revision.clone(),
        object_count: inventory.objects.len(),
        moc,
        textures,
        motions,
        animator_controllers,
        game_objects: inventory.by_class(ClassId::GAME_OBJECT).count(),
        parameters: inventory.by_script("CubismParameter").count(),
        parts: inventory.by_script("CubismPart").count(),
        drawables: inventory.by_script("CubismDrawable").count(),
        fade_sources,
    };

    if out.moc.is_none() {
        report.warn(Degradation::MissingReference { kind: "CubismMoc".into(), name: "the model payload".into() });
    }
    if out.textures.is_empty() {
        report.warn(Degradation::MissingReference { kind: "texture".into(), name: "any Texture2D".into() });
    }
    Ok(out)
}

/// Pulls the `.moc3` bytes out of the `CubismMoc` behaviour.
///
/// The class is a `ScriptableObject` with one `byte[]` field, so after the
/// standard behaviour header and its name comes a length and then the payload,
/// untouched from the editor.
fn read_moc(
    file: &SerializedFile,
    inventory: &Inventory,
    report: &mut LoadReport,
) -> Result<Option<MocPayload>, DecodeError> {
    let mut found = inventory.by_script("CubismMoc");
    let Some(info) = found.next() else { return Ok(None) };
    if found.next().is_some() {
        // Two models in one bundle is not something this understands; saying so
        // beats picking one and being quietly wrong.
        report.warn(Degradation::Note("the bundle holds more than one CubismMoc; only the first is read".into()));
    }

    let object = file
        .objects
        .iter()
        .find(|o| o.path_id == info.path_id)
        .ok_or_else(|| DecodeError::corrupt("the CubismMoc object vanished between passes".to_string()))?;
    let data = file.object_data(object)?;

    // Header: PPtr m_GameObject (12) + m_Enabled (1, aligned to 4) + PPtr
    // m_Script (12) + m_Name (length-prefixed, padded) + the array length.
    let name_len_at = 12 + 4 + 12;
    let name_len = read_i32(data, name_len_at)?;
    if !(0..=4096).contains(&name_len) {
        return Err(DecodeError::corrupt(format!("the CubismMoc name declares {name_len} bytes")));
    }
    let after_name = name_len_at + 4 + name_len as usize;
    let payload_len_at = (after_name + 3) & !3; // the string is padded to four
    let payload_len = read_i32(data, payload_len_at)?;
    if payload_len <= 0 {
        return Err(DecodeError::corrupt(format!("the CubismMoc payload declares {payload_len} bytes")));
    }
    let start = payload_len_at + 4;
    let end = start.checked_add(payload_len as usize).filter(|e| *e <= data.len()).ok_or_else(|| {
        DecodeError::corrupt(format!(
            "the CubismMoc payload claims {payload_len} bytes at {start}, past the {} the object holds",
            data.len()
        ))
    })?;
    let bytes = data[start..end].to_vec();

    if bytes.len() < 8 || &bytes[..4] != MOC3_MAGIC {
        return Err(DecodeError::Reconstruction {
            game: "unity_cubism".into(),
            message: format!(
                "the CubismMoc payload does not start with the MOC3 magic (found {:02x?}); \
                 the field layout of this Cubism SDK build is not the one read here",
                &bytes[..bytes.len().min(4)]
            ),
        });
    }
    let version = bytes[4];

    Ok(Some(MocPayload {
        name: info.name.clone().unwrap_or_else(|| "model".into()),
        asset_path: info.asset_path.clone(),
        version,
        bytes,
    }))
}

/// The `.motion3.json` path a `CubismFadeMotionData` records: its first field
/// after the behaviour's own name.
fn motion3_path(file: &SerializedFile, info: &ObjectInfo) -> Option<String> {
    let object = file.objects.iter().find(|o| o.path_id == info.path_id)?;
    let (mut body, _) = behaviour_body(file, object).ok()?;
    let text = body.string().ok()?;
    text.contains(".motion3.json").then_some(text)
}

/// Whether a source path names a given clip, ignoring case.
///
/// Unity lower-cases container paths but keeps the authored casing inside the
/// asset, so `Hero_idle` has to match `.../hero_idle.motion3.json`.
fn mentions(path: &str, clip: &str) -> bool {
    let stem = clip.to_ascii_lowercase();
    path.to_ascii_lowercase().contains(&stem)
}

fn read_i32(data: &[u8], at: usize) -> Result<i32, DecodeError> {
    let slice = data
        .get(at..at + 4)
        .ok_or_else(|| DecodeError::corrupt(format!("wanted four bytes at {at}, the object holds {}", data.len())))?;
    let b: [u8; 4] = slice.try_into().unwrap_or([0; 4]);
    Ok(i32::from_le_bytes(b))
}

/// Reconstructs a loadable package from a bundle.
///
/// This is the whole of spec §8's "asset discovery and reconstruction" for
/// `unity_cubism`: find the model, decode it, decode its texture pages, and
/// write both into the shape the viewer loads. Nothing downstream learns that
/// any of it came out of Unity.
///
/// Motions are recovered from the `CubismFadeMotionData` behaviours, which keep
/// each motion's parameter curves; the matching Unity `AnimationClip`s are not
/// decoded. A motion that cannot be decoded, or a clip with no fade data, is
/// reported through `report` and the model still loads.
pub fn import_bundle(bytes: &[u8], report: &mut LoadReport) -> Result<Package, DecodeError> {
    let inventory = inspect_bundle(bytes, report)?;
    let moc = inventory.moc.ok_or_else(|| DecodeError::Reconstruction {
        game: "unity_cubism".into(),
        message: "the bundle holds no CubismMoc, so there is no model to reconstruct".into(),
    })?;

    let parsed = a2d_cubism::Moc3::parse(&moc.bytes)?;
    let source_format = format!("cubism-moc3-v{}", parsed.version);
    if !parsed.is_verified_version() {
        report.warn(Degradation::Note(format!(
            "{source_format} is newer than any layout checked against a real model;              it is read on the same layout rather than refused"
        )));
    }
    let mut model: a2d_core::ir::cubism::CubismIr = parsed.into();

    let bundle = Bundle::parse(bytes)?;
    let node = bundle.nodes.iter().find(|n| n.is_serialized()).ok_or_else(|| DecodeError::Reconstruction {
        game: "unity_cubism".into(),
        message: "the bundle holds no serialized file".into(),
    })?;
    let file = SerializedFile::parse(bundle.node_data(node)?)?;

    model.motions = recover_motions(&file, &Inventory::build(&file), report);
    // Curves aimed at ids the model lacks (model-level opacity, say) are kept
    // and playback skips them; saying so here keeps that visible at import.
    for degradation in model.unresolved_motion_targets() {
        report.warn(degradation);
    }
    let unmatched: Vec<&str> = inventory.motions.iter().filter(|m| !m.has_fade_data).map(|m| m.name.as_str()).collect();
    if !unmatched.is_empty() {
        report.note(format!(
            "{} AnimationClip(s) have no matching fade motion data and are not recovered: {}",
            unmatched.len(),
            unmatched.join(", ")
        ));
    }

    let mut package = Package::from_cubism(model, moc.name.clone(), source_format);
    // Informational only: the importer is the last layer allowed to know this,
    // and nothing downstream branches on it (spec §2).
    package.manifest.source_game = crate::games::Importer::UnityCubism.as_str().to_string();

    // Texture pages. Unity stores these block-compressed, so they are decoded
    // and re-encoded as PNG: a package holds pages the viewer can read without
    // knowing anything about Unity's formats.
    for (index, texture) in a2d_unity::read_textures(&file)?.into_iter().enumerate() {
        let name = format!("{}.png", sanitize(&texture.name, index));
        let png = encode_png(texture.width, texture.height, &texture.rgba, &name)?;
        package.manifest.textures.push(PackTextureEntry {
            file: name.clone(),
            size: Some([texture.width, texture.height]),
            // Cubism atlases are straight alpha, not premultiplied.
            premultiplied_alpha: false,
        });
        package.textures.push(TextureFile { file: name, bytes: png });
    }
    if package.textures.is_empty() {
        report.warn(Degradation::MissingReference { kind: "texture".into(), name: "any Texture2D".into() });
    }

    record_warnings(&mut package, report);
    Ok(package)
}

/// Copies the import report into the manifest, so `preview` and `validate`
/// can show it after the bundle is gone.
///
/// Unresolved motion targets are left out: they are re-derived from the IR
/// every time the package is opened and validated, so persisting them as well
/// would print each one twice. Order is the report's insertion order, which is
/// deterministic for a given bundle.
fn record_warnings(package: &mut Package, report: &LoadReport) {
    let rederived = match &package.model {
        a2d_pack::PackageModel::Cubism(ir) => ir.unresolved_motion_targets(),
        _ => Vec::new(),
    };
    package.manifest.import_warnings =
        report.warnings().iter().filter(|w| !rederived.contains(w)).map(|w| w.to_string()).collect();
}

/// Decodes every `CubismFadeMotionData` into a motion.
///
/// One that fails is reported by name and left out; the others are unaffected.
/// The result is sorted by name, with duplicate names made unique, so the
/// package serializes the same way on every run.
fn recover_motions(file: &SerializedFile, inventory: &Inventory, report: &mut LoadReport) -> Vec<Motion> {
    let items = inventory.by_script("CubismFadeMotionData").map(|info| {
        let label = info.name.clone().unwrap_or_else(|| format!("fade#{}", info.path_id));
        let body = file
            .objects
            .iter()
            .find(|o| o.path_id == info.path_id)
            .ok_or_else(|| DecodeError::corrupt("the fade motion object vanished between passes".to_string()))
            .and_then(|object| behaviour_body(file, object));
        (label, body)
    });
    motions_from_bodies(items, report)
}

/// The part of motion recovery that needs no bundle: each item is a label and
/// either a body reader with its behaviour name, or the error that kept it
/// from being read.
fn motions_from_bodies<'a>(
    items: impl IntoIterator<Item = (String, Result<(Reader<'a>, String), DecodeError>)>,
    report: &mut LoadReport,
) -> Vec<Motion> {
    let mut decoded = Vec::new();
    for (label, body) in items {
        let result = body.and_then(|(body, name)| Ok((name, fade_motion::decode(&body)?)));
        match result {
            Ok((behaviour_name, fade)) => {
                let stem = fade_motion::motion_stem(&fade.motion_name, &behaviour_name);
                decoded.push((stem, fade));
            }
            Err(error) => report.note(format!("motion {label:?} was not recovered: {error}")),
        }
    }
    decoded.sort_by(|a, b| (&a.0, &a.1.motion_name).cmp(&(&b.0, &b.1.motion_name)));

    let mut names: Vec<String> = decoded.iter().map(|(stem, _)| stem.clone()).collect();
    fade_motion::dedupe_names(&mut names, report);

    let mut motions: Vec<Motion> =
        decoded.iter().zip(names).map(|((_, fade), name)| fade_motion::to_motion(fade, name, report)).collect();
    motions.sort_by(|a, b| a.name.cmp(&b.name));
    motions
}

/// A file name that is safe to write and stable across runs.
///
/// Unity object names are arbitrary strings; a package's file names end up on
/// disk, so anything outside a conservative set is replaced rather than
/// escaped. An empty result falls back to the page's index so two pages never
/// collide on one name.
fn sanitize(name: &str, index: usize) -> String {
    let cleaned: String =
        name.chars().map(|c| if c.is_ascii_alphanumeric() || c == '_' || c == '-' { c } else { '_' }).collect();
    let trimmed = cleaned.trim_matches('_');
    if trimmed.is_empty() {
        format!("texture_{index:02}")
    } else {
        trimmed.to_string()
    }
}

/// Encodes decoded pixels as a PNG.
fn encode_png(width: u32, height: u32, rgba: &[u8], label: &str) -> Result<Vec<u8>, DecodeError> {
    let expected = width as usize * height as usize * 4;
    if rgba.len() != expected {
        return Err(DecodeError::corrupt(format!(
            "texture {label} is {width}x{height} but carries {} bytes, not {expected}",
            rgba.len()
        )));
    }
    let mut out = Vec::new();
    {
        let mut encoder = png::Encoder::new(&mut out, width, height);
        encoder.set_color(png::ColorType::Rgba);
        encoder.set_depth(png::BitDepth::Eight);
        let mut writer = encoder
            .write_header()
            .map_err(|e| DecodeError::corrupt(format!("could not start PNG for {label}: {e}")))?;
        writer
            .write_image_data(rgba)
            .map_err(|e| DecodeError::corrupt(format!("could not encode PNG for {label}: {e}")))?;
    }
    Ok(out)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_clip_is_matched_to_its_source_regardless_of_case() {
        let path = "Assets/.../motions/Hero_idle.motion3.json";
        assert!(mentions(path, "Hero_idle"));
        assert!(mentions(path, "hero_idle"), "container paths are lower-cased");
        assert!(!mentions(path, "Hero_smile"));
    }

    #[test]
    fn reading_past_an_object_is_an_error_and_never_a_panic() {
        assert!(read_i32(&[1, 2], 0).is_err());
        assert!(read_i32(&[1, 2, 3, 4], 2).is_err());
        assert_eq!(read_i32(&[1, 0, 0, 0], 0).unwrap(), 1);
    }

    fn cubism_package(motions: Vec<a2d_core::ir::cubism::Motion>) -> Package {
        let ir = a2d_core::ir::cubism::CubismIr { motions, ..Default::default() };
        Package::from_cubism(ir, "m", "cubism-moc3-v3")
    }

    #[test]
    fn report_warnings_are_recorded_in_the_manifest_in_order() {
        let mut package = cubism_package(Vec::new());
        let mut report = LoadReport::new();
        report.note("2 AnimationClip(s) have no matching fade motion data");
        report.warn(Degradation::MissingReference { kind: "texture".into(), name: "any Texture2D".into() });
        record_warnings(&mut package, &report);
        let expected: Vec<String> = report.warnings().iter().map(|w| w.to_string()).collect();
        assert_eq!(package.manifest.import_warnings, expected);
        assert_eq!(expected.len(), 2);
    }

    #[test]
    fn unresolved_motion_targets_are_not_persisted_because_they_are_rederived() {
        use a2d_core::ir::cubism::{Motion, MotionCurve};
        let motion = Motion {
            name: "Idle".into(),
            duration: 1.0,
            curves: vec![MotionCurve { target: "ParamNoSuchThing".into(), ..MotionCurve::default() }],
            ..Motion::default()
        };
        let mut package = cubism_package(vec![motion]);
        let a2d_pack::PackageModel::Cubism(ir) = &package.model else { panic!("cubism package expected") };
        let mut report = LoadReport::new();
        for d in ir.unresolved_motion_targets() {
            report.warn(d);
        }
        report.note("kept");
        assert_eq!(report.len(), 2);
        record_warnings(&mut package, &report);
        assert_eq!(package.manifest.import_warnings, vec!["kept".to_string()]);
    }

    #[test]
    fn a_bundle_that_is_not_one_is_refused() {
        let mut report = LoadReport::new();
        assert!(inspect_bundle(b"not a bundle at all", &mut report).is_err());
    }

    #[test]
    fn an_inventory_without_a_moc_is_not_cubism() {
        let empty = CubismInventory {
            unity_revision: "2022.3".into(),
            object_count: 0,
            moc: None,
            textures: Vec::new(),
            motions: Vec::new(),
            animator_controllers: Vec::new(),
            game_objects: 0,
            parameters: 0,
            parts: 0,
            drawables: 0,
            fade_sources: Vec::new(),
        };
        assert!(!empty.is_cubism());
    }
}
