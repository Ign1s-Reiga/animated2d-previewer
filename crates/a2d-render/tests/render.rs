//! Rendering tests that run on a real GPU.
//!
//! These render into an offscreen target and read the pixels back, which is the
//! same path a visual regression test takes (spec §17.3). Where no adapter is
//! available they skip with a note rather than failing, so the suite still
//! passes on a headless box — set `A2D_REQUIRE_GPU=1` to turn that skip into a
//! failure, which is what CI on a machine that *should* have a GPU wants.

use a2d_core::{BlendMode, MaskId, RenderList, RenderMesh, Rgba, TextureId, Vec2};
use a2d_render::{Camera, FrameSettings, GpuContext, OffscreenTarget, Renderer, Rgba8Image, SamplerConfig, Viewport};

const SIZE: u32 = 64;

/// Acquires a GPU, or returns `None` after explaining why.
fn gpu() -> Option<GpuContext> {
    match GpuContext::headless() {
        Ok(gpu) => Some(gpu),
        Err(e) => {
            if std::env::var("A2D_REQUIRE_GPU").is_ok() {
                panic!("A2D_REQUIRE_GPU is set, but no GPU adapter is available: {e}");
            }
            eprintln!("skipping GPU test: {e}");
            None
        }
    }
}

/// A renderer with one opaque white 1x1 page uploaded as texture 0.
///
/// White is the identity for tinting, so anything the tests observe comes from
/// the colour and blend path rather than from the texture.
fn setup(gpu: &GpuContext) -> (Renderer, OffscreenTarget) {
    let mut renderer = Renderer::new(gpu.clone());
    let white = Rgba8Image::solid(1, 1, [255, 255, 255, 255]);
    renderer
        .textures_mut()
        .upload(gpu, "white", &white, false, SamplerConfig::default())
        .expect("a 1x1 white page should upload");
    let target = OffscreenTarget::new(gpu, SIZE, SIZE).expect("target should be creatable");
    (renderer, target)
}

/// A camera mapping model units 1:1 onto pixels, origin at the bottom-left.
fn settings() -> FrameSettings {
    let viewport = Viewport::new(SIZE, SIZE);
    let camera = Camera::new(Vec2::new(SIZE as f32 / 2.0, SIZE as f32 / 2.0), 1.0);
    FrameSettings::new(viewport, camera)
}

/// An axis-aligned quad in model space, sampling the whole of its page.
fn quad(min: Vec2, max: Vec2, color: Rgba, blend: BlendMode) -> RenderMesh {
    RenderMesh {
        vertices: vec![min, Vec2::new(max.x, min.y), max, Vec2::new(min.x, max.y)],
        uvs: vec![Vec2::ZERO, Vec2::new(1.0, 0.0), Vec2::ONE, Vec2::new(0.0, 1.0)],
        indices: vec![0, 1, 2, 2, 3, 0],
        texture: TextureId(0),
        color,
        blend_mode: blend,
        ..Default::default()
    }
}

/// Reads a pixel. `x` and `y` are in image space, with y growing downwards.
fn pixel(image: &Rgba8Image, x: u32, y: u32) -> [u8; 4] {
    let at = ((y * image.width + x) * 4) as usize;
    [image.pixels[at], image.pixels[at + 1], image.pixels[at + 2], image.pixels[at + 3]]
}

/// Renders a list and reads the result back.
fn render(gpu: &GpuContext, renderer: &mut Renderer, target: &OffscreenTarget, list: &RenderList) -> Rgba8Image {
    renderer.render(target.view(), target.format(), settings(), list).expect("render should succeed");
    target.read_pixels(gpu).expect("read-back should succeed")
}

#[test]
fn a_transparent_clear_leaves_every_pixel_transparent() {
    let Some(gpu) = gpu() else { return };
    let (mut renderer, target) = setup(&gpu);
    let image = render(&gpu, &mut renderer, &target, &RenderList::new());

    assert_eq!(image.width, SIZE);
    assert_eq!(image.height, SIZE);
    assert!(image.pixels.chunks_exact(4).all(|p| p[3] == 0), "a transparent clear must leave alpha at zero");
}

#[test]
fn an_opaque_clear_fills_the_target() {
    let Some(gpu) = gpu() else { return };
    let (mut renderer, target) = setup(&gpu);
    let list = RenderList::new();
    renderer
        .render(target.view(), target.format(), settings().with_clear_color(Rgba::WHITE), &list)
        .expect("render should succeed");
    let image = target.read_pixels(&gpu).expect("read-back should succeed");

    assert!(image.pixels.chunks_exact(4).all(|p| p == [255, 255, 255, 255]), "clear should fill with white");
}

#[test]
fn an_opaque_quad_covers_exactly_where_it_was_placed() {
    let Some(gpu) = gpu() else { return };
    let (mut renderer, target) = setup(&gpu);

    // Left half of the viewport, in model units.
    let mut list = RenderList::new();
    list.push_mesh(quad(Vec2::ZERO, Vec2::new(32.0, 64.0), Rgba::WHITE, BlendMode::Normal));
    let image = render(&gpu, &mut renderer, &target, &list);

    assert_eq!(pixel(&image, 8, 32), [255, 255, 255, 255], "inside the quad");
    assert_eq!(pixel(&image, 56, 32), [0, 0, 0, 0], "outside the quad stays transparent");
}

#[test]
fn model_space_is_y_up_on_screen() {
    let Some(gpu) = gpu() else { return };
    let (mut renderer, target) = setup(&gpu);

    // The top half in model space (high Y) must land in the top rows of the
    // image, where y is low.
    let mut list = RenderList::new();
    list.push_mesh(quad(Vec2::new(0.0, 32.0), Vec2::new(64.0, 64.0), Rgba::WHITE, BlendMode::Normal));
    let image = render(&gpu, &mut renderer, &target, &list);

    assert_eq!(pixel(&image, 32, 8)[3], 255, "high model Y should be near the top of the image");
    assert_eq!(pixel(&image, 32, 56)[3], 0, "low model Y should be empty");
}

#[test]
fn a_tint_multiplies_the_texture() {
    let Some(gpu) = gpu() else { return };
    let (mut renderer, target) = setup(&gpu);

    let mut list = RenderList::new();
    list.push_mesh(quad(Vec2::ZERO, Vec2::new(64.0, 64.0), Rgba::new(1.0, 0.0, 0.0, 1.0), BlendMode::Normal));
    let image = render(&gpu, &mut renderer, &target, &list);

    let p = pixel(&image, 32, 32);
    assert_eq!(p, [255, 0, 0, 255], "a red tint on a white page should be red");
}

#[test]
fn half_alpha_over_nothing_keeps_its_colour_and_halves_its_alpha() {
    let Some(gpu) = gpu() else { return };
    let (mut renderer, target) = setup(&gpu);

    let mut list = RenderList::new();
    list.push_mesh(quad(Vec2::ZERO, Vec2::new(64.0, 64.0), Rgba::new(1.0, 1.0, 1.0, 0.5), BlendMode::Normal));
    let image = render(&gpu, &mut renderer, &target, &list);

    let p = pixel(&image, 32, 32);
    // Alpha is stored linearly, so half really is about half.
    assert!((120..=136).contains(&p[3]), "alpha should be about half, got {}", p[3]);
    // Read-back is straight alpha, so the colour is the one that was drawn --
    // white -- rather than the halved value the frame holds premultiplied.
    assert!(p[0] >= 250, "colour should come back white, got {}", p[0]);
}

#[test]
fn half_alpha_blends_towards_the_background() {
    let Some(gpu) = gpu() else { return };
    let (mut renderer, target) = setup(&gpu);

    // Over an opaque background there is something to blend *with*, so the
    // result is a genuine mid-tone rather than a partly transparent white.
    let mut list = RenderList::new();
    list.push_mesh(quad(Vec2::ZERO, Vec2::new(64.0, 64.0), Rgba::new(1.0, 1.0, 1.0, 0.5), BlendMode::Normal));
    renderer
        .render(target.view(), target.format(), settings().with_clear_color(Rgba::new(0.0, 0.0, 0.0, 1.0)), &list)
        .expect("render should succeed");
    let image = target.read_pixels(&gpu).expect("read-back should succeed");

    let p = pixel(&image, 32, 32);
    assert_eq!(p[3], 255, "an opaque background stays opaque");
    // Colour is sRGB-encoded, so linear 0.5 lands near 188 rather than 128.
    assert!(p[0] > 150 && p[0] < 220, "colour should be a blended mid-tone, got {}", p[0]);
}

#[test]
fn additive_blending_accumulates_and_leaves_the_background_transparent() {
    let Some(gpu) = gpu() else { return };
    let (mut renderer, target) = setup(&gpu);

    // Two overlapping half-brightness additive quads should reach full white
    // where they overlap.
    let dim = Rgba::new(0.5, 0.5, 0.5, 1.0);
    let mut list = RenderList::new();
    list.push_mesh(quad(Vec2::ZERO, Vec2::new(64.0, 64.0), dim, BlendMode::Additive));
    let mut second = quad(Vec2::ZERO, Vec2::new(32.0, 64.0), dim, BlendMode::Additive);
    second.z_order = 1;
    list.push_mesh(second);
    let image = render(&gpu, &mut renderer, &target, &list);

    // Additive adds no coverage of its own, so read-back carries the light it
    // contributed in the alpha channel -- the colour is already saturated where
    // it lands at all. Twice the light, twice the alpha.
    let once = pixel(&image, 48, 32);
    let twice = pixel(&image, 8, 32);
    assert!(twice[3] > once[3], "the overlap should be brighter: {twice:?} vs {once:?}");
    assert!(once[3] > 0, "a single additive draw must still be visible, got {once:?}");
}

/// A rig can be nothing but additive slots — a glow or an effect layer the
/// source composited over something else. Additive deliberately writes no
/// destination alpha, so such a rig fills the frame with colour the alpha
/// channel says is not there. Read-back has to resolve that, or the whole model
/// exports as a blank image.
#[test]
fn a_wholly_additive_model_does_not_read_back_blank() {
    let Some(gpu) = gpu() else { return };
    let (mut renderer, target) = setup(&gpu);

    let mut list = RenderList::new();
    list.push_mesh(quad(Vec2::ZERO, Vec2::new(32.0, 64.0), Rgba::WHITE, BlendMode::Additive));
    let image = render(&gpu, &mut renderer, &target, &list);

    assert_eq!(pixel(&image, 8, 32), [255, 255, 255, 255], "inside the glow");
    assert_eq!(pixel(&image, 56, 32), [0, 0, 0, 0], "outside it stays transparent");
}

/// Black contributes nothing to an additive draw however opaque the source is,
/// so it must not turn into an occluder. An effect texture whose alpha channel
/// is a solid silhouette would otherwise export a dark halo around its glow.
#[test]
fn additive_black_does_not_become_an_occluder() {
    let Some(gpu) = gpu() else { return };
    let (mut renderer, target) = setup(&gpu);

    let mut list = RenderList::new();
    let black = Rgba::new(0.0, 0.0, 0.0, 1.0);
    list.push_mesh(quad(Vec2::ZERO, Vec2::new(32.0, 64.0), black, BlendMode::Additive));
    let image = render(&gpu, &mut renderer, &target, &list);

    assert_eq!(pixel(&image, 8, 32), [0, 0, 0, 0], "black light must leave the frame clear");
}

#[test]
fn draw_order_decides_what_ends_up_on_top() {
    let Some(gpu) = gpu() else { return };
    let (mut renderer, target) = setup(&gpu);

    let mut list = RenderList::new();
    let mut under = quad(Vec2::ZERO, Vec2::new(64.0, 64.0), Rgba::new(1.0, 0.0, 0.0, 1.0), BlendMode::Normal);
    under.z_order = 0;
    let mut over = quad(Vec2::ZERO, Vec2::new(64.0, 64.0), Rgba::new(0.0, 1.0, 0.0, 1.0), BlendMode::Normal);
    over.z_order = 1;
    // Pushed out of order on purpose: z_order, not push order, must decide.
    list.push_mesh(over);
    list.push_mesh(under);
    let image = render(&gpu, &mut renderer, &target, &list);

    assert_eq!(pixel(&image, 32, 32), [0, 255, 0, 255], "the higher z_order should win");
}

#[test]
fn a_clipping_mask_limits_where_a_mesh_draws() {
    let Some(gpu) = gpu() else { return };
    let (mut renderer, target) = setup(&gpu);

    let mut list = RenderList::new();
    // Mask covering the left quarter.
    let mask =
        list.push_mask(vec![Vec2::new(0.0, 0.0), Vec2::new(16.0, 0.0), Vec2::new(16.0, 64.0), Vec2::new(0.0, 64.0)]);
    let mut masked = quad(Vec2::ZERO, Vec2::new(64.0, 64.0), Rgba::WHITE, BlendMode::Normal);
    masked.clipping_mask = Some(mask);
    list.push_mesh(masked);
    let image = render(&gpu, &mut renderer, &target, &list);

    assert_eq!(pixel(&image, 8, 32)[3], 255, "inside the mask should draw");
    assert_eq!(pixel(&image, 40, 32)[3], 0, "outside the mask should be clipped away");
}

#[test]
fn an_unmasked_mesh_after_a_masked_one_is_not_clipped() {
    let Some(gpu) = gpu() else { return };
    let (mut renderer, target) = setup(&gpu);

    let mut list = RenderList::new();
    let mask =
        list.push_mask(vec![Vec2::new(0.0, 0.0), Vec2::new(16.0, 0.0), Vec2::new(16.0, 64.0), Vec2::new(0.0, 64.0)]);
    let mut masked = quad(Vec2::ZERO, Vec2::new(64.0, 32.0), Rgba::WHITE, BlendMode::Normal);
    masked.clipping_mask = Some(mask);
    masked.z_order = 0;
    list.push_mesh(masked);

    // A later mesh with no mask must be unaffected by the earlier stencil.
    let mut free = quad(Vec2::new(0.0, 32.0), Vec2::new(64.0, 64.0), Rgba::WHITE, BlendMode::Normal);
    free.z_order = 1;
    list.push_mesh(free);

    let image = render(&gpu, &mut renderer, &target, &list);
    // Model y 32..64 is the *top* half of the image, so the unmasked mesh
    // occupies image rows 0..32.
    assert_eq!(pixel(&image, 40, 16)[3], 255, "the unmasked mesh should draw everywhere");
    assert_eq!(pixel(&image, 40, 48)[3], 0, "the masked mesh should still be clipped");
    assert_eq!(pixel(&image, 8, 48)[3], 255, "the masked mesh should draw inside its mask");
}

#[test]
fn a_concave_mask_fills_by_the_even_odd_rule() {
    let Some(gpu) = gpu() else { return };
    let (mut renderer, target) = setup(&gpu);

    // A C shape: the notch on the right must stay unfilled even though a naive
    // triangle fan would cover it.
    let mut list = RenderList::new();
    let mask = list.push_mask(vec![
        Vec2::new(8.0, 8.0),
        Vec2::new(56.0, 8.0),
        Vec2::new(56.0, 20.0),
        Vec2::new(20.0, 20.0),
        Vec2::new(20.0, 44.0),
        Vec2::new(56.0, 44.0),
        Vec2::new(56.0, 56.0),
        Vec2::new(8.0, 56.0),
    ]);
    let mut masked = quad(Vec2::ZERO, Vec2::new(64.0, 64.0), Rgba::WHITE, BlendMode::Normal);
    masked.clipping_mask = Some(mask);
    list.push_mesh(masked);
    let image = render(&gpu, &mut renderer, &target, &list);

    // Inside the C's spine (model x≈12, y≈32 -> image y≈32).
    assert_eq!(pixel(&image, 12, 32)[3], 255, "the spine of the C should be filled");
    // Inside the notch (model x≈40, y≈32).
    assert_eq!(pixel(&image, 40, 32)[3], 0, "the notch must stay unfilled");
    // Inside the top arm (model y≈50 -> image y≈14).
    assert_eq!(pixel(&image, 40, 14)[3], 255, "the arms of the C should be filled");
}

#[test]
fn rendering_the_same_list_twice_produces_identical_pixels() {
    // Determinism is what makes a framebuffer hash a usable regression signal.
    let Some(gpu) = gpu() else { return };
    let (mut renderer, target) = setup(&gpu);

    let mut list = RenderList::new();
    list.push_mesh(quad(Vec2::new(4.0, 4.0), Vec2::new(60.0, 44.0), Rgba::new(0.2, 0.6, 0.9, 0.8), BlendMode::Normal));

    let first = render(&gpu, &mut renderer, &target, &list);
    let second = render(&gpu, &mut renderer, &target, &list);
    assert_eq!(first.pixels, second.pixels);
}

#[test]
fn frame_stats_report_what_was_drawn() {
    let Some(gpu) = gpu() else { return };
    let (mut renderer, target) = setup(&gpu);

    let mut list = RenderList::new();
    let mask = list.push_mask(vec![Vec2::ZERO, Vec2::new(32.0, 0.0), Vec2::new(0.0, 32.0)]);
    let mut masked = quad(Vec2::ZERO, Vec2::new(64.0, 64.0), Rgba::WHITE, BlendMode::Normal);
    masked.clipping_mask = Some(mask);
    list.push_mesh(masked);
    list.push_mesh(quad(Vec2::ZERO, Vec2::new(64.0, 64.0), Rgba::WHITE, BlendMode::Normal));

    let stats = renderer.render(target.view(), target.format(), settings(), &list).expect("render should succeed");
    assert_eq!(stats.masks, 1);
    assert_eq!(stats.triangles, 2 + 2 + 1, "two quads plus the mask's single triangle");
    // Mask draw + masked batch + unmasked batch.
    assert_eq!(stats.draw_calls, 3);
    assert_eq!(stats.missing_textures, 0);
    assert_eq!(stats.skipped_meshes, 0);
}

#[test]
fn a_mesh_referencing_a_page_that_was_never_uploaded_is_counted_not_drawn() {
    let Some(gpu) = gpu() else { return };
    let mut renderer = Renderer::new(gpu.clone());
    // No pages uploaded at all, and no fallback installed.
    let target = OffscreenTarget::new(&gpu, SIZE, SIZE).expect("target should be creatable");

    let mut list = RenderList::new();
    list.push_mesh(quad(Vec2::ZERO, Vec2::new(64.0, 64.0), Rgba::WHITE, BlendMode::Normal));
    let stats = renderer
        .render(target.view(), target.format(), settings(), &list)
        .expect("a missing page must not fail the frame");

    assert_eq!(stats.missing_textures, 1);
    assert_eq!(stats.draw_calls, 0);
}

#[test]
fn a_fallback_page_stands_in_for_a_missing_one() {
    let Some(gpu) = gpu() else { return };
    let mut renderer = Renderer::new(gpu.clone());
    renderer.textures_mut().install_fallback(&gpu).expect("fallback should upload");
    let target = OffscreenTarget::new(&gpu, SIZE, SIZE).expect("target should be creatable");

    let mut list = RenderList::new();
    // Page 7 was never uploaded.
    let mut mesh = quad(Vec2::ZERO, Vec2::new(64.0, 64.0), Rgba::WHITE, BlendMode::Normal);
    mesh.texture = TextureId(7);
    list.push_mesh(mesh);

    let stats = renderer.render(target.view(), target.format(), settings(), &list).expect("render should succeed");
    assert_eq!(stats.missing_textures, 0, "the fallback should stand in");
    assert_eq!(stats.draw_calls, 1);

    let image = target.read_pixels(&gpu).expect("read-back should succeed");
    let p = pixel(&image, 32, 32);
    assert!(p[0] > 200 && p[1] < 60 && p[2] > 200, "the placeholder should be obviously magenta, got {p:?}");
}

#[test]
fn a_degenerate_viewport_is_refused_rather_than_dividing_by_zero() {
    let Some(gpu) = gpu() else { return };
    let (mut renderer, target) = setup(&gpu);
    let mut settings = settings();
    settings.viewport = Viewport::new(0, 0);

    let err = renderer
        .render(target.view(), target.format(), settings, &RenderList::new())
        .expect_err("a zero-area viewport should be refused");
    assert!(err.to_string().contains("no area"), "{err}");
}

#[test]
fn resizing_the_target_between_frames_works() {
    let Some(gpu) = gpu() else { return };
    let (mut renderer, _) = setup(&gpu);

    for size in [16u32, 128, 64] {
        let target = OffscreenTarget::new(&gpu, size, size).expect("target should be creatable");
        let viewport = Viewport::new(size, size);
        let camera = Camera::new(Vec2::new(size as f32 / 2.0, size as f32 / 2.0), 1.0);
        let mut list = RenderList::new();
        list.push_mesh(quad(Vec2::ZERO, Vec2::new(size as f32, size as f32), Rgba::WHITE, BlendMode::Normal));

        renderer
            .render(target.view(), target.format(), FrameSettings::new(viewport, camera), &list)
            .expect("render should succeed");
        let image = target.read_pixels(&gpu).expect("read-back should succeed");
        assert_eq!(image.width, size);
        assert_eq!(pixel(&image, size / 2, size / 2)[3], 255, "size {size}");
    }
}

#[test]
fn a_growing_mesh_count_reallocates_buffers_without_corruption() {
    let Some(gpu) = gpu() else { return };
    let (mut renderer, target) = setup(&gpu);

    // Start small, then jump past the initial buffer capacity.
    for count in [1usize, 4, 400, 2] {
        let mut list = RenderList::new();
        for i in 0..count {
            let x = (i % 8) as f32 * 8.0;
            let mut mesh = quad(Vec2::new(x, 0.0), Vec2::new(x + 8.0, 64.0), Rgba::WHITE, BlendMode::Normal);
            mesh.z_order = i as u32;
            list.push_mesh(mesh);
        }
        let image = render(&gpu, &mut renderer, &target, &list);
        assert_eq!(pixel(&image, 4, 32)[3], 255, "count {count}");
    }
}

#[test]
fn masks_do_not_leak_between_frames() {
    let Some(gpu) = gpu() else { return };
    let (mut renderer, target) = setup(&gpu);

    let mut masked_list = RenderList::new();
    let mask = masked_list.push_mask(vec![
        Vec2::new(0.0, 0.0),
        Vec2::new(16.0, 0.0),
        Vec2::new(16.0, 64.0),
        Vec2::new(0.0, 64.0),
    ]);
    let mut masked = quad(Vec2::ZERO, Vec2::new(64.0, 64.0), Rgba::WHITE, BlendMode::Normal);
    masked.clipping_mask = Some(mask);
    masked_list.push_mesh(masked);
    render(&gpu, &mut renderer, &target, &masked_list);

    // The next frame has no mask at all; the stencil from before must not
    // still be clipping it.
    let mut plain = RenderList::new();
    plain.push_mesh(quad(Vec2::ZERO, Vec2::new(64.0, 64.0), Rgba::WHITE, BlendMode::Normal));
    let image = render(&gpu, &mut renderer, &target, &plain);
    assert_eq!(pixel(&image, 40, 32)[3], 255, "last frame's mask must not persist");
}

#[test]
fn a_mask_id_that_does_not_exist_draws_nothing_rather_than_everything() {
    let Some(gpu) = gpu() else { return };
    let (mut renderer, target) = setup(&gpu);

    let mut list = RenderList::new();
    let mut mesh = quad(Vec2::ZERO, Vec2::new(64.0, 64.0), Rgba::WHITE, BlendMode::Normal);
    // No masks were registered, so this handle is dangling.
    mesh.clipping_mask = Some(MaskId(3));
    list.push_mesh(mesh);
    let image = render(&gpu, &mut renderer, &target, &list);

    // With no mask drawn the stencil stays zero, and the NotEqual test rejects
    // everything. Clipping to nothing is the safe reading of a broken handle.
    assert_eq!(pixel(&image, 32, 32)[3], 0, "a dangling mask should clip everything away");
}
