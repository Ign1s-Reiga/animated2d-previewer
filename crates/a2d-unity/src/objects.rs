//! Reading the few object fields an importer needs to find its way around.
//!
//! With no type tree in a shipping bundle, each of these is parsed by hand from
//! the class's known field order. Only the leading fields are read — enough to
//! identify an object — and the rest is left to whoever recognises it.
//!
//! Field orders, all little-endian, verified against a real 2022.3 bundle:
//!
//! ```text
//! MonoBehaviour   PPtr m_GameObject (12) · u8 m_Enabled + align(4)
//!                 · PPtr m_Script (12) · string m_Name
//! GameObject      PPtr[] m_Component · i32 m_Layer · string m_Name
//! MonoScript      string m_Name · string m_ExecutionOrder... (see below)
//! AssetBundle     string m_Name · PPtr[] m_PreloadTable
//!                 · (string, AssetInfo)[] m_Container
//! ```
//!
//! A `PPtr` is a file index (`i32`) plus a path id (`i64`); index 0 means "this
//! file", anything else indexes the externals table.

use std::collections::HashMap;

use a2d_core::DecodeError;

use crate::reader::{Endian, Reader};
use crate::serialized::{ClassId, Object, SerializedFile};

/// A reference to another object.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct PPtr {
    pub file_index: i32,
    pub path_id: i64,
}

impl PPtr {
    pub fn is_null(self) -> bool {
        self.path_id == 0
    }

    /// Whether the target lives in the same file.
    pub fn is_local(self) -> bool {
        self.file_index == 0 && self.path_id != 0
    }

    fn read(r: &mut Reader<'_>) -> Result<PPtr, DecodeError> {
        Ok(PPtr { file_index: r.i32()?, path_id: r.i64()? })
    }
}

/// The C# type behind a `MonoBehaviour`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ScriptInfo {
    pub class_name: String,
    pub namespace: String,
    pub assembly: String,
}

impl ScriptInfo {
    /// `Namespace.ClassName`, or just the class when there is no namespace.
    pub fn full_name(&self) -> String {
        if self.namespace.is_empty() {
            self.class_name.clone()
        } else {
            format!("{}.{}", self.namespace, self.class_name)
        }
    }
}

/// What the inspector knows about one object.
#[derive(Debug, Clone)]
pub struct ObjectInfo {
    pub path_id: i64,
    pub class_id: ClassId,
    pub byte_size: u32,
    /// `m_Name`, where the class carries one.
    pub name: Option<String>,
    /// For a `MonoBehaviour`, the C# type it instantiates.
    pub script: Option<ScriptInfo>,
    /// The path this asset was authored under, from the bundle's own table.
    pub asset_path: Option<String>,
}

impl ObjectInfo {
    /// The most useful label available: the script class for a behaviour,
    /// otherwise the Unity class.
    pub fn type_label(&self) -> String {
        match &self.script {
            Some(s) => s.class_name.clone(),
            None => self.class_id.name(),
        }
    }
}

/// Every object in a serialized file, identified as far as it can be.
pub struct Inventory {
    pub objects: Vec<ObjectInfo>,
}

impl Inventory {
    /// Identifies every object in `file`.
    pub fn build(file: &SerializedFile) -> Inventory {
        // Scripts first: a behaviour's identity is a reference into these.
        let mut scripts: HashMap<i64, ScriptInfo> = HashMap::new();
        for object in &file.objects {
            if object.class_id != ClassId::MONO_SCRIPT {
                continue;
            }
            if let Ok(info) = read_mono_script(file, object) {
                scripts.insert(object.path_id, info);
            }
        }

        // Then the bundle's own table, which maps authored paths to objects.
        let mut paths: HashMap<i64, String> = HashMap::new();
        for object in &file.objects {
            if object.class_id != ClassId::ASSET_BUNDLE {
                continue;
            }
            if let Ok(table) = read_asset_bundle_container(file, object) {
                paths.extend(table);
            }
        }

        let objects = file
            .objects
            .iter()
            .map(|object| {
                let script = read_behaviour_script(file, object).ok().flatten().and_then(|p| scripts.get(&p).cloned());
                ObjectInfo {
                    path_id: object.path_id,
                    class_id: object.class_id,
                    byte_size: object.byte_size,
                    name: read_name(file, object).ok().flatten(),
                    script,
                    asset_path: paths.get(&object.path_id).cloned(),
                }
            })
            .collect();

        Inventory { objects }
    }

    /// Objects whose `MonoBehaviour` script has the given class name.
    pub fn by_script<'a>(&'a self, class_name: &'a str) -> impl Iterator<Item = &'a ObjectInfo> {
        self.objects.iter().filter(move |o| o.script.as_ref().is_some_and(|s| s.class_name == class_name))
    }

    /// Objects of a given Unity class.
    pub fn by_class(&self, class_id: ClassId) -> impl Iterator<Item = &ObjectInfo> {
        self.objects.iter().filter(move |o| o.class_id == class_id)
    }
}

fn reader<'a>(file: &'a SerializedFile, object: &Object) -> Result<Reader<'a>, DecodeError> {
    Ok(Reader::new(file.object_data(object)?, Endian::Little))
}

/// `m_Name`, for the classes that begin with one.
///
/// A `MonoBehaviour` carries its name too, but after its header, so it is read
/// separately.
pub fn read_name(file: &SerializedFile, object: &Object) -> Result<Option<String>, DecodeError> {
    if object.class_id == ClassId::MONO_BEHAVIOUR {
        let mut r = reader(file, object)?;
        skip_behaviour_header(&mut r)?;
        return Ok(Some(r.string()?));
    }
    if object.class_id == ClassId::GAME_OBJECT {
        let mut r = reader(file, object)?;
        // m_Component is a plain PPtr array; m_Layer follows it, then the name.
        let components = r.i32()?;
        if !(0..=1_000_000).contains(&components) {
            return Err(DecodeError::corrupt(format!("a GameObject declares {components} components")));
        }
        r.skip(components as usize * 12)?;
        let _layer = r.i32()?;
        return Ok(Some(r.string()?));
    }
    if !object.class_id.has_name_first() {
        return Ok(None);
    }
    Ok(Some(reader(file, object)?.string()?))
}

/// `m_GameObject`, `m_Enabled` and `m_Script`.
fn skip_behaviour_header(r: &mut Reader<'_>) -> Result<PPtr, DecodeError> {
    let _game_object = PPtr::read(r)?;
    let _enabled = r.u8()?;
    r.align(4)?;
    PPtr::read(r)
}

/// The script a `MonoBehaviour` instantiates, as a local path id.
fn read_behaviour_script(file: &SerializedFile, object: &Object) -> Result<Option<i64>, DecodeError> {
    if object.class_id != ClassId::MONO_BEHAVIOUR {
        return Ok(None);
    }
    let mut r = reader(file, object)?;
    let script = skip_behaviour_header(&mut r)?;
    Ok(script.is_local().then_some(script.path_id))
}

/// A `MonoScript`'s C# identity.
///
/// The fields between the name and the class name vary by Unity version, so
/// they are stepped over by size rather than named: an `i32` execution order and
/// a 16-byte properties hash at every version this reads.
fn read_mono_script(file: &SerializedFile, object: &Object) -> Result<ScriptInfo, DecodeError> {
    let mut r = reader(file, object)?;
    let _name = r.string()?;
    let _execution_order = r.i32()?;
    r.skip(16)?; // m_PropertiesHash
    let class_name = r.string()?;
    let namespace = r.string()?;
    let assembly = r.string()?;
    Ok(ScriptInfo { class_name, namespace, assembly })
}

/// The bundle's `m_Container`: authored path to object.
///
/// This is what preserves an asset's authored path after the editor is gone,
/// and it is the only reliable way to tell which of many `MonoBehaviour`s is the
/// model, the fade list, or a motion.
fn read_asset_bundle_container(file: &SerializedFile, object: &Object) -> Result<HashMap<i64, String>, DecodeError> {
    let mut r = reader(file, object)?;
    let _name = r.string()?;

    // m_PreloadTable: PPtr[]
    let preload = r.i32()?;
    if !(0..=10_000_000).contains(&preload) {
        return Err(DecodeError::corrupt(format!("the bundle's preload table declares {preload} entries")));
    }
    r.skip(preload as usize * 12)?;

    let count = r.i32()?;
    if !(0..=10_000_000).contains(&count) {
        return Err(DecodeError::corrupt(format!("the bundle's container declares {count} entries")));
    }
    let mut out = HashMap::with_capacity(count.min(4096) as usize);
    for _ in 0..count {
        let path = r.string()?;
        // AssetInfo: i32 preloadIndex, i32 preloadSize, PPtr asset.
        let _preload_index = r.i32()?;
        let _preload_size = r.i32()?;
        let asset = PPtr::read(&mut r)?;
        if asset.is_local() {
            out.insert(asset.path_id, path);
        }
    }
    Ok(out)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_pptr_knows_when_it_points_outside_its_file() {
        assert!(PPtr { file_index: 0, path_id: 5 }.is_local());
        assert!(!PPtr { file_index: 2, path_id: 5 }.is_local(), "another file");
        assert!(!PPtr { file_index: 0, path_id: 0 }.is_local(), "null");
        assert!(PPtr { file_index: 0, path_id: 0 }.is_null());
    }

    #[test]
    fn a_script_full_name_omits_an_empty_namespace() {
        let ns = ScriptInfo {
            class_name: "CubismMoc".into(),
            namespace: "Live2D.Cubism.Core".into(),
            assembly: "Live2D".into(),
        };
        assert_eq!(ns.full_name(), "Live2D.Cubism.Core.CubismMoc");
        let bare = ScriptInfo { class_name: "Thing".into(), namespace: String::new(), assembly: "A".into() };
        assert_eq!(bare.full_name(), "Thing");
    }

    #[test]
    fn an_object_labels_itself_by_script_where_it_has_one() {
        let mut info = ObjectInfo {
            path_id: 1,
            class_id: ClassId::MONO_BEHAVIOUR,
            byte_size: 10,
            name: None,
            script: None,
            asset_path: None,
        };
        // Without a script all that can be said is the Unity class.
        assert_eq!(info.type_label(), "MonoBehaviour");
        info.script = Some(ScriptInfo {
            class_name: "CubismMoc".into(),
            namespace: "Live2D.Cubism.Core".into(),
            assembly: "Live2D".into(),
        });
        assert_eq!(info.type_label(), "CubismMoc");
    }
}
