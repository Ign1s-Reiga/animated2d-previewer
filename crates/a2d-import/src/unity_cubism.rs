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
//! * `CubismFadeMotionData` — one per motion, and it keeps the path of the
//!   `.motion3.json` it was built from, which is the only place the original
//!   name survives.
//! * `CubismFadeMotionList` — the index tying those to their clips.
//!
//! Motions themselves become Unity `AnimationClip`s, and textures become
//! `Texture2D`. The bundle's own container table maps each of these back to the
//! path it was authored under.

use a2d_core::{DecodeError, Degradation, LoadReport};
use a2d_unity::{Bundle, ClassId, Inventory, ObjectInfo, SerializedFile};

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

/// The `.motion3.json` path a `CubismFadeMotionData` records.
///
/// The class stores its source path as the first string after the behaviour
/// header and name, which is what lets a motion keep its original identity.
fn motion3_path(file: &SerializedFile, info: &ObjectInfo) -> Option<String> {
    let object = file.objects.iter().find(|o| o.path_id == info.path_id)?;
    let data = file.object_data(object).ok()?;
    let name_len_at = 12 + 4 + 12;
    let name_len = read_i32(data, name_len_at).ok()?;
    if !(0..=4096).contains(&name_len) {
        return None;
    }
    let after_name = name_len_at + 4 + name_len as usize;
    let path_len_at = (after_name + 3) & !3;
    let path_len = read_i32(data, path_len_at).ok()?;
    if !(0..=4096).contains(&path_len) {
        return None;
    }
    let start = path_len_at + 4;
    let end = start.checked_add(path_len as usize)?;
    let raw = data.get(start..end)?;
    let text = String::from_utf8_lossy(raw).into_owned();
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
