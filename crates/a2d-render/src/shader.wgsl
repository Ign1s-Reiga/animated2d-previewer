// Character shading.
//
// One fragment shader serves both tinting modes. Spine's two-colour tint
// formula degenerates exactly to a plain multiply when the dark colour is
// (0, 0, 0, 0), so a mesh without a dark tint needs no separate pipeline and
// no branch — it just carries zeroes.

struct Camera {
    // xy = scale, zw = translation. Model space maps to clip space as
    // `position * scale + translation`, which is all an orthographic 2D
    // camera needs.
    transform: vec4<f32>,
};

@group(0) @binding(0) var<uniform> camera: Camera;

@group(1) @binding(0) var page: texture_2d<f32>;
@group(1) @binding(1) var page_sampler: sampler;

struct VertexIn {
    @location(0) position: vec2<f32>,
    @location(1) uv: vec2<f32>,
    @location(2) light: vec4<f32>,
    @location(3) dark: vec4<f32>,
};

struct VertexOut {
    @builtin(position) clip: vec4<f32>,
    @location(0) uv: vec2<f32>,
    @location(1) light: vec4<f32>,
    @location(2) dark: vec4<f32>,
};

@vertex
fn vs_main(in: VertexIn) -> VertexOut {
    var out: VertexOut;
    out.clip = vec4<f32>(in.position * camera.transform.xy + camera.transform.zw, 0.0, 1.0);
    out.uv = in.uv;
    out.light = in.light;
    out.dark = in.dark;
    return out;
}

@fragment
fn fs_main(in: VertexOut) -> @location(0) vec4<f32> {
    let tex = textureSample(page, page_sampler, in.uv);
    // With dark = 0 this is `tex.rgb * light.rgb`; with a dark colour it lifts
    // the shadowed end of the ramp towards `dark.rgb`.
    let rgb = ((tex.a - 1.0) * in.dark.a + 1.0 - tex.rgb) * in.dark.rgb + tex.rgb * in.light.rgb;
    return vec4<f32>(rgb, tex.a * in.light.a);
}

// Clipping masks. Shares the vertex format so masks and meshes can live in one
// buffer; everything but the position is ignored. The pipeline writes no colour
// and only inverts stencil, which fills the polygon by the even-odd rule and so
// handles concave outlines without triangulating them.
@vertex
fn vs_mask(in: VertexIn) -> @builtin(position) vec4<f32> {
    return vec4<f32>(in.position * camera.transform.xy + camera.transform.zw, 0.0, 1.0);
}

@fragment
fn fs_mask() -> @location(0) vec4<f32> {
    return vec4<f32>(0.0, 0.0, 0.0, 0.0);
}
