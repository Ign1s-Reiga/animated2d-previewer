//! Orthographic 2D camera and viewport.
//!
//! Model space is Y-up, which is what both source ecosystems author in, and
//! wgpu's clip space is also Y-up — so no flip is needed anywhere and there is
//! no chance of one being applied twice.

use a2d_core::{Aabb, Vec2};

/// The surface being drawn into, in physical pixels.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Viewport {
    /// Physical width in pixels — already multiplied by the DPI scale factor.
    pub width: u32,
    /// Physical height in pixels.
    pub height: u32,
    /// Ratio of physical pixels to logical pixels. 2.0 on a typical HiDPI
    /// display. Keeping it separate from the pixel size is what lets a camera
    /// be specified in logical units and stay the same apparent size across
    /// displays.
    pub scale_factor: f32,
}

impl Viewport {
    pub fn new(width: u32, height: u32) -> Viewport {
        Viewport { width, height, scale_factor: 1.0 }
    }

    pub fn with_scale_factor(mut self, scale_factor: f32) -> Viewport {
        self.scale_factor = scale_factor;
        self
    }

    /// True when the viewport has no area, in which case there is nothing to
    /// draw and every derived value would divide by zero.
    ///
    /// A NaN or infinite scale factor counts as degenerate: it would otherwise
    /// propagate through the whole transform and collapse clip space silently.
    pub fn is_degenerate(&self) -> bool {
        self.width == 0 || self.height == 0 || self.scale_factor <= 0.0 || !self.scale_factor.is_finite()
    }

    pub fn aspect_ratio(&self) -> f32 {
        if self.is_degenerate() {
            1.0
        } else {
            self.width as f32 / self.height as f32
        }
    }
}

/// Where the camera is looking and how much it magnifies.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Camera {
    /// Model-space point that lands at the centre of the viewport.
    pub center: Vec2,
    /// Logical pixels per model unit. Multiplied by the viewport's scale factor
    /// internally, so a value of 1.0 looks the same size on any display.
    pub pixels_per_unit: f32,
}

impl Default for Camera {
    fn default() -> Self {
        Camera { center: Vec2::ZERO, pixels_per_unit: 1.0 }
    }
}

impl Camera {
    pub fn new(center: Vec2, pixels_per_unit: f32) -> Camera {
        Camera { center, pixels_per_unit }
    }

    /// Frames `bounds` inside `viewport`, leaving `margin` of the smaller axis
    /// as padding.
    ///
    /// `margin` is a fraction: 0.1 leaves 10% of the fitted axis empty. An
    /// empty or degenerate box falls back to the default camera rather than
    /// producing an infinite zoom.
    pub fn fit(bounds: Aabb, viewport: Viewport, margin: f32) -> Camera {
        if bounds.is_empty() || viewport.is_degenerate() {
            return Camera::default();
        }
        let size = bounds.size();
        let usable = (1.0 - margin.clamp(0.0, 0.9)).max(0.01);
        // Logical pixels available on each axis.
        let logical_w = viewport.width as f32 / viewport.scale_factor;
        let logical_h = viewport.height as f32 / viewport.scale_factor;

        // A zero-extent axis must not drive the fit; a flat model still frames
        // by its other axis.
        let fit_x = if size.x > 1e-6 { logical_w * usable / size.x } else { f32::INFINITY };
        let fit_y = if size.y > 1e-6 { logical_h * usable / size.y } else { f32::INFINITY };
        let ppu = fit_x.min(fit_y);
        let ppu = if ppu.is_finite() && ppu > 0.0 { ppu } else { 1.0 };

        Camera { center: bounds.center(), pixels_per_unit: ppu }
    }

    /// The `(scale_x, scale_y, translate_x, translate_y)` the shader applies as
    /// `position * scale + translate`.
    pub fn transform(&self, viewport: Viewport) -> [f32; 4] {
        if viewport.is_degenerate() || self.pixels_per_unit <= 0.0 || !self.pixels_per_unit.is_finite() {
            // A degenerate viewport draws nothing; a zero transform collapses
            // every triangle rather than emitting NaNs into clip space.
            return [0.0, 0.0, 0.0, 0.0];
        }
        let ppu = self.pixels_per_unit * viewport.scale_factor;
        let sx = ppu * 2.0 / viewport.width as f32;
        let sy = ppu * 2.0 / viewport.height as f32;
        [sx, sy, -self.center.x * sx, -self.center.y * sy]
    }

    /// Maps a model-space point to normalised device coordinates.
    ///
    /// Used by tests to predict where geometry lands, and by hit testing in
    /// reverse via [`Camera::ndc_to_model`].
    pub fn model_to_ndc(&self, point: Vec2, viewport: Viewport) -> Vec2 {
        let t = self.transform(viewport);
        Vec2::new(point.x * t[0] + t[2], point.y * t[1] + t[3])
    }

    /// Maps normalised device coordinates back to model space.
    ///
    /// Returns `None` when the transform is degenerate and has no inverse.
    pub fn ndc_to_model(&self, ndc: Vec2, viewport: Viewport) -> Option<Vec2> {
        let t = self.transform(viewport);
        if t[0].abs() < 1e-12 || t[1].abs() < 1e-12 {
            return None;
        }
        Some(Vec2::new((ndc.x - t[2]) / t[0], (ndc.y - t[3]) / t[1]))
    }

    /// Maps a physical pixel position (origin top-left, Y down, as every window
    /// system reports it) to model space.
    pub fn screen_to_model(&self, x: f32, y: f32, viewport: Viewport) -> Option<Vec2> {
        if viewport.is_degenerate() {
            return None;
        }
        let ndc = Vec2::new(
            x / viewport.width as f32 * 2.0 - 1.0,
            // Screen Y grows downwards, clip space Y grows upwards.
            1.0 - y / viewport.height as f32 * 2.0,
        );
        self.ndc_to_model(ndc, viewport)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn close(a: f32, b: f32) -> bool {
        (a - b).abs() < 1e-4
    }

    fn assert_close2(a: Vec2, b: Vec2) {
        assert!(close(a.x, b.x) && close(a.y, b.y), "{a:?} != {b:?}");
    }

    #[test]
    fn the_camera_centre_lands_at_the_middle_of_the_viewport() {
        let vp = Viewport::new(800, 600);
        let cam = Camera::new(Vec2::new(10.0, 20.0), 1.0);
        assert_close2(cam.model_to_ndc(Vec2::new(10.0, 20.0), vp), Vec2::ZERO);
    }

    #[test]
    fn one_pixel_per_unit_maps_units_to_pixels() {
        let vp = Viewport::new(800, 600);
        let cam = Camera::new(Vec2::ZERO, 1.0);
        // Half the width in model units is the right edge of clip space.
        assert_close2(cam.model_to_ndc(Vec2::new(400.0, 0.0), vp), Vec2::new(1.0, 0.0));
        assert_close2(cam.model_to_ndc(Vec2::new(0.0, 300.0), vp), Vec2::new(0.0, 1.0));
    }

    #[test]
    fn model_space_is_y_up_in_clip_space_too() {
        let vp = Viewport::new(100, 100);
        let cam = Camera::new(Vec2::ZERO, 1.0);
        assert!(cam.model_to_ndc(Vec2::new(0.0, 10.0), vp).y > 0.0, "up in model must be up in clip");
    }

    #[test]
    fn zooming_magnifies_about_the_centre() {
        let vp = Viewport::new(800, 600);
        let one = Camera::new(Vec2::ZERO, 1.0);
        let two = Camera::new(Vec2::ZERO, 2.0);
        let p = Vec2::new(100.0, 0.0);
        assert!(close(two.model_to_ndc(p, vp).x, one.model_to_ndc(p, vp).x * 2.0));
    }

    #[test]
    fn the_dpi_scale_factor_keeps_apparent_size_constant() {
        // The same camera on a 2x display with twice the pixels must place
        // geometry at the same normalised position.
        let standard = Viewport::new(800, 600);
        let hidpi = Viewport::new(1600, 1200).with_scale_factor(2.0);
        let cam = Camera::new(Vec2::ZERO, 1.5);
        let p = Vec2::new(120.0, -60.0);
        assert_close2(cam.model_to_ndc(p, standard), cam.model_to_ndc(p, hidpi));
    }

    #[test]
    fn ndc_round_trips_back_to_model_space() {
        let vp = Viewport::new(1280, 800).with_scale_factor(1.25);
        let cam = Camera::new(Vec2::new(-30.0, 45.0), 3.0);
        let p = Vec2::new(12.5, -7.25);
        assert_close2(cam.ndc_to_model(cam.model_to_ndc(p, vp), vp).unwrap(), p);
    }

    #[test]
    fn screen_pixels_map_with_y_flipped() {
        let vp = Viewport::new(800, 600);
        let cam = Camera::new(Vec2::ZERO, 1.0);
        // The centre pixel is the camera centre.
        assert_close2(cam.screen_to_model(400.0, 300.0, vp).unwrap(), Vec2::ZERO);
        // The top of the screen is positive Y in model space.
        assert!(cam.screen_to_model(400.0, 0.0, vp).unwrap().y > 0.0);
        assert!(cam.screen_to_model(400.0, 600.0, vp).unwrap().y < 0.0);
    }

    #[test]
    fn fit_frames_the_bounds_with_the_requested_margin() {
        let vp = Viewport::new(800, 400);
        let bounds = Aabb::new(Vec2::new(-50.0, -100.0), Vec2::new(50.0, 100.0));
        let cam = Camera::fit(bounds, vp, 0.0);
        assert_close2(cam.center, Vec2::ZERO);
        // Height is the binding axis: 200 units into 400 pixels.
        assert!(close(cam.pixels_per_unit, 2.0), "got {}", cam.pixels_per_unit);
        // The extremes land exactly on the clip-space edges.
        assert!(close(cam.model_to_ndc(Vec2::new(0.0, 100.0), vp).y, 1.0));
    }

    #[test]
    fn fit_leaves_a_margin_when_asked() {
        let vp = Viewport::new(400, 400);
        let bounds = Aabb::new(Vec2::new(-50.0, -50.0), Vec2::new(50.0, 50.0));
        let tight = Camera::fit(bounds, vp, 0.0);
        let padded = Camera::fit(bounds, vp, 0.2);
        assert!(padded.pixels_per_unit < tight.pixels_per_unit);
        assert!(close(cam_edge(padded, vp), 0.8), "got {}", cam_edge(padded, vp));
    }

    fn cam_edge(cam: Camera, vp: Viewport) -> f32 {
        cam.model_to_ndc(Vec2::new(0.0, 50.0), vp).y
    }

    #[test]
    fn fit_recentres_on_an_off_centre_model() {
        let vp = Viewport::new(400, 400);
        let bounds = Aabb::new(Vec2::new(100.0, 200.0), Vec2::new(200.0, 300.0));
        let cam = Camera::fit(bounds, vp, 0.0);
        assert_close2(cam.center, Vec2::new(150.0, 250.0));
        assert_close2(cam.model_to_ndc(Vec2::new(150.0, 250.0), vp), Vec2::ZERO);
    }

    #[test]
    fn fit_on_an_empty_box_falls_back_rather_than_dividing_by_zero() {
        let cam = Camera::fit(Aabb::EMPTY, Viewport::new(800, 600), 0.1);
        assert_eq!(cam, Camera::default());
        assert!(cam.pixels_per_unit.is_finite());
    }

    #[test]
    fn fit_on_a_flat_model_frames_by_its_other_axis() {
        let vp = Viewport::new(400, 400);
        // Zero height: only the horizontal extent can drive the zoom.
        let bounds = Aabb::new(Vec2::new(-100.0, 5.0), Vec2::new(100.0, 5.0));
        let cam = Camera::fit(bounds, vp, 0.0);
        assert!(cam.pixels_per_unit.is_finite(), "got {}", cam.pixels_per_unit);
        assert!(close(cam.pixels_per_unit, 2.0), "got {}", cam.pixels_per_unit);
    }

    #[test]
    fn a_degenerate_viewport_produces_a_collapsed_transform_not_nans() {
        for vp in [Viewport::new(0, 600), Viewport::new(800, 0), Viewport::new(800, 600).with_scale_factor(0.0)] {
            let t = Camera::default().transform(vp);
            assert!(t.iter().all(|v| v.is_finite()), "{t:?}");
            assert_eq!(t, [0.0; 4]);
            assert!(Camera::default().screen_to_model(1.0, 1.0, vp).is_none());
        }
    }

    #[test]
    fn a_zero_zoom_camera_does_not_produce_nans() {
        let vp = Viewport::new(800, 600);
        let cam = Camera::new(Vec2::ZERO, 0.0);
        assert_eq!(cam.transform(vp), [0.0; 4]);
        assert!(cam.ndc_to_model(Vec2::ZERO, vp).is_none());
    }

    #[test]
    fn aspect_ratio_is_reported_and_is_safe_when_degenerate() {
        assert!(close(Viewport::new(800, 400).aspect_ratio(), 2.0));
        assert!(close(Viewport::new(0, 0).aspect_ratio(), 1.0));
    }
}
