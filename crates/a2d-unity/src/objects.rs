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
    scripts: HashMap<i64, ScriptInfo>,
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

        Inventory { objects, scripts }
    }

    /// The C# identity of the local `MonoScript` with this path id.
    pub fn script(&self, path_id: i64) -> Option<&ScriptInfo> {
        self.scripts.get(&path_id)
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

/// A `MonoBehaviour`'s own fields: a reader positioned just past the header
/// and `m_Name`, plus that name.
///
/// This is the generic half of reading a behaviour whose type tree is stripped;
/// the field order after it belongs to the C# class, which only the importer
/// that recognises the class may interpret. Errors when `object` is not a
/// `MonoBehaviour`.
pub fn behaviour_body<'a>(file: &'a SerializedFile, object: &Object) -> Result<(Reader<'a>, String), DecodeError> {
    if object.class_id != ClassId::MONO_BEHAVIOUR {
        return Err(DecodeError::corrupt(format!(
            "object {} is a {}, not a MonoBehaviour",
            object.path_id,
            object.class_id.name()
        )));
    }
    let mut r = reader(file, object)?;
    skip_behaviour_header(&mut r)?;
    let name = r.string()?;
    Ok((r, name))
}

/// The name of the `GameObject` a `MonoBehaviour` is attached to.
///
/// `Ok(None)` when the behaviour is detached, or its owner is not in this
/// file. Errors when `object` is not a `MonoBehaviour`, or is unreadable.
pub fn behaviour_owner_name(file: &SerializedFile, object: &Object) -> Result<Option<String>, DecodeError> {
    if object.class_id != ClassId::MONO_BEHAVIOUR {
        return Err(DecodeError::corrupt(format!(
            "object {} is a {}, not a MonoBehaviour",
            object.path_id,
            object.class_id.name()
        )));
    }
    let mut r = reader(file, object)?;
    let (game_object, _) = read_behaviour_header(&mut r)?;
    if !game_object.is_local() {
        return Ok(None);
    }
    match file.find_object(game_object.path_id) {
        Some(owner) if owner.class_id == ClassId::GAME_OBJECT => read_name(file, owner),
        _ => Ok(None),
    }
}

/// `m_GameObject`, `m_Enabled` and `m_Script`: the owner, then the script.
fn read_behaviour_header(r: &mut Reader<'_>) -> Result<(PPtr, PPtr), DecodeError> {
    let game_object = PPtr::read(r)?;
    let _enabled = r.u8()?;
    r.align(4)?;
    Ok((game_object, PPtr::read(r)?))
}

/// The header, keeping only the script.
fn skip_behaviour_header(r: &mut Reader<'_>) -> Result<PPtr, DecodeError> {
    Ok(read_behaviour_header(r)?.1)
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

    // --- A minimal serialized file, enough to exercise object lookup. ---

    fn put_i32(o: &mut Vec<u8>, v: i32) {
        o.extend_from_slice(&v.to_le_bytes());
    }
    fn put_string(o: &mut Vec<u8>, s: &str) {
        put_i32(o, s.len() as i32);
        o.extend_from_slice(s.as_bytes());
        while o.len() % 4 != 0 {
            o.push(0);
        }
    }
    fn put_pptr(o: &mut Vec<u8>, file: i32, path: i64) {
        put_i32(o, file);
        o.extend_from_slice(&path.to_le_bytes());
    }

    /// `(path_id, type_index, bytes)` objects into a version-17 file with no
    /// type tree. Type 0 is GameObject, 1 MonoScript, 2 MonoBehaviour.
    fn build(objects: &[(i64, usize, Vec<u8>)]) -> SerializedFile {
        // The 20-byte header is a multiple of four, so alignment inside the
        // metadata is the same as alignment from the start of the file.
        let mut meta = Vec::new();
        meta.extend_from_slice(b"2022.3.0f1\0");
        put_i32(&mut meta, 5); // target platform
        meta.push(0); // no type tree
        put_i32(&mut meta, 3);
        for class in [1i32, 115, 114] {
            put_i32(&mut meta, class);
            meta.push(0); // not stripped
            meta.extend_from_slice(&(-1i16).to_le_bytes());
            if class == 114 {
                meta.extend_from_slice(&[0; 16]); // script hash
            }
            meta.extend_from_slice(&[0; 16]); // old type hash
        }
        put_i32(&mut meta, objects.len() as i32);
        let mut data = Vec::new();
        for (path_id, type_index, bytes) in objects {
            while (20 + meta.len()) % 4 != 0 {
                meta.push(0);
            }
            meta.extend_from_slice(&path_id.to_le_bytes());
            meta.extend_from_slice(&(data.len() as u32).to_le_bytes());
            meta.extend_from_slice(&(bytes.len() as u32).to_le_bytes());
            put_i32(&mut meta, *type_index as i32);
            data.extend_from_slice(bytes);
        }
        put_i32(&mut meta, 0); // script types
        put_i32(&mut meta, 0); // externals

        let mut file = Vec::new();
        file.extend_from_slice(&0u32.to_be_bytes()); // metadata size, unused
        file.extend_from_slice(&0u32.to_be_bytes()); // file size, unused
        file.extend_from_slice(&17u32.to_be_bytes());
        file.extend_from_slice(&((20 + meta.len()) as u32).to_be_bytes());
        file.extend_from_slice(&[0, 0, 0, 0]); // little-endian, reserved
        file.extend_from_slice(&meta);
        file.extend_from_slice(&data);
        SerializedFile::parse(&file).expect("the synthetic file should parse")
    }

    fn game_object(name: &str) -> Vec<u8> {
        let mut o = Vec::new();
        put_i32(&mut o, 0); // no components
        put_i32(&mut o, 0); // layer
        put_string(&mut o, name);
        o
    }

    fn mono_script(class: &str) -> Vec<u8> {
        let mut o = Vec::new();
        put_string(&mut o, class);
        put_i32(&mut o, 0);
        o.extend_from_slice(&[0; 16]);
        put_string(&mut o, class);
        put_string(&mut o, "Some.Namespace");
        put_string(&mut o, "Assembly");
        o
    }

    fn behaviour(owner: (i32, i64), script: i64, name: &str) -> Vec<u8> {
        let mut o = Vec::new();
        put_pptr(&mut o, owner.0, owner.1);
        o.extend_from_slice(&[1, 0, 0, 0]); // enabled + padding
        put_pptr(&mut o, 0, script);
        put_string(&mut o, name);
        o
    }

    fn sample() -> SerializedFile {
        build(&[
            (10, 0, game_object("Owner")),
            (20, 1, mono_script("Thing")),
            (30, 2, behaviour((0, 10), 20, "b1")),
            (31, 2, behaviour((0, 0), 20, "detached")),
            (32, 2, behaviour((1, 10), 20, "elsewhere")),
            (33, 2, behaviour((0, 99), 20, "dangling")),
        ])
    }

    #[test]
    fn an_object_is_found_by_path_id() {
        let file = sample();
        assert_eq!(file.find_object(30).map(|o| o.class_id), Some(ClassId::MONO_BEHAVIOUR));
        assert_eq!(file.find_object(10).map(|o| o.class_id), Some(ClassId::GAME_OBJECT));
        assert!(file.find_object(12345).is_none());
    }

    #[test]
    fn a_behaviour_reports_the_name_of_its_owner() {
        let file = sample();
        let owner = |id| behaviour_owner_name(&file, file.find_object(id).unwrap());
        assert_eq!(owner(30).unwrap().as_deref(), Some("Owner"));
        assert_eq!(owner(31).unwrap(), None, "a detached behaviour has no owner");
        assert_eq!(owner(32).unwrap(), None, "an owner in another file is not resolved");
        assert_eq!(owner(33).unwrap(), None, "a dangling owner is not an error");
        // Only a behaviour has an owner in this sense.
        assert!(behaviour_owner_name(&file, file.find_object(10).unwrap()).is_err());
    }

    #[test]
    fn the_inventory_resolves_scripts_by_path_id() {
        let file = sample();
        let inventory = Inventory::build(&file);
        let script = inventory.script(20).expect("the script is known");
        assert_eq!(script.full_name(), "Some.Namespace.Thing");
        assert!(inventory.script(30).is_none(), "a behaviour is not a script");
        assert!(inventory.script(999).is_none());
        assert_eq!(inventory.by_script("Thing").count(), 4);
        let names: Vec<_> = inventory.by_script("Thing").filter_map(|o| o.name.clone()).collect();
        assert_eq!(names, ["b1", "detached", "elsewhere", "dangling"]);
    }
}
