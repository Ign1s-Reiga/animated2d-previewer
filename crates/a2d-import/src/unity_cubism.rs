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
//! * `CubismPhysicsController` — the physics rig, as serialized fields (see
//!   `physics`). There is no `.physics3.json` in a bundle.
//!
//! The same motions also exist as Unity `AnimationClip`s. Shipping bundles often
//! hold *only* those, compressed, with no fade data at all; `import_bundle`
//! decodes such a clip itself (see `clip_motion`) and uses the fade data when
//! there is some. Textures become `Texture2D`. The bundle's own container table
//! maps each of these back to the path it was authored under.

mod clip_motion;
mod fade_motion;
mod physics;

use std::collections::HashSet;

use a2d_core::ir::cubism::{Motion, Physics};
use a2d_core::{DecodeError, Degradation, LoadReport};
use a2d_pack::manifest::TextureEntry as PackTextureEntry;
use a2d_pack::package::TextureFile;
use a2d_pack::Package;
use a2d_unity::{
    behaviour_body, behaviour_owner_name, AnimationClip, Bundle, ClassId, Inventory, ObjectInfo, PPtr, Reader,
    SerializedFile,
};

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
    /// When it was, the motion is recovered from that; otherwise from the
    /// clip's own compressed curves.
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
    /// The physics rig recovered from the model's `CubismPhysicsController`.
    /// `None` when the bundle has none or it could not be recovered; the
    /// report says which.
    pub physics: Option<Physics>,
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
            let has_fade_data = fade_sources.iter().any(|p| names_clip(p, &name));
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
        physics: recover_physics(&file, &inventory, report),
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

/// The lower-cased file stem of a motion source path, which is what a clip is
/// matched on: `Assets/x/Hero_idle.motion3.json` is `hero_idle`.
fn source_stem(path: &str) -> String {
    fade_motion::motion_stem(path, "").to_ascii_lowercase()
}

/// Whether a source path names a given clip: an exact, case-insensitive match
/// of file stems. Unity lower-cases container paths but keeps the authored
/// casing inside the asset, so `Hero_idle` matches `.../hero_idle.motion3.json`
/// while `idle` does not match `idle_02.motion3.json`.
fn names_clip(path: &str, clip: &str) -> bool {
    source_stem(path) == clip.to_ascii_lowercase()
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
/// each motion's parameter curves, and, for every `AnimationClip` with no
/// matching fade data, from the clip itself. A motion that cannot be decoded is
/// reported through `report` and the model still loads.
pub fn import_bundle(bytes: &[u8], report: &mut LoadReport) -> Result<Package, DecodeError> {
    let inventory = inspect_bundle(bytes, report)?;
    let physics = inventory.physics;
    let moc = inventory.moc.ok_or_else(|| DecodeError::Reconstruction {
        game: "unity_cubism".into(),
        message: "the bundle holds no CubismMoc, so there is no model to reconstruct".into(),
    })?;

    let parsed = a2d_cubism::Moc3::parse(&moc.bytes)?;
    let source_format = format!("cubism-moc3-v{}", parsed.version);
    if !parsed.is_verified_version() {
        report.warn(Degradation::Note(format!(
            "{source_format} is newer than any layout checked against a real model; it is read on the same layout rather than refused"
        )));
    }
    for note in &parsed.notes {
        report.warn(Degradation::Note(note.clone()));
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
    // Recovered once, while inspecting, so its notes are not reported twice.
    model.physics = physics;
    for degradation in model.unresolved_physics_targets() {
        report.warn(degradation);
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
/// Unresolved motion and physics targets are left out: they are re-derived
/// from the IR every time the package is opened and validated, so persisting
/// them as well would print each one twice. Order is the report's insertion order, which is
/// deterministic for a given bundle.
fn record_warnings(package: &mut Package, report: &LoadReport) {
    let rederived = match &package.model {
        a2d_pack::PackageModel::Cubism(ir) => {
            let mut all = ir.unresolved_motion_targets();
            all.extend(ir.unresolved_physics_targets());
            all
        }
        _ => Vec::new(),
    };
    package.manifest.import_warnings =
        report.warnings().iter().filter(|w| !rederived.contains(w)).map(|w| w.to_string()).collect();
}

/// Recovers the physics rig from the model's `CubismPhysicsController`.
///
/// A bundle with no controller simply has no physics and says nothing. Every
/// other way of coming up empty is reported: a body that fits no known layout,
/// a controller the source had switched off, a second controller (the first is
/// used), and whatever conversion had to drop.
///
/// A disabled controller (`m_Enabled` is 0) is not recovered: the source ran
/// with no physics, and a viewer that swayed hair the original never swayed
/// would be showing something else. The body is still decoded first, so a
/// layout change is reported either way.
fn recover_physics(file: &SerializedFile, inventory: &Inventory, report: &mut LoadReport) -> Option<Physics> {
    let mut controllers = inventory.by_script("CubismPhysicsController");
    let info = controllers.next()?;
    if controllers.next().is_some() {
        report.note("the bundle holds more than one CubismPhysicsController; only the first is read");
    }
    let label = info.name.clone().filter(|n| !n.is_empty()).unwrap_or_else(|| format!("physics#{}", info.path_id));
    let Some(object) = file.find_object(info.path_id) else {
        report.note(format!("physics controller {label:?} vanished between passes; physics is not recovered"));
        return None;
    };
    let raw = match behaviour_body(file, object).and_then(|(body, _)| physics::decode(&body)) {
        Ok(raw) => raw,
        Err(error) => {
            report.note(format!("physics controller {label:?} was not recovered: {error}"));
            return None;
        }
    };
    // `m_Enabled` is the byte right after the owner's `PPtr` (12 bytes).
    let enabled = file.object_data(object).ok().and_then(|data| data.get(12).copied());
    physics_if_enabled(&raw, enabled, &label, report)
}

/// The enabled-switch decision of [`recover_physics`], apart from the bundle.
fn physics_if_enabled(
    raw: &physics::RawRig,
    enabled: Option<u8>,
    label: &str,
    report: &mut LoadReport,
) -> Option<Physics> {
    if enabled == Some(0) {
        report.note(format!(
            "physics controller {label:?} is disabled in the source (m_Enabled is 0), which ran without physics; \
             it is not recovered"
        ));
        return None;
    }
    physics::to_physics(raw, report)
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
    let (mut motions, covered) = motions_from_bodies(items, report);
    let from_clips = clip_motions(file, inventory, &covered, report);
    merge_motions(&mut motions, from_clips, report);
    motions
}

/// Adds clip-derived motions to the fade-derived ones and puts the whole list
/// in its final, deterministic order with unique names.
///
/// The stable sort keeps bundle order between equal names, so the result is
/// the same on every run.
fn merge_motions(motions: &mut Vec<Motion>, from_clips: Vec<Motion>, report: &mut LoadReport) {
    if from_clips.is_empty() {
        return;
    }
    motions.extend(from_clips);
    motions.sort_by(|a, b| a.name.cmp(&b.name));
    let mut names: Vec<String> = motions.iter().map(|m| m.name.clone()).collect();
    fade_motion::dedupe_names(&mut names, report);
    for (motion, name) in motions.iter_mut().zip(names) {
        motion.name = name;
    }
}

/// Whether a clip's own name is among the lower-cased stems of fade motions
/// that decoded.
fn is_covered(covered: &HashSet<String>, clip: &str) -> bool {
    covered.contains(&clip.to_ascii_lowercase())
}

/// Decodes every `AnimationClip` that has no matching fade data.
///
/// Parameter names come from the `GameObject` that carries each
/// `CubismParameter`, because a clip names its targets only by hash. A clip that
/// fails is reported by name and the others are unaffected.
fn clip_motions(
    file: &SerializedFile,
    inventory: &Inventory,
    covered: &HashSet<String>,
    report: &mut LoadReport,
) -> Vec<Motion> {
    let pending: Vec<&ObjectInfo> = inventory
        .by_class(ClassId::ANIMATION_CLIP)
        .filter(|o| {
            let name = o.name.clone().unwrap_or_else(|| format!("clip#{}", o.path_id));
            !is_covered(covered, &name)
        })
        .collect();
    if pending.is_empty() {
        return Vec::new();
    }

    let mut ids: Vec<String> = Vec::new();
    let mut unnamed = 0usize;
    for info in inventory.by_script("CubismParameter") {
        let owner = file
            .find_object(info.path_id)
            .ok_or_else(|| DecodeError::corrupt("a CubismParameter vanished between passes".to_string()))
            .and_then(|object| behaviour_owner_name(file, object));
        match owner {
            Ok(Some(name)) if !name.is_empty() => ids.push(name),
            _ => unnamed += 1,
        }
    }
    if unnamed > 0 {
        report.note(format!(
            "{unnamed} CubismParameter(s) have no readable owner name, so clip curves aimed at them cannot be matched"
        ));
    }
    let names = match clip_motion::NameTable::new(ids.iter().map(String::as_str)) {
        Ok(names) => names,
        Err(error) => {
            report.note(format!("{} AnimationClip(s) were not recovered: {error}", pending.len()));
            return Vec::new();
        }
    };
    let script_class =
        |p: PPtr| p.is_local().then(|| inventory.script(p.path_id)).flatten().map(|s| s.class_name.clone());

    let mut motions = Vec::new();
    for info in pending {
        let label = info.name.clone().unwrap_or_else(|| format!("clip#{}", info.path_id));
        let clip = file
            .find_object(info.path_id)
            .ok_or_else(|| DecodeError::corrupt("the AnimationClip vanished between passes".to_string()))
            .and_then(|object| AnimationClip::parse(file.object_data(object)?));
        match clip {
            Ok(clip) => motions.extend(clip_motion::to_motion(&clip, &names, &script_class, report)),
            Err(error) => report.note(format!("motion {label:?} was not recovered from its AnimationClip: {error}")),
        }
    }
    motions
}

/// The part of motion recovery that needs no bundle: each item is a label and
/// either a body reader with its behaviour name, or the error that kept it
/// from being read.
fn motions_from_bodies<'a>(
    items: impl IntoIterator<Item = (String, Result<(Reader<'a>, String), DecodeError>)>,
    report: &mut LoadReport,
) -> (Vec<Motion>, HashSet<String>) {
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
    // Only motions that actually decoded cover a clip, so a clip whose fade
    // data is unreadable falls back to its own curves.
    let covered = decoded.iter().map(|(stem, _)| stem.to_ascii_lowercase()).collect();
    (motions, covered)
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
        assert!(names_clip(path, "Hero_idle"));
        assert!(names_clip(path, "hero_idle"), "container paths are lower-cased");
        assert!(!names_clip(path, "Hero_smile"));
        assert!(!names_clip("x/idle_02.motion3.json", "idle"), "a prefix is not a match");
        assert!(!names_clip("x/my_idle.motion3.json", "idle"), "nor is a suffix");
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
        report.note("a note");
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
    fn clips_are_covered_by_decoded_fade_data_only_by_exact_stem_and_the_merge_is_deterministic() {
        use super::fade_motion::tests::{body, sample};
        use a2d_unity::Endian;

        let mk = |path: &str| fade_motion::FadeMotion { motion_name: path.into(), ..sample() };
        let bytes_hero = body(&mk("x/Hero.motion3.json"), true);
        let bytes_idle2 = body(&mk("x/Idle_02.motion3.json"), false);
        let junk = vec![9u8; 8];
        fn rd(b: &[u8]) -> Reader<'_> {
            Reader::new(b, Endian::Little)
        }
        let items = vec![
            ("hero".to_string(), Ok((rd(&bytes_hero), "hero".to_string()))),
            ("idle2".to_string(), Ok((rd(&bytes_idle2), "idle2".to_string()))),
            // Fade data that fails to decode must not hide its clip.
            ("broken".to_string(), Ok((rd(&junk), "broken".to_string()))),
        ];
        let mut report = LoadReport::new();
        let (mut motions, covered) = motions_from_bodies(items, &mut report);
        assert_eq!(motions.iter().map(|m| m.name.as_str()).collect::<Vec<_>>(), ["Hero", "Idle_02"]);

        // `idle` is a substring of `idle_02` but is not covered by it.
        let clips = ["hero", "idle", "idle_02", "broken", "idle"];
        let uncovered: Vec<&str> = clips.iter().copied().filter(|c| !is_covered(&covered, c)).collect();
        assert_eq!(uncovered, ["idle", "broken", "idle"]);

        let stub = |name: &str| Motion { name: name.into(), duration: 1.0, ..Motion::default() };
        let mut merge_report = LoadReport::new();
        merge_motions(&mut motions, uncovered.iter().map(|n| stub(n)).collect(), &mut merge_report);
        let names: Vec<&str> = motions.iter().map(|m| m.name.as_str()).collect();
        // Byte order puts the upper-case fade-derived names first; the clash
        // between the two `idle` clips is resolved by suffix.
        assert_eq!(names, ["Hero", "Idle_02", "broken", "idle", "idle_2"]);
        assert_eq!(merge_report.len(), 1);
        assert!(merge_report.to_string().contains("renamed"), "{merge_report}");
        let text = report.to_string();
        assert!(text.contains("\"broken\"") && text.contains("not recovered"), "{text}");
    }

    #[test]
    fn a_disabled_physics_controller_is_reported_and_not_recovered() {
        let rig = physics::tests::sample();
        let mut report = LoadReport::new();
        assert!(physics_if_enabled(&rig, Some(0), "ctl", &mut report).is_none());
        assert!(report.to_string().contains("disabled in the source"), "{report}");

        for enabled in [Some(1), None] {
            let mut report = LoadReport::new();
            assert!(physics_if_enabled(&rig, enabled, "ctl", &mut report).is_some());
            assert!(report.is_empty(), "{report}");
        }
    }

    #[test]
    fn recovered_physics_packs_and_its_unresolved_ids_are_rederived_not_persisted() {
        let rig = physics::tests::sample();
        let mut report = LoadReport::new();
        let physics = physics::to_physics(&rig, &mut report).unwrap();
        let ir = a2d_core::ir::cubism::CubismIr {
            parameters: vec![a2d_core::ir::cubism::Parameter {
                id: "ParamAngleX".into(),
                minimum: -30.0,
                maximum: 30.0,
                default: 0.0,
            }],
            physics: Some(physics),
            ..Default::default()
        };
        // Three ids name nothing: ParamAngleZ, ParamHairA, ParamHairB.
        let unresolved = ir.unresolved_physics_targets();
        assert_eq!(unresolved.len(), 3, "{unresolved:?}");
        let mut package = Package::from_cubism(ir, "m", "cubism-moc3-v3");
        let mut report = LoadReport::new();
        for d in unresolved {
            report.warn(d);
        }
        report.note("kept");
        record_warnings(&mut package, &report);
        assert_eq!(package.manifest.import_warnings, vec!["kept".to_string()]);
        assert!(package.encode_model().is_ok());
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
            physics: None,
        };
        assert!(!empty.is_cubism());
    }
}
