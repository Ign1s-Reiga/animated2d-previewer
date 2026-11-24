//! Synthetic bundles shared by this crate's tests.

use a2d_unity::testing::BundleBuilder;

/// A one-bone, one-region rig with a single `idle` animation.
pub const SKELETON_JSON: &str = r#"{
  "skeleton": { "spine": "4.1.00", "width": 16, "height": 16 },
  "bones": [ { "name": "root" } ],
  "slots": [ { "name": "body", "bone": "root", "attachment": "body" } ],
  "skins": [ { "name": "default", "attachments": { "body": { "body": { "width": 16, "height": 16 } } } } ],
  "animations": { "idle": { "bones": { "root": { "rotate": [ { "time": 0, "angle": 0 }, { "time": 1, "angle": 10 } ] } } } }
}"#;

/// An atlas with one page, `page`, holding one region named `body`.
pub fn atlas_text(page: &str) -> String {
    format!(
        "\n{page}\nsize: 2,2\nformat: RGBA8888\nfilter: Linear,Linear\nrepeat: none\nbody\n  rotate: false\n  xy: 0, 0\n  size: 2, 2\n  orig: 2, 2\n  offset: 0, 0\n  index: -1\n"
    )
}

/// Four RGBA pixels, bottom row first.
pub fn pixels() -> Vec<u8> {
    vec![255, 0, 0, 255, 0, 255, 0, 255, 0, 0, 255, 255, 255, 255, 255, 255]
}

/// A bundle with one complete rig named `hero`: skeleton, atlas, one page.
pub fn complete_rig() -> BundleBuilder {
    BundleBuilder::new()
        .text_asset("hero.skel", SKELETON_JSON.as_bytes())
        .text_asset("hero.atlas", atlas_text("hero.png").as_bytes())
        .texture("hero", 2, 2, &pixels())
}
