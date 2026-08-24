//! Texture atlas types.
//!
//! The atlas grammar itself is parsed in `a2d-spine`; only the decoded shape
//! lives here, because attachments in the IR reference regions by handle.

use crate::ir::ids::{AtlasPageId, AtlasRegionId};
use crate::math::Vec2;

/// Texture minification/magnification filter requested by the atlas.
///
/// The renderer maps these onto its own sampler descriptors; unmapped values
/// fall back to linear and are reported.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum TextureFilter {
    Nearest,
    #[default]
    Linear,
    MipMap,
    MipMapNearestNearest,
    MipMapLinearNearest,
    MipMapNearestLinear,
    MipMapLinearLinear,
}

/// Texture coordinate wrapping.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum TextureWrap {
    MirroredRepeat,
    #[default]
    ClampToEdge,
    Repeat,
}

/// One texture page: a single image file plus its sampler settings.
#[derive(Debug, Clone, PartialEq)]
pub struct AtlasPage {
    /// File name as written in the atlas, e.g. `character.png`.
    pub name: String,
    /// Pixel dimensions. Older atlas versions omit this; it is then filled in
    /// from the decoded image, because UV computation needs it.
    pub size: Option<(u32, u32)>,
    pub min_filter: TextureFilter,
    pub mag_filter: TextureFilter,
    pub u_wrap: TextureWrap,
    pub v_wrap: TextureWrap,
    /// Whether the page stores premultiplied alpha. Changes the blend equation.
    pub premultiplied_alpha: bool,
}

impl AtlasPage {
    pub fn new(name: impl Into<String>) -> Self {
        AtlasPage {
            name: name.into(),
            size: None,
            min_filter: TextureFilter::default(),
            mag_filter: TextureFilter::default(),
            u_wrap: TextureWrap::default(),
            v_wrap: TextureWrap::default(),
            premultiplied_alpha: false,
        }
    }
}

/// A packed sub-image on a page.
///
/// Coordinates are in pixels with the origin at the page's top-left, which is
/// how every atlas dialect writes them.
#[derive(Debug, Clone, PartialEq)]
pub struct AtlasRegion {
    pub name: String,
    pub page: AtlasPageId,
    /// Top-left corner on the page, in pixels.
    pub xy: (u32, u32),
    /// Size on the page, in pixels, *before* un-rotating.
    pub size: (u32, u32),
    /// Degrees the sub-image was rotated by when packed: 0, 90, 180 or 270.
    /// The legacy `rotate: true` flag means 90.
    pub rotate_deg: u16,
    /// Offset of the packed image inside its original, un-trimmed bounds.
    pub offset: (i32, i32),
    /// Size the image had before whitespace was stripped.
    pub original_size: (u32, u32),
    /// Trailing numeric suffix used for frame sequences, or `-1`.
    pub index: i32,
    /// Nine-patch split, in pixels: left, right, top, bottom.
    pub splits: Option<[i32; 4]>,
    /// Nine-patch content padding.
    pub pads: Option<[i32; 4]>,
}

impl AtlasRegion {
    /// Size of this region as it appears once un-rotated.
    pub fn unrotated_size(&self) -> (u32, u32) {
        if self.rotate_deg == 90 || self.rotate_deg == 270 {
            (self.size.1, self.size.0)
        } else {
            self.size
        }
    }

    /// The region's four UV corners, in the order bottom-left, bottom-right,
    /// top-right, top-left of the *un-rotated* image.
    ///
    /// Returns `None` when the page size is unknown, which callers report as a
    /// corrupt atlas rather than guessing a page size.
    pub fn corner_uvs(&self, page: &AtlasPage) -> Option<[Vec2; 4]> {
        let (pw, ph) = page.size?;
        if pw == 0 || ph == 0 {
            return None;
        }
        let (pw, ph) = (pw as f32, ph as f32);
        let u0 = self.xy.0 as f32 / pw;
        let v0 = self.xy.1 as f32 / ph;
        let u1 = (self.xy.0 + self.size.0) as f32 / pw;
        let v1 = (self.xy.1 + self.size.1) as f32 / ph;

        // Corners of the packed rectangle, then rotated so that index 0 is the
        // un-rotated image's bottom-left regardless of packing orientation.
        let bl = Vec2::new(u0, v1);
        let br = Vec2::new(u1, v1);
        let tr = Vec2::new(u1, v0);
        let tl = Vec2::new(u0, v0);
        Some(match self.rotate_deg {
            90 => [br, tr, tl, bl],
            180 => [tr, tl, bl, br],
            270 => [tl, bl, br, tr],
            _ => [bl, br, tr, tl],
        })
    }
}

/// A decoded atlas: pages plus the regions packed into them.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct Atlas {
    pub pages: Vec<AtlasPage>,
    /// Sorted by name, so lookup is a binary search and serialization is
    /// deterministic.
    pub regions: Vec<AtlasRegion>,
}

impl Atlas {
    pub fn page(&self, id: AtlasPageId) -> Option<&AtlasPage> {
        self.pages.get(id.index())
    }

    pub fn region(&self, id: AtlasRegionId) -> Option<&AtlasRegion> {
        self.regions.get(id.index())
    }

    /// Finds a region by its atlas name.
    ///
    /// Sequence regions share a name and differ by `index`; this returns the
    /// lowest-indexed one, matching what a static attachment expects.
    pub fn find(&self, name: &str) -> Option<AtlasRegionId> {
        let at = self.regions.binary_search_by(|r| r.name.as_str().cmp(name)).ok()?;
        // Walk back to the first entry with this name.
        let mut first = at;
        while first > 0 && self.regions[first - 1].name == name {
            first -= 1;
        }
        AtlasRegionId::from_index(first)
    }

    /// Restores the sorted-by-name invariant. Call once after parsing.
    pub fn sort_regions(&mut self) {
        self.regions.sort_by(|a, b| a.name.cmp(&b.name).then(a.index.cmp(&b.index)));
    }

    pub fn is_empty(&self) -> bool {
        self.pages.is_empty()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn page() -> AtlasPage {
        AtlasPage { size: Some((100, 200)), ..AtlasPage::new("p.png") }
    }

    fn region(name: &str, index: i32) -> AtlasRegion {
        AtlasRegion {
            name: name.into(),
            page: AtlasPageId(0),
            xy: (10, 20),
            size: (30, 40),
            rotate_deg: 0,
            offset: (0, 0),
            original_size: (30, 40),
            index,
            splits: None,
            pads: None,
        }
    }

    #[test]
    fn unrotated_size_swaps_only_for_quarter_turns() {
        let mut r = region("a", -1);
        assert_eq!(r.unrotated_size(), (30, 40));
        r.rotate_deg = 90;
        assert_eq!(r.unrotated_size(), (40, 30));
        r.rotate_deg = 180;
        assert_eq!(r.unrotated_size(), (30, 40));
        r.rotate_deg = 270;
        assert_eq!(r.unrotated_size(), (40, 30));
    }

    #[test]
    fn uvs_map_pixels_to_normalised_page_space() {
        let uvs = region("a", -1).corner_uvs(&page()).unwrap();
        // x: 10..40 of 100, y: 20..60 of 200
        assert_eq!(uvs[0], Vec2::new(0.10, 0.30)); // bottom-left
        assert_eq!(uvs[1], Vec2::new(0.40, 0.30)); // bottom-right
        assert_eq!(uvs[2], Vec2::new(0.40, 0.10)); // top-right
        assert_eq!(uvs[3], Vec2::new(0.10, 0.10)); // top-left
    }

    #[test]
    fn rotation_permutes_the_corner_order() {
        let mut r = region("a", -1);
        let flat = r.corner_uvs(&page()).unwrap();
        r.rotate_deg = 90;
        let turned = r.corner_uvs(&page()).unwrap();
        assert_eq!(turned, [flat[1], flat[2], flat[3], flat[0]]);
    }

    #[test]
    fn uvs_require_a_known_page_size() {
        let sizeless = AtlasPage::new("p.png");
        assert!(region("a", -1).corner_uvs(&sizeless).is_none());
        let zero = AtlasPage { size: Some((0, 10)), ..AtlasPage::new("p.png") };
        assert!(region("a", -1).corner_uvs(&zero).is_none());
    }

    #[test]
    fn find_returns_the_lowest_index_of_a_sequence() {
        let mut atlas = Atlas { pages: vec![page()], regions: vec![] };
        atlas.regions.push(region("walk", 2));
        atlas.regions.push(region("walk", 0));
        atlas.regions.push(region("head", -1));
        atlas.regions.push(region("walk", 1));
        atlas.sort_regions();

        let id = atlas.find("walk").unwrap();
        assert_eq!(atlas.region(id).unwrap().index, 0);
        assert_eq!(atlas.region(atlas.find("head").unwrap()).unwrap().name, "head");
        assert!(atlas.find("nope").is_none());
    }

    #[test]
    fn sort_regions_orders_by_name_then_index() {
        let mut atlas = Atlas { pages: vec![page()], regions: vec![region("b", 1), region("a", 5), region("a", 1)] };
        atlas.sort_regions();
        let seen: Vec<_> = atlas.regions.iter().map(|r| (r.name.as_str(), r.index)).collect();
        assert_eq!(seen, vec![("a", 1), ("a", 5), ("b", 1)]);
    }
}
