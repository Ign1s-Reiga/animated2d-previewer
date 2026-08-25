//! Unity's serialized file: the object table inside a bundle node.
//!
//! The header is big-endian and declares the byte order of everything after it.
//! Layout, by file version (2022.3 writes version 22):
//!
//! ```text
//! u32   metadata size          (superseded at v22)
//! u32   file size              (superseded at v22)
//! u32   version
//! u32   data offset            (superseded at v22)
//! v>=9: u8 endianness, u8[3] reserved
//! v>=22: u32 metadata size, i64 file size, i64 data offset, i64 unknown
//! ```
//!
//! Then, in the declared byte order: the Unity version string, the target
//! platform, a type table, the object table, script and external references.
//!
//! # Why the type tree is usually absent
//!
//! A type tree describes a class field-by-field, which is what lets a general
//! tool parse an arbitrary `MonoBehaviour`. Shipping builds strip it to save
//! space, and this bundle is no exception. Everything here therefore works from
//! the *class id* and the script reference instead, and hands raw bytes onward.

use a2d_core::DecodeError;

use crate::reader::{Endian, Reader};

/// Unity's built-in class ids, for the handful that matter here.
///
/// The full list runs to hundreds; naming only what is acted on keeps the
/// mapping honest about what this reader actually understands.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ClassId(pub i32);

impl ClassId {
    pub const GAME_OBJECT: ClassId = ClassId(1);
    pub const TRANSFORM: ClassId = ClassId(4);
    pub const MATERIAL: ClassId = ClassId(21);
    pub const TEXTURE_2D: ClassId = ClassId(28);
    pub const MESH_FILTER: ClassId = ClassId(33);
    pub const MESH_RENDERER: ClassId = ClassId(23);
    pub const MESH: ClassId = ClassId(43);
    pub const MESH_COLLIDER: ClassId = ClassId(64);
    pub const ANIMATOR: ClassId = ClassId(95);
    pub const ANIMATION_CLIP: ClassId = ClassId(74);
    pub const ANIMATOR_CONTROLLER: ClassId = ClassId(91);
    pub const MONO_BEHAVIOUR: ClassId = ClassId(114);
    pub const MONO_SCRIPT: ClassId = ClassId(115);
    pub const SPRITE: ClassId = ClassId(213);
    pub const TEXT_ASSET: ClassId = ClassId(49);
    pub const ASSET_BUNDLE: ClassId = ClassId(142);

    /// A readable name, falling back to the bare number.
    pub fn name(self) -> String {
        match self {
            ClassId::GAME_OBJECT => "GameObject".into(),
            ClassId::TRANSFORM => "Transform".into(),
            ClassId::MATERIAL => "Material".into(),
            ClassId::TEXTURE_2D => "Texture2D".into(),
            ClassId::MESH_FILTER => "MeshFilter".into(),
            ClassId::MESH_RENDERER => "MeshRenderer".into(),
            ClassId::MESH => "Mesh".into(),
            ClassId::MESH_COLLIDER => "MeshCollider".into(),
            ClassId::ANIMATOR => "Animator".into(),
            ClassId::ANIMATION_CLIP => "AnimationClip".into(),
            ClassId::ANIMATOR_CONTROLLER => "AnimatorController".into(),
            ClassId::MONO_BEHAVIOUR => "MonoBehaviour".into(),
            ClassId::MONO_SCRIPT => "MonoScript".into(),
            ClassId::SPRITE => "Sprite".into(),
            ClassId::ASSET_BUNDLE => "AssetBundle".into(),
            ClassId(other) => format!("Class{other}"),
        }
    }

    /// Whether objects of this class begin with a `m_Name` string.
    ///
    /// Components do not: they identify themselves through the `GameObject`
    /// that owns them. Nor does `GameObject` itself, whose name sits behind its
    /// component list -- reading it as if it led produced two NUL bytes, which
    /// is what the control-character check on a real bundle caught.
    pub fn has_name_first(self) -> bool {
        matches!(
            self,
            ClassId::MATERIAL
                | ClassId::TEXTURE_2D
                | ClassId::MESH
                | ClassId::ANIMATION_CLIP
                | ClassId::ANIMATOR_CONTROLLER
                | ClassId::MONO_SCRIPT
                | ClassId::SPRITE
                | ClassId::ASSET_BUNDLE
        )
    }
}

/// One entry in the file's type table.
#[derive(Debug, Clone)]
pub struct TypeEntry {
    pub class_id: ClassId,
    /// Index into the script-type table, for `MonoBehaviour` entries.
    pub script_type_index: i16,
    pub is_stripped: bool,
}

/// One object in the file.
#[derive(Debug, Clone)]
pub struct Object {
    pub path_id: i64,
    /// Offset of the object's bytes, relative to the file's data section.
    pub byte_start: u64,
    pub byte_size: u32,
    /// Index into [`SerializedFile::types`].
    pub type_index: usize,
    pub class_id: ClassId,
}

/// A file this one references, by index.
#[derive(Debug, Clone)]
pub struct External {
    pub path: String,
}

/// A parsed serialized file, with its object bytes still in place.
pub struct SerializedFile {
    pub version: u32,
    pub unity_version: String,
    pub target_platform: i32,
    pub has_type_tree: bool,
    pub types: Vec<TypeEntry>,
    pub objects: Vec<Object>,
    pub externals: Vec<External>,
    data_offset: usize,
    bytes: Vec<u8>,
}

/// Prints the shape of the file, never its bytes.
impl std::fmt::Debug for SerializedFile {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("SerializedFile")
            .field("version", &self.version)
            .field("unity_version", &self.unity_version)
            .field("has_type_tree", &self.has_type_tree)
            .field("types", &self.types.len())
            .field("objects", &self.objects.len())
            .field("externals", &self.externals.len())
            .field("bytes", &self.bytes.len())
            .finish()
    }
}

impl SerializedFile {
    /// The raw bytes of one object.
    pub fn object_data(&self, object: &Object) -> Result<&[u8], DecodeError> {
        let start = self
            .data_offset
            .checked_add(usize::try_from(object.byte_start).unwrap_or(usize::MAX))
            .ok_or_else(|| DecodeError::corrupt("an object offset overflowed".to_string()))?;
        let end = start.checked_add(object.byte_size as usize).filter(|e| *e <= self.bytes.len()).ok_or_else(|| {
            DecodeError::corrupt(format!(
                "object {} spans {} bytes at {start}, past the {} the file holds",
                object.path_id,
                object.byte_size,
                self.bytes.len()
            ))
        })?;
        Ok(&self.bytes[start..end])
    }

    pub fn parse(bytes: &[u8]) -> Result<SerializedFile, DecodeError> {
        let mut r = Reader::new(bytes, Endian::Big);
        let mut metadata_size = r.u32()? as u64;
        let mut file_size = r.u32()? as u64;
        let version = r.u32()?;
        let mut data_offset = r.u32()? as u64;

        if version >= 22 {
            // The 32-bit fields above are placeholders once files may exceed 4GB.
            r.skip(4)?; // endianness byte plus three reserved
            metadata_size = r.u32()? as u64;
            file_size = r.u64()?;
            data_offset = r.u64()?;
            r.skip(8)?; // reserved
                        // Metadata is little-endian from here in every 22+ file seen.
            r.set_endian(Endian::Little);
        } else if version >= 9 {
            let big_endian = r.u8()? != 0;
            r.skip(3)?;
            r.set_endian(if big_endian { Endian::Big } else { Endian::Little });
        }
        let _ = (metadata_size, file_size);

        if data_offset as usize > bytes.len() {
            return Err(DecodeError::corrupt(format!(
                "the data section starts at {data_offset} but the file is only {} bytes",
                bytes.len()
            )));
        }

        let unity_version = if version >= 7 { r.cstring()? } else { String::new() };
        let target_platform = if version >= 8 { r.i32()? } else { 0 };
        let has_type_tree = if version >= 13 { r.bool()? } else { true };

        let types = read_types(&mut r, version, has_type_tree)?;
        let objects = read_objects(&mut r, version, &types)?;
        read_script_types(&mut r, version)?;
        let externals = read_externals(&mut r, version)?;

        Ok(SerializedFile {
            version,
            unity_version,
            target_platform,
            has_type_tree,
            types,
            objects,
            externals,
            data_offset: data_offset as usize,
            bytes: bytes.to_vec(),
        })
    }
}

fn read_types(r: &mut Reader<'_>, version: u32, has_type_tree: bool) -> Result<Vec<TypeEntry>, DecodeError> {
    let count = r.i32()?;
    if !(0..=1_000_000).contains(&count) {
        return Err(DecodeError::corrupt(format!("the file declares {count} types")));
    }
    let mut out = Vec::with_capacity(count.min(4096) as usize);
    for _ in 0..count {
        out.push(read_type(r, version, has_type_tree, false)?);
    }
    Ok(out)
}

fn read_type(
    r: &mut Reader<'_>,
    version: u32,
    has_type_tree: bool,
    is_ref_type: bool,
) -> Result<TypeEntry, DecodeError> {
    let class_id = ClassId(r.i32()?);
    let is_stripped = if version >= 16 { r.bool()? } else { false };
    let script_type_index = if version >= 17 { r.i16()? } else { -1 };

    if version >= 13 {
        let needs_script_hash = if is_ref_type {
            script_type_index >= 0
        } else {
            // Pre-16 files hashed every negative class id; from 16 the marker is
            // MonoBehaviour itself.
            (version < 16 && class_id.0 < 0) || (version >= 16 && class_id == ClassId::MONO_BEHAVIOUR)
        };
        if needs_script_hash {
            r.skip(16)?; // script id hash
        }
        r.skip(16)?; // old type hash
    }

    if has_type_tree {
        skip_type_tree(r, version)?;
        // Version 21 began recording which other types a type depends on, as a
        // plain int array after the tree. Missing it puts every later type out
        // of step, which is how this was found.
        if version >= 21 {
            if is_ref_type {
                // A reference type names itself here instead.
                let _class = r.cstring()?;
                let _namespace = r.cstring()?;
                let _assembly = r.cstring()?;
            } else {
                let dependencies = r.i32()?;
                if !(0..=1_000_000).contains(&dependencies) {
                    return Err(DecodeError::corrupt(format!("a type declares {dependencies} dependencies")));
                }
                r.skip(dependencies as usize * 4)?;
            }
        }
    }
    Ok(TypeEntry { class_id, script_type_index, is_stripped })
}

/// Steps over a type tree without building it.
///
/// Nothing here needs the schema — the objects that matter are parsed by hand
/// from their class — but the bytes have to be consumed to stay in step.
fn skip_type_tree(r: &mut Reader<'_>, version: u32) -> Result<(), DecodeError> {
    if version < 12 && version != 10 {
        return Err(DecodeError::corrupt(format!(
            "type trees in file version {version} use the older recursive layout, which is not read here"
        )));
    }
    let node_count = r.i32()?;
    let string_size = r.i32()?;
    if node_count < 0 || string_size < 0 {
        return Err(DecodeError::corrupt("a type tree declares a negative size".to_string()));
    }
    // Each node is 24 bytes at version 12+, 32 from version 19.
    let node_size = if version >= 19 { 32 } else { 24 };
    r.skip(node_count as usize * node_size)?;
    r.skip(string_size as usize)?;
    Ok(())
}

fn read_objects(r: &mut Reader<'_>, version: u32, types: &[TypeEntry]) -> Result<Vec<Object>, DecodeError> {
    let count = r.i32()?;
    if !(0..=10_000_000).contains(&count) {
        return Err(DecodeError::corrupt(format!("the file declares {count} objects")));
    }
    let mut out = Vec::with_capacity(count.min(65536) as usize);
    for _ in 0..count {
        if version >= 14 {
            r.align(4)?;
        }
        let path_id = if version >= 14 { r.i64()? } else { r.i32()? as i64 };
        let byte_start = if version >= 22 { r.u64()? } else { r.u32()? as u64 };
        let byte_size = r.u32()?;
        let type_index = r.i32()?;

        let entry = usize::try_from(type_index).ok().and_then(|i| types.get(i)).ok_or_else(|| {
            DecodeError::corrupt(format!("object {path_id} names type index {type_index}, of {}", types.len()))
        })?;
        out.push(Object { path_id, byte_start, byte_size, type_index: type_index as usize, class_id: entry.class_id });
    }
    Ok(out)
}

fn read_script_types(r: &mut Reader<'_>, version: u32) -> Result<(), DecodeError> {
    if version < 11 {
        return Ok(());
    }
    let count = r.i32()?;
    if !(0..=1_000_000).contains(&count) {
        return Err(DecodeError::corrupt(format!("the file declares {count} script types")));
    }
    for _ in 0..count {
        r.i32()?; // local serialized file index
        if version < 14 {
            r.i32()?;
        } else {
            r.align(4)?;
            r.i64()?;
        }
    }
    Ok(())
}

fn read_externals(r: &mut Reader<'_>, version: u32) -> Result<Vec<External>, DecodeError> {
    let count = r.i32()?;
    if !(0..=1_000_000).contains(&count) {
        return Err(DecodeError::corrupt(format!("the file declares {count} externals")));
    }
    let mut out = Vec::with_capacity(count.min(1024) as usize);
    for _ in 0..count {
        if version >= 6 {
            let _temp = r.cstring()?;
        }
        if version >= 5 {
            r.skip(16)?; // guid
            r.i32()?; // type
        }
        out.push(External { path: r.cstring()? });
    }
    Ok(out)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn class_names_cover_what_the_inspector_reports() {
        assert_eq!(ClassId::MONO_BEHAVIOUR.name(), "MonoBehaviour");
        assert_eq!(ClassId::TEXTURE_2D.name(), "Texture2D");
        // An unmapped class is reported by number rather than guessed at.
        assert_eq!(ClassId(9999).name(), "Class9999");
    }

    #[test]
    fn only_asset_classes_start_with_a_name() {
        assert!(ClassId::TEXTURE_2D.has_name_first());
        assert!(!ClassId::GAME_OBJECT.has_name_first(), "a GameObject's name follows its component list");
        // Components identify themselves through their GameObject.
        assert!(!ClassId::TRANSFORM.has_name_first());
        assert!(!ClassId::MONO_BEHAVIOUR.has_name_first(), "a MonoBehaviour's name follows its script reference");
    }

    #[test]
    fn a_truncated_header_is_an_error_and_never_a_panic() {
        for cut in 0..64 {
            let _ = SerializedFile::parse(&vec![0u8; cut]);
        }
    }

    #[test]
    fn an_absurd_object_count_is_refused_before_allocating() {
        // Header claiming version 22, then a type count that would allocate GBs.
        let mut b = Vec::new();
        b.extend_from_slice(&0u32.to_be_bytes());
        b.extend_from_slice(&0u32.to_be_bytes());
        b.extend_from_slice(&22u32.to_be_bytes());
        b.extend_from_slice(&0u32.to_be_bytes());
        b.extend_from_slice(&[0, 0, 0, 0]);
        b.extend_from_slice(&0u32.to_be_bytes());
        b.extend_from_slice(&0u64.to_be_bytes());
        b.extend_from_slice(&0u64.to_be_bytes());
        b.extend_from_slice(&0u64.to_be_bytes());
        b.extend_from_slice(b"2022.3.20p1\0");
        b.extend_from_slice(&19i32.to_le_bytes()); // target platform
        b.push(0); // no type tree
        b.extend_from_slice(&i32::MAX.to_le_bytes()); // type count
        let err = SerializedFile::parse(&b).unwrap_err();
        assert!(err.to_string().contains("types"), "{err}");
    }
}
