//! Render pipelines, one per distinct piece of fixed-function state.
//!
//! Everything that varies per draw and *can* live in a buffer does — tint
//! travels per vertex, the camera lives in a uniform. What is left here is only
//! what the pipeline object genuinely has to encode: blend factors, stencil
//! state, and the target format.

use std::collections::HashMap;

use a2d_core::BlendMode;

use crate::batch::Vertex;
use crate::gpu::GpuContext;

/// Depth-stencil format used for clipping.
///
/// A stencil-only format would be a better fit, but `Depth24PlusStencil8` is
/// the one every backend is required to support, and the depth aspect costs
/// nothing here because depth writes and tests are both disabled.
pub const DEPTH_STENCIL_FORMAT: wgpu::TextureFormat = wgpu::TextureFormat::Depth24PlusStencil8;

/// Reference value the stencil test compares against. Masked geometry draws
/// where the stencil is *not* this, i.e. wherever the mask polygon covered.
pub const STENCIL_REFERENCE: u32 = 0;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
struct MeshKey {
    format: wgpu::TextureFormat,
    blend: BlendMode,
    premultiplied: bool,
    masked: bool,
}

/// Compiled pipelines, created on first use and kept for the renderer's life.
#[derive(Debug)]
pub struct Pipelines {
    shader: wgpu::ShaderModule,
    mesh_layout: wgpu::PipelineLayout,
    /// The mask pass binds no texture, so it needs a layout that does not
    /// declare one — a pipeline must set every group its layout declares.
    mask_layout: wgpu::PipelineLayout,
    mesh: HashMap<MeshKey, wgpu::RenderPipeline>,
    mask: HashMap<wgpu::TextureFormat, wgpu::RenderPipeline>,
}

impl Pipelines {
    pub(crate) fn new(
        gpu: &GpuContext,
        camera_layout: &wgpu::BindGroupLayout,
        texture_layout: &wgpu::BindGroupLayout,
    ) -> Pipelines {
        let shader = gpu.device.create_shader_module(wgpu::ShaderModuleDescriptor {
            label: Some("a2d character shader"),
            source: wgpu::ShaderSource::Wgsl(include_str!("shader.wgsl").into()),
        });
        let mesh_layout = gpu.device.create_pipeline_layout(&wgpu::PipelineLayoutDescriptor {
            label: Some("a2d mesh layout"),
            bind_group_layouts: &[camera_layout, texture_layout],
            push_constant_ranges: &[],
        });
        let mask_layout = gpu.device.create_pipeline_layout(&wgpu::PipelineLayoutDescriptor {
            label: Some("a2d mask layout"),
            bind_group_layouts: &[camera_layout],
            push_constant_ranges: &[],
        });
        Pipelines { shader, mesh_layout, mask_layout, mesh: HashMap::new(), mask: HashMap::new() }
    }

    /// Number of pipelines compiled so far. Used by tests to prove that state
    /// which should not require a new pipeline does not create one.
    pub fn compiled_count(&self) -> usize {
        self.mesh.len() + self.mask.len()
    }

    pub(crate) fn mesh(
        &mut self,
        gpu: &GpuContext,
        format: wgpu::TextureFormat,
        blend: BlendMode,
        premultiplied: bool,
        masked: bool,
    ) -> &wgpu::RenderPipeline {
        let key = MeshKey { format, blend, premultiplied, masked };
        self.mesh.entry(key).or_insert_with(|| {
            gpu.device.create_render_pipeline(&wgpu::RenderPipelineDescriptor {
                label: Some("a2d mesh pipeline"),
                layout: Some(&self.mesh_layout),
                vertex: wgpu::VertexState {
                    module: &self.shader,
                    entry_point: Some("vs_main"),
                    compilation_options: Default::default(),
                    buffers: &[Vertex::layout()],
                },
                fragment: Some(wgpu::FragmentState {
                    module: &self.shader,
                    entry_point: Some("fs_main"),
                    compilation_options: Default::default(),
                    targets: &[Some(wgpu::ColorTargetState {
                        format,
                        blend: Some(blend_state(blend, premultiplied)),
                        write_mask: wgpu::ColorWrites::ALL,
                    })],
                }),
                primitive: wgpu::PrimitiveState {
                    topology: wgpu::PrimitiveTopology::TriangleList,
                    // Character meshes are authored with mixed winding and are
                    // meant to be visible from both sides once deformed, so
                    // face culling would drop valid geometry.
                    cull_mode: None,
                    ..Default::default()
                },
                depth_stencil: Some(depth_stencil_state(if masked {
                    StencilUse::TestAgainstMask
                } else {
                    StencilUse::Ignore
                })),
                multisample: wgpu::MultisampleState::default(),
                multiview: None,
                cache: None,
            })
        })
    }

    pub(crate) fn mask(&mut self, gpu: &GpuContext, format: wgpu::TextureFormat) -> &wgpu::RenderPipeline {
        self.mask.entry(format).or_insert_with(|| {
            gpu.device.create_render_pipeline(&wgpu::RenderPipelineDescriptor {
                label: Some("a2d mask pipeline"),
                layout: Some(&self.mask_layout),
                vertex: wgpu::VertexState {
                    module: &self.shader,
                    entry_point: Some("vs_mask"),
                    compilation_options: Default::default(),
                    buffers: &[Vertex::layout()],
                },
                fragment: Some(wgpu::FragmentState {
                    module: &self.shader,
                    entry_point: Some("fs_mask"),
                    compilation_options: Default::default(),
                    targets: &[Some(wgpu::ColorTargetState {
                        format,
                        blend: None,
                        // The mask pass exists only to touch stencil.
                        write_mask: wgpu::ColorWrites::empty(),
                    })],
                }),
                primitive: wgpu::PrimitiveState {
                    topology: wgpu::PrimitiveTopology::TriangleList,
                    cull_mode: None,
                    ..Default::default()
                },
                depth_stencil: Some(depth_stencil_state(StencilUse::InvertForMask)),
                multisample: wgpu::MultisampleState::default(),
                multiview: None,
                cache: None,
            })
        })
    }
}

enum StencilUse {
    /// No clipping: the stencil buffer is neither read nor written.
    Ignore,
    /// Fill the mask outline by inverting, which resolves a self-overlapping
    /// fan into an even-odd fill.
    InvertForMask,
    /// Draw only where a mask has marked the stencil.
    TestAgainstMask,
}

fn depth_stencil_state(use_: StencilUse) -> wgpu::DepthStencilState {
    let face = match use_ {
        StencilUse::Ignore => wgpu::StencilFaceState::IGNORE,
        StencilUse::InvertForMask => wgpu::StencilFaceState {
            compare: wgpu::CompareFunction::Always,
            fail_op: wgpu::StencilOperation::Keep,
            depth_fail_op: wgpu::StencilOperation::Keep,
            pass_op: wgpu::StencilOperation::Invert,
        },
        StencilUse::TestAgainstMask => wgpu::StencilFaceState {
            compare: wgpu::CompareFunction::NotEqual,
            fail_op: wgpu::StencilOperation::Keep,
            depth_fail_op: wgpu::StencilOperation::Keep,
            pass_op: wgpu::StencilOperation::Keep,
        },
    };
    let write_mask = match use_ {
        StencilUse::InvertForMask => 0xff,
        // Drawing must never disturb the mask it is being clipped by.
        _ => 0x00,
    };

    wgpu::DepthStencilState {
        format: DEPTH_STENCIL_FORMAT,
        // Draw order is the painter's algorithm, decided on the CPU by
        // `z_order`. A depth test would fight it.
        depth_write_enabled: false,
        depth_compare: wgpu::CompareFunction::Always,
        stencil: wgpu::StencilState { front: face, back: face, read_mask: 0xff, write_mask },
        bias: wgpu::DepthBiasState::default(),
    }
}

/// Blend factors for a mode.
///
/// The premultiplied variants differ only in the source colour factor: the
/// texture has already been multiplied by its alpha, so multiplying again would
/// darken every edge pixel.
///
/// Alpha is always composited with `One, OneMinusSrcAlpha` regardless of mode,
/// so the destination alpha accumulates correctly. That matters for the
/// transparent desktop window, where the alpha channel is what the compositor
/// uses to decide what shows through.
pub fn blend_state(mode: BlendMode, premultiplied: bool) -> wgpu::BlendState {
    use wgpu::BlendFactor as F;
    let src = if premultiplied { F::One } else { F::SrcAlpha };

    let color = match mode {
        BlendMode::Normal => wgpu::BlendComponent {
            src_factor: src,
            dst_factor: F::OneMinusSrcAlpha,
            operation: wgpu::BlendOperation::Add,
        },
        BlendMode::Additive => {
            wgpu::BlendComponent { src_factor: src, dst_factor: F::One, operation: wgpu::BlendOperation::Add }
        }
        BlendMode::Multiply => wgpu::BlendComponent {
            src_factor: F::Dst,
            dst_factor: F::OneMinusSrcAlpha,
            operation: wgpu::BlendOperation::Add,
        },
        BlendMode::Screen => wgpu::BlendComponent {
            src_factor: F::One,
            dst_factor: F::OneMinusSrc,
            operation: wgpu::BlendOperation::Add,
        },
    };

    let alpha = match mode {
        // Additive must not drive destination alpha to 1 over a transparent
        // background, or a glow would punch an opaque hole in the window.
        BlendMode::Additive => {
            wgpu::BlendComponent { src_factor: F::Zero, dst_factor: F::One, operation: wgpu::BlendOperation::Add }
        }
        _ => wgpu::BlendComponent {
            src_factor: F::One,
            dst_factor: F::OneMinusSrcAlpha,
            operation: wgpu::BlendOperation::Add,
        },
    };

    wgpu::BlendState { color, alpha }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn straight_alpha_multiplies_the_source_by_its_alpha() {
        let b = blend_state(BlendMode::Normal, false);
        assert_eq!(b.color.src_factor, wgpu::BlendFactor::SrcAlpha);
        assert_eq!(b.color.dst_factor, wgpu::BlendFactor::OneMinusSrcAlpha);
    }

    #[test]
    fn premultiplied_alpha_does_not_multiply_again() {
        let b = blend_state(BlendMode::Normal, true);
        assert_eq!(b.color.src_factor, wgpu::BlendFactor::One);
        assert_eq!(b.color.dst_factor, wgpu::BlendFactor::OneMinusSrcAlpha);
    }

    #[test]
    fn additive_accumulates_colour() {
        for premultiplied in [false, true] {
            let b = blend_state(BlendMode::Additive, premultiplied);
            assert_eq!(b.color.dst_factor, wgpu::BlendFactor::One);
        }
    }

    #[test]
    fn additive_leaves_destination_alpha_alone() {
        // Otherwise a glow would turn a transparent window opaque.
        let b = blend_state(BlendMode::Additive, false);
        assert_eq!(b.alpha.src_factor, wgpu::BlendFactor::Zero);
        assert_eq!(b.alpha.dst_factor, wgpu::BlendFactor::One);
    }

    #[test]
    fn multiply_scales_the_destination() {
        let b = blend_state(BlendMode::Multiply, false);
        assert_eq!(b.color.src_factor, wgpu::BlendFactor::Dst);
    }

    #[test]
    fn screen_inverts_the_source_for_the_destination_factor() {
        let b = blend_state(BlendMode::Screen, false);
        assert_eq!(b.color.src_factor, wgpu::BlendFactor::One);
        assert_eq!(b.color.dst_factor, wgpu::BlendFactor::OneMinusSrc);
    }

    #[test]
    fn every_mode_composites_alpha_as_over_except_additive() {
        for mode in [BlendMode::Normal, BlendMode::Multiply, BlendMode::Screen] {
            let b = blend_state(mode, false);
            assert_eq!(b.alpha.src_factor, wgpu::BlendFactor::One, "{mode:?}");
            assert_eq!(b.alpha.dst_factor, wgpu::BlendFactor::OneMinusSrcAlpha, "{mode:?}");
        }
    }

    #[test]
    fn unmasked_drawing_neither_reads_nor_writes_stencil() {
        let s = depth_stencil_state(StencilUse::Ignore);
        assert_eq!(s.stencil.write_mask, 0);
        assert_eq!(s.stencil.front.compare, wgpu::CompareFunction::Always);
    }

    #[test]
    fn the_mask_pass_inverts_stencil_so_a_concave_fan_resolves() {
        let s = depth_stencil_state(StencilUse::InvertForMask);
        assert_eq!(s.stencil.front.pass_op, wgpu::StencilOperation::Invert);
        assert_eq!(s.stencil.write_mask, 0xff);
    }

    #[test]
    fn masked_drawing_tests_stencil_without_writing_it() {
        let s = depth_stencil_state(StencilUse::TestAgainstMask);
        assert_eq!(s.stencil.front.compare, wgpu::CompareFunction::NotEqual);
        assert_eq!(s.stencil.front.pass_op, wgpu::StencilOperation::Keep);
        assert_eq!(s.stencil.write_mask, 0, "clipped drawing must not disturb its own mask");
    }

    #[test]
    fn depth_is_never_written_because_draw_order_decides_occlusion() {
        for use_ in [StencilUse::Ignore, StencilUse::InvertForMask, StencilUse::TestAgainstMask] {
            let s = depth_stencil_state(use_);
            assert!(!s.depth_write_enabled);
            assert_eq!(s.depth_compare, wgpu::CompareFunction::Always);
        }
    }

    #[test]
    fn front_and_back_faces_share_their_stencil_state() {
        // Culling is off, so a triangle's winding must not change its stencil
        // behaviour.
        let s = depth_stencil_state(StencilUse::InvertForMask);
        assert_eq!(s.stencil.front, s.stencil.back);
    }
}
