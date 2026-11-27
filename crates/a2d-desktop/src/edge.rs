//! Which part of the window's border, if any, the pointer is on.
//!
//! The window has no frame, so there is nothing to grab to resize it. Instead a
//! thin band along each edge (and a larger square at each corner) turns a press
//! into a native resize. The decision is a pure function of the pointer, the
//! window size and the scale factor, so it is tested without a window.
//!
//! Precedence, written down because it is where "my mascot is stuck" comes from:
//!
//! 1. click-through on: the window is inert; there is no zone, no cursor, no tint.
//! 2. click-through off, pointer in a zone: a press resizes and does NOT drag.
//! 3. click-through off, pointer elsewhere: a press drags the character.
//!
//! If the platform refuses a native resize, zones are switched off for the
//! rest of the run and a press drags, so the border is never a dead spot.

use a2d_core::Rgba;

/// Width of the band along each edge, in logical pixels.
pub const EDGE_LOGICAL: f64 = 8.0;
/// Side of the square at each corner, in logical pixels. Corners win over edges.
pub const CORNER_LOGICAL: f64 = 16.0;
/// The faint fill shown while the pointer is over the window, premultiplied:
/// grey 0.5 at 0.12 alpha.
pub const HOVER_TINT: Rgba = Rgba { r: 0.06, g: 0.06, b: 0.06, a: 0.12 };

/// A border zone, named by the side or corner it is on.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum EdgeZone {
    North,
    South,
    East,
    West,
    NorthEast,
    NorthWest,
    SouthEast,
    SouthWest,
}

/// The zone under `cursor`, or `None` for the interior, a pointer outside the
/// window, an inert window, or any non-finite or degenerate input.
///
/// `cursor` and `size` are physical pixels from the client area's top-left.
/// Windows reports whole-pixel client positions `0..w-1`, so distance from the
/// far side is measured from the last pixel (`w - 1 - x`): each band is then
/// the same number of pixels on every side. A zone covers positions strictly
/// closer to a side than its width, so a pointer exactly on the inner boundary
/// is interior. Widths are capped by the window's smaller side (edge a
/// quarter, corner a third) so an interior always remains on a tiny window.
///
/// `inert` is true while click-through is on or resizing by dragging is known
/// to be unsupported: either way there is no zone.
pub fn edge_zone(cursor: (f64, f64), size: (u32, u32), scale_factor: f64, inert: bool) -> Option<EdgeZone> {
    if inert {
        return None;
    }
    let (x, y) = cursor;
    if !x.is_finite() || !y.is_finite() || !scale_factor.is_finite() || scale_factor <= 0.0 {
        return None;
    }
    let (w, h) = (f64::from(size.0), f64::from(size.1));
    if w <= 0.0 || h <= 0.0 || x < 0.0 || y < 0.0 || x >= w || y >= h {
        return None;
    }
    let smaller = w.min(h);
    let edge = (EDGE_LOGICAL * scale_factor).min(smaller / 4.0);
    let corner = (CORNER_LOGICAL * scale_factor).min(smaller / 3.0).max(edge);

    let (from_right, from_bottom) = (w - 1.0 - x, h - 1.0 - y);
    let (left, right) = (x < corner, from_right < corner);
    let (top, bottom) = (y < corner, from_bottom < corner);
    match (left, right, top, bottom) {
        (true, _, true, _) => return Some(EdgeZone::NorthWest),
        (_, true, true, _) => return Some(EdgeZone::NorthEast),
        (true, _, _, true) => return Some(EdgeZone::SouthWest),
        (_, true, _, true) => return Some(EdgeZone::SouthEast),
        _ => {}
    }
    if x < edge {
        Some(EdgeZone::West)
    } else if from_right < edge {
        Some(EdgeZone::East)
    } else if y < edge {
        Some(EdgeZone::North)
    } else if from_bottom < edge {
        Some(EdgeZone::South)
    } else {
        None
    }
}

/// The logical size to remember for a window of `physical` pixels at
/// `scale_factor`: the size that, handed back as a logical size (which winit
/// rounds to whole physical pixels), reopens the same window. Non-finite or
/// non-positive scale factors count as 1.
pub fn logical_size(physical: (u32, u32), scale_factor: f64) -> (u32, u32) {
    let scale = if scale_factor.is_finite() && scale_factor > 0.0 { scale_factor } else { 1.0 };
    let to = |p: u32| (f64::from(p) / scale).round() as u32;
    (to(physical.0), to(physical.1))
}

/// The largest inner size, in physical pixels, a window may be dragged to: the
/// configured logical maximum at this scale, but never more than the device
/// can make a texture, since the surface is as big as the window. Never below
/// the minimum at this scale.
pub fn max_physical_size(max_logical: u32, min_logical: u32, scale_factor: f64, max_texture: u32) -> u32 {
    let scale = if scale_factor.is_finite() && scale_factor > 0.0 { scale_factor } else { 1.0 };
    let min = (f64::from(min_logical) * scale).ceil().min(f64::from(u32::MAX)) as u32;
    let wanted = (f64::from(max_logical) * scale).floor().min(f64::from(u32::MAX)) as u32;
    wanted.min(max_texture).max(min)
}

/// Whether `point` lies inside the rectangle at `origin` of `size`, all in
/// physical screen pixels. Right and bottom edges are exclusive.
pub fn point_in_rect(point: (i32, i32), origin: (i32, i32), size: (u32, u32)) -> bool {
    let (dx, dy) = (i64::from(point.0) - i64::from(origin.0), i64::from(point.1) - i64::from(origin.1));
    dx >= 0 && dy >= 0 && dx < i64::from(size.0) && dy < i64::from(size.1)
}

/// The clear colour for a frame: the faint tint while the pointer is over a
/// visible, interactive window, otherwise fully transparent.
///
/// [`HOVER_TINT`] is premultiplied, which is only right for a premultiplied
/// surface; a post-multiplied one gets the straight colour, and an opaque one
/// no tint (it would be a solid grey rectangle).
pub fn clear_color(hovered: bool, click_through: bool, hidden: bool, alpha: wgpu::CompositeAlphaMode) -> Rgba {
    if !hovered || click_through || hidden {
        return Rgba::TRANSPARENT;
    }
    match alpha {
        wgpu::CompositeAlphaMode::PreMultiplied => HOVER_TINT,
        wgpu::CompositeAlphaMode::PostMultiplied => {
            let a = HOVER_TINT.a;
            Rgba { r: HOVER_TINT.r / a, g: HOVER_TINT.g / a, b: HOVER_TINT.b / a, a }
        }
        _ => Rgba::TRANSPARENT,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const SIZE: (u32, u32) = (400, 300);

    fn zone(x: f64, y: f64) -> Option<EdgeZone> {
        edge_zone((x, y), SIZE, 1.0, false)
    }

    #[test]
    fn each_edge_is_recognised() {
        assert_eq!(zone(2.0, 150.0), Some(EdgeZone::West));
        assert_eq!(zone(397.0, 150.0), Some(EdgeZone::East));
        assert_eq!(zone(200.0, 2.0), Some(EdgeZone::North));
        assert_eq!(zone(200.0, 297.0), Some(EdgeZone::South));
    }

    #[test]
    fn each_corner_is_recognised() {
        assert_eq!(zone(2.0, 2.0), Some(EdgeZone::NorthWest));
        assert_eq!(zone(397.0, 2.0), Some(EdgeZone::NorthEast));
        assert_eq!(zone(2.0, 297.0), Some(EdgeZone::SouthWest));
        assert_eq!(zone(397.0, 297.0), Some(EdgeZone::SouthEast));
    }

    #[test]
    fn corners_win_over_edges() {
        assert_eq!(zone(12.0, 2.0), Some(EdgeZone::NorthWest));
        assert_eq!(zone(2.0, 12.0), Some(EdgeZone::NorthWest));
        assert_eq!(zone(20.0, 2.0), Some(EdgeZone::North));
    }

    #[test]
    fn the_interior_is_not_a_zone() {
        assert_eq!(zone(200.0, 150.0), None);
        assert_eq!(zone(50.0, 50.0), None);
    }

    #[test]
    fn whole_pixel_boundaries_are_the_same_width_on_every_side() {
        // 400 x 300: pixels 0..=399 and 0..=299; each edge band is 8 pixels.
        assert_eq!(zone(7.0, 150.0), Some(EdgeZone::West));
        assert_eq!(zone(8.0, 150.0), None);
        assert_eq!(zone(391.0, 150.0), None);
        assert_eq!(zone(392.0, 150.0), Some(EdgeZone::East));
        assert_eq!(zone(399.0, 150.0), Some(EdgeZone::East));
        assert_eq!(zone(200.0, 7.0), Some(EdgeZone::North));
        assert_eq!(zone(200.0, 8.0), None);
        assert_eq!(zone(200.0, 291.0), None);
        assert_eq!(zone(200.0, 292.0), Some(EdgeZone::South));
        assert_eq!(zone(200.0, 299.0), Some(EdgeZone::South));
        // Corners are 16 pixels square on every corner.
        assert_eq!(zone(15.0, 15.0), Some(EdgeZone::NorthWest));
        assert_eq!(zone(16.0, 4.0), Some(EdgeZone::North));
        assert_eq!(zone(4.0, 16.0), Some(EdgeZone::West));
        assert_eq!(zone(384.0, 15.0), Some(EdgeZone::NorthEast));
        assert_eq!(zone(383.0, 4.0), Some(EdgeZone::North));
        assert_eq!(zone(395.0, 16.0), Some(EdgeZone::East));
        assert_eq!(zone(15.0, 284.0), Some(EdgeZone::SouthWest));
        assert_eq!(zone(16.0, 295.0), Some(EdgeZone::South));
        assert_eq!(zone(4.0, 283.0), Some(EdgeZone::West));
        assert_eq!(zone(384.0, 284.0), Some(EdgeZone::SouthEast));
        assert_eq!(zone(383.0, 295.0), Some(EdgeZone::South));
        assert_eq!(zone(395.0, 283.0), Some(EdgeZone::East));
        // Fractions just inside the last pixel still count.
        assert_eq!(zone(399.9, 150.0), Some(EdgeZone::East));
    }

    #[test]
    fn positions_outside_the_window_are_no_zone() {
        assert_eq!(zone(400.0, 150.0), None);
        assert_eq!(zone(200.0, 300.0), None);
        assert_eq!(zone(-0.5, 150.0), None);
        assert_eq!(zone(200.0, -1.0), None);
        assert_eq!(zone(0.0, 0.0), Some(EdgeZone::NorthWest));
    }

    #[test]
    fn a_tiny_window_keeps_an_interior() {
        // 24 x 24: the edge is capped to 6 and the corner to 8.
        let size = (24, 24);
        assert_eq!(edge_zone((12.0, 12.0), size, 1.0, false), None);
        assert_eq!(edge_zone((1.0, 1.0), size, 1.0, false), Some(EdgeZone::NorthWest));
        assert_eq!(edge_zone((12.0, 1.0), size, 1.0, false), Some(EdgeZone::North));
        // A large scale factor cannot swallow the window either.
        for scale in [1.0, 1.5, 3.0, 10.0] {
            for s in [8u32, 9, 24, 64] {
                let centre = f64::from(s - 1) / 2.0;
                assert_eq!(edge_zone((centre, centre), (s, s), scale, false), None, "size {s} scale {scale}");
            }
        }
    }

    #[test]
    fn the_zone_scales_with_the_scale_factor() {
        // At 1.5 the band is 12 physical px and the corner 24.
        let at = |x: f64, y: f64| edge_zone((x, y), SIZE, 1.5, false);
        assert_eq!(at(11.0, 150.0), Some(EdgeZone::West));
        assert_eq!(at(12.0, 150.0), None);
        assert_eq!(at(387.0, 150.0), None);
        assert_eq!(at(388.0, 150.0), Some(EdgeZone::East));
        assert_eq!(at(23.0, 5.0), Some(EdgeZone::NorthWest));
        assert_eq!(at(24.0, 5.0), Some(EdgeZone::North));
        // At 1.0 the same 12 px is already interior.
        assert_eq!(edge_zone((12.0, 150.0), SIZE, 1.0, false), None);
    }

    #[test]
    fn non_finite_or_degenerate_input_is_no_zone() {
        for bad in [f64::NAN, f64::INFINITY, f64::NEG_INFINITY] {
            assert_eq!(edge_zone((bad, 2.0), SIZE, 1.0, false), None);
            assert_eq!(edge_zone((2.0, bad), SIZE, 1.0, false), None);
            assert_eq!(edge_zone((2.0, 2.0), SIZE, bad, false), None);
        }
        assert_eq!(edge_zone((2.0, 2.0), SIZE, 0.0, false), None);
        assert_eq!(edge_zone((2.0, 2.0), SIZE, -1.0, false), None);
        assert_eq!(edge_zone((0.0, 0.0), (0, 0), 1.0, false), None);
        assert_eq!(edge_zone((0.0, 0.0), (0, 10), 1.0, false), None);
    }

    #[test]
    fn an_inert_window_has_no_zone() {
        // Inert covers click-through and an unsupported native resize alike.
        for (x, y) in [(2.0, 150.0), (2.0, 2.0), (397.0, 297.0), (200.0, 2.0)] {
            assert!(edge_zone((x, y), SIZE, 1.0, false).is_some());
            assert_eq!(edge_zone((x, y), SIZE, 1.0, true), None);
        }
    }

    #[test]
    fn a_saved_logical_size_reopens_to_the_same_window() {
        for scale in [1.0, 1.25, 1.5, 1.75] {
            for physical in 64u32..4000 {
                let logical = logical_size((physical, physical), scale).0;
                // What winit makes of a logical size, and what is saved next.
                let reopened = (f64::from(logical) * scale).round() as u32;
                assert_eq!(logical_size((reopened, reopened), scale).0, logical, "{physical} at {scale}");
            }
        }
        assert_eq!(logical_size((600, 900), 1.5), (400, 600));
        assert_eq!(logical_size((480, 640), 1.0), (480, 640));
        assert_eq!(logical_size((480, 640), f64::NAN), (480, 640));
    }

    #[test]
    fn the_maximum_size_never_exceeds_the_texture_limit_or_drops_below_the_minimum() {
        assert_eq!(max_physical_size(8192, 64, 1.0, 16384), 8192);
        assert_eq!(max_physical_size(8192, 64, 2.5, 16384), 16384, "20480 is past the device");
        assert_eq!(max_physical_size(8192, 64, 1.5, 16384), 12288);
        assert_eq!(max_physical_size(8192, 64, 1.0, 2048), 2048);
        assert_eq!(max_physical_size(8192, 64, 1.5, 10), 96, "never below the minimum");
        assert_eq!(max_physical_size(8192, 64, f64::NAN, 16384), 8192);
    }

    #[test]
    fn point_in_rect_excludes_the_far_edges() {
        assert!(point_in_rect((10, 20), (10, 20), (5, 5)));
        assert!(point_in_rect((14, 24), (10, 20), (5, 5)));
        assert!(!point_in_rect((15, 24), (10, 20), (5, 5)));
        assert!(!point_in_rect((9, 20), (10, 20), (5, 5)));
        assert!(point_in_rect((-5, -5), (-10, -10), (10, 10)));
        assert!(!point_in_rect((0, 0), (0, 0), (0, 0)));
    }

    #[test]
    fn the_tint_is_shown_only_while_hovering_a_visible_interactive_window() {
        let pre = wgpu::CompositeAlphaMode::PreMultiplied;
        assert_eq!(clear_color(true, false, false, pre), HOVER_TINT);
        assert_eq!(clear_color(false, false, false, pre), Rgba::TRANSPARENT);
        assert_eq!(clear_color(true, true, false, pre), Rgba::TRANSPARENT, "click-through");
        assert_eq!(clear_color(true, false, true, pre), Rgba::TRANSPARENT, "hidden run");
    }

    #[test]
    fn the_tint_follows_the_surface_alpha_mode() {
        use wgpu::CompositeAlphaMode as M;
        // Premultiplied: colour already scaled by alpha, so it never exceeds it.
        let pre = clear_color(true, false, false, M::PreMultiplied);
        assert!(pre.a > 0.0 && pre.a <= 0.15);
        assert!(pre.r <= pre.a && pre.g <= pre.a && pre.b <= pre.a);
        // Post-multiplied: the straight colour, which is the premultiplied one
        // divided back by alpha, so both composite to the same result.
        let post = clear_color(true, false, false, M::PostMultiplied);
        assert_eq!(post.a, pre.a);
        assert!((post.r * post.a - pre.r).abs() < 1e-6 && (post.g * post.a - pre.g).abs() < 1e-6);
        assert!(post.r > pre.r);
        // Opaque (or unknown): a tint would be a solid rectangle.
        assert_eq!(clear_color(true, false, false, M::Opaque), Rgba::TRANSPARENT);
        assert_eq!(clear_color(true, false, false, M::Inherit), Rgba::TRANSPARENT);
    }
}
