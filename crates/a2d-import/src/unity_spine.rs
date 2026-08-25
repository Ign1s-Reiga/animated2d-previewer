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
//! filters by that yet, because the bundles seen hold one rig each; when one
//! turns up holding several, this is where the choice belongs, not downstream.

use a2d_core::{DecodeError, Degradation, LoadReport};
use a2d_unity::{Bundle, ClassId, Endian, Inventory, ObjectInfo, Reader, SerializedFile};

use crate::detect::{classify, AssetKind};

/// A `TextAsset` payload and how it was found.
#[derive(Debug, Clone)]
pub struct SpineTextAsset {
    /// The asset's own name, which keeps its original extension.
    pub name: String,
    /// Path the asset was authored under, when the bundle still records it.
    pub asset_path: Option<String>,
    pub bytes: Vec<u8>,
}

/// A texture page.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SpineTextureEntry {
    pub name: String,
    pub asset_path: Option<String>,
}

/// Everything found in one bundle.
#[derive(Debug, Clone)]
pub struct SpineInventory {
    /// The exact player revision the bundle was built with.
    pub unity_revision: String,
    /// Total objects in the serialized file.
    pub object_count: usize,
    /// The skeleton, with the version detection already done.
    pub skeleton: Option<SpineTextAsset>,
    /// What the skeleton turned out to be, as the detector reported it.
    pub skeleton_kind: Option<AssetKind>,
    pub atlas: Option<SpineTextAsset>,
    pub textures: Vec<SpineTextureEntry>,
    /// spine-unity component classes present, which is what marks the bundle.
    pub components: Vec<String>,
    /// `TextAsset`s that are neither the skeleton nor the atlas.
    pub other_text_assets: Vec<String>,
}

impl SpineInventory {
    /// Whether enough was found to be worth reconstructing.
    ///
    /// A skeleton with no atlas cannot be drawn and an atlas with no skeleton
    /// is not a model, so both are required rather than either.
    pub fn is_spine(&self) -> bool {
        self.skeleton.is_some() && self.atlas.is_some()
    }
}

/// The spine-unity classes that identify a bundle as holding a rig.
const SPINE_COMPONENTS: &[&str] =
    &["SkeletonDataAsset", "SpineAtlasAsset", "SkeletonAnimation", "SkeletonGraphic", "SkeletonRenderer"];

/// Reads a bundle and reports the Spine assets in it.
pub fn inspect_spine_bundle(bytes: &[u8], report: &mut LoadReport) -> Result<SpineInventory, DecodeError> {
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
    let (mut skeleton, mut skeleton_kind, mut atlas) = (None, None, None);
    let mut other_text_assets = Vec::new();
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
            kind @ AssetKind::SpineSkeleton { .. } => match skeleton {
                None => {
                    skeleton_kind = Some(kind);
                    skeleton = Some(payload);
                }
                Some(_) => {
                    // Ambiguity is an error, not a coin flip (spec §10). It is
                    // reported rather than raised so the rest still loads.
                    report.warn(Degradation::Note(format!(
                        "this bundle holds more than one Spine skeleton; {:?} was ignored",
                        payload.name
                    )));
                }
            },
            AssetKind::SpineAtlas => match atlas {
                None => atlas = Some(payload),
                Some(_) => report.warn(Degradation::Note(format!(
                    "this bundle holds more than one atlas; {:?} was ignored",
                    payload.name
                ))),
            },
            _ => other_text_assets.push(payload.name),
        }
    }

    if skeleton.is_some() && atlas.is_none() {
        report.warn(Degradation::Note("this bundle holds a skeleton but no atlas, so it cannot be drawn".into()));
    }

    let textures = inventory
        .by_class(ClassId::TEXTURE_2D)
        .map(|o| SpineTextureEntry {
            name: o.name.clone().unwrap_or_else(|| format!("texture#{}", o.path_id)),
            asset_path: o.asset_path.clone(),
        })
        .collect();

    Ok(SpineInventory {
        unity_revision: bundle.unity_revision.clone(),
        object_count: inventory.objects.len(),
        skeleton,
        skeleton_kind,
        atlas,
        textures,
        components,
        other_text_assets,
    })
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

    #[test]
    fn a_bundle_needs_both_halves_to_be_reconstructable() {
        let mut inventory = SpineInventory {
            unity_revision: String::new(),
            object_count: 0,
            skeleton: None,
            skeleton_kind: None,
            atlas: None,
            textures: Vec::new(),
            components: Vec::new(),
            other_text_assets: Vec::new(),
        };
        assert!(!inventory.is_spine());

        let asset = |name: &str| SpineTextAsset { name: name.into(), asset_path: None, bytes: Vec::new() };
        inventory.skeleton = Some(asset("a.skel"));
        assert!(!inventory.is_spine(), "a skeleton with no atlas cannot be drawn");
        inventory.atlas = Some(asset("a.atlas"));
        assert!(inventory.is_spine());
    }

    #[test]
    fn the_spine_component_list_covers_the_classes_that_mark_a_bundle() {
        // These are what spine-unity attaches; a bundle with none of them is
        // not a Spine bundle however its text assets are named.
        assert!(SPINE_COMPONENTS.contains(&"SkeletonDataAsset"));
        assert!(SPINE_COMPONENTS.contains(&"SpineAtlasAsset"));
    }
}
