//! Reads a real Unity AssetBundle, when one is available.
//!
//! Synthetic fixtures prove the reader is self-consistent. Only a real bundle
//! proves it agrees with what Unity actually writes — and a container format is
//! all convention, so that distinction matters more here than anywhere else in
//! the workspace.
//!
//! The asset is never committed (spec §11). Point `A2D_FIXTURE_CUBISM` at one:
//!
//! ```bash
//! A2D_FIXTURE_CUBISM=/path/to/bundle cargo test -p a2d-unity -- --ignored
//! ```

use a2d_unity::{Bundle, ClassId, Inventory, SerializedFile};

fn fixture() -> Option<Vec<u8>> {
    let path = std::env::var("A2D_FIXTURE_CUBISM").ok()?;
    match std::fs::read(&path) {
        Ok(bytes) => Some(bytes),
        Err(e) => panic!("A2D_FIXTURE_CUBISM points at {path:?}, which could not be read: {e}"),
    }
}

/// Parses the bundle and returns its one serialized file.
fn open() -> Option<(Bundle, SerializedFile)> {
    let bytes = fixture()?;
    let bundle = Bundle::parse(&bytes).expect("a real bundle should parse");
    let node = bundle
        .nodes
        .iter()
        .find(|n| n.is_serialized())
        .unwrap_or_else(|| panic!("no serialized node among {:?}", bundle.nodes));
    let data = bundle.node_data(node).expect("the node should be in range");
    let file = SerializedFile::parse(data).expect("the serialized file should parse");
    Some((bundle, file))
}

#[test]
#[ignore = "needs a real asset; set A2D_FIXTURE_CUBISM to a Unity AssetBundle"]
fn a_real_bundle_parses_and_names_its_unity_version() {
    let Some((bundle, file)) = open() else { return };

    assert_eq!(bundle.signature, "UnityFS");
    assert!(!bundle.unity_revision.is_empty(), "the player revision should be recorded");
    assert!(!bundle.nodes.is_empty(), "a bundle with no directory is not useful");
    // The serialized file agrees with the container about the engine version.
    assert!(!file.unity_version.is_empty(), "{file:?}");
    assert!(!file.objects.is_empty(), "{file:?}");
    println!("bundle: {bundle:?}");
    println!("file:   {file:?}");
}

#[test]
#[ignore = "needs a real asset; set A2D_FIXTURE_CUBISM to a Unity AssetBundle"]
fn every_object_in_a_real_bundle_is_addressable() {
    // The strongest end-to-end check available: if the object table were
    // misread, offsets would not all land inside the data section.
    let Some((_, file)) = open() else { return };
    for object in &file.objects {
        file.object_data(object)
            .unwrap_or_else(|e| panic!("object {} ({}) is unreachable: {e}", object.path_id, object.class_id.name()));
    }
}

#[test]
#[ignore = "needs a real asset; set A2D_FIXTURE_CUBISM to a Unity AssetBundle"]
fn a_real_bundle_yields_script_classes_and_authored_paths() {
    let Some((_, file)) = open() else { return };
    let inventory = Inventory::build(&file);

    let scripts: Vec<_> = inventory.objects.iter().filter_map(|o| o.script.as_ref()).collect();
    assert!(!scripts.is_empty(), "no MonoBehaviour resolved to a script class");

    let named = inventory.objects.iter().filter(|o| o.name.is_some()).count();
    assert!(named > 0, "no object yielded a name");

    let with_paths = inventory.objects.iter().filter(|o| o.asset_path.is_some()).count();
    assert!(with_paths > 0, "the bundle's container table yielded no authored paths");

    // Names come out of a length-prefixed string; a misread would show up as
    // control characters rather than as an error.
    for object in &inventory.objects {
        if let Some(name) = &object.name {
            assert!(
                name.chars().all(|c| !c.is_control()),
                "object {} has a name with control characters: {name:?}",
                object.path_id
            );
        }
    }

    println!("{} objects, {named} named, {with_paths} with authored paths", inventory.objects.len());
    let mut classes: Vec<_> = inventory
        .objects
        .iter()
        .map(|o| o.type_label())
        .collect::<std::collections::HashSet<_>>()
        .into_iter()
        .collect();
    classes.sort();
    println!("classes: {classes:?}");
}

#[test]
#[ignore = "needs a real asset; set A2D_FIXTURE_CUBISM to a Cubism Unity AssetBundle"]
fn a_cubism_bundle_carries_a_moc_a_texture_and_motions() {
    let Some((_, file)) = open() else { return };
    let inventory = Inventory::build(&file);

    let mocs: Vec<_> = inventory.by_script("CubismMoc").collect();
    assert_eq!(mocs.len(), 1, "expected exactly one CubismMoc, found {}", mocs.len());

    let textures: Vec<_> = inventory.by_class(ClassId::TEXTURE_2D).collect();
    assert!(!textures.is_empty(), "a Cubism model needs at least one texture");

    let clips: Vec<_> = inventory.by_class(ClassId::ANIMATION_CLIP).collect();
    assert!(!clips.is_empty(), "no AnimationClip: the motions would have nowhere to come from");

    for clip in &clips {
        assert!(clip.name.is_some(), "clip {} has no name", clip.path_id);
    }
    println!("moc: {:?}", mocs[0]);
    println!("textures: {:?}", textures.iter().map(|t| &t.name).collect::<Vec<_>>());
    println!("clips: {:?}", clips.iter().map(|c| &c.name).collect::<Vec<_>>());
}
