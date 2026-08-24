//! The renderer.
//!
//! Takes a [`RenderList`] and draws it. It has no idea what produced the list:
//! no source format, no game, no importer. That is the whole point of the
//! [`RenderMesh`](a2d_core::RenderMesh) contract, and an architecture test
//! enforces that this crate cannot even depend on the layers that would let it
//! find out.

use a2d_core::{MaskId, RenderList, Rgba};

use crate::batch::{self, FrameGeometry, Vertex};
use crate::camera::{Camera, Viewport};
use crate::gpu::{GpuContext, RenderError};
use crate::pipeline::{Pipelines, STENCIL_REFERENCE};
use crate::target::DepthStencil;
use crate::texture::TextureCache;

/// What one frame should look like.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct FrameSettings {
    pub viewport: Viewport,
    pub camera: Camera,
    /// Cleared before drawing. Alpha 0 gives the transparent background a
    /// desktop mascot window needs.
    pub clear_color: Rgba,
}

impl FrameSettings {
    pub fn new(viewport: Viewport, camera: Camera) -> FrameSettings {
        FrameSettings { viewport, camera, clear_color: Rgba::TRANSPARENT }
    }

    pub fn with_clear_color(mut self, clear_color: Rgba) -> FrameSettings {
        self.clear_color = clear_color;
        self
    }
}

/// What a frame actually did. Returned so a caller can assert on it and a
/// diagnostic surface can report it.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct FrameStats {
    pub draw_calls: usize,
    pub triangles: usize,
    pub masks: usize,
    /// Batches whose texture id resolved to nothing at all. Non-zero means the
    /// caller uploaded fewer pages than the model references.
    pub missing_textures: usize,
    /// Meshes the batcher refused as malformed.
    pub skipped_meshes: usize,
}

/// Draws render lists.
#[derive(Debug)]
pub struct Renderer {
    gpu: GpuContext,
    pipelines: Pipelines,
    textures: TextureCache,
    geometry: FrameGeometry,

    camera_buffer: wgpu::Buffer,
    camera_bind_group: wgpu::BindGroup,

    vertices: wgpu::Buffer,
    vertex_capacity: usize,
    indices: wgpu::Buffer,
    index_capacity: usize,

    depth: Option<DepthStencil>,
}

impl Renderer {
    pub fn new(gpu: GpuContext) -> Renderer {
        let camera_layout = gpu.device.create_bind_group_layout(&wgpu::BindGroupLayoutDescriptor {
            label: Some("a2d camera"),
            entries: &[wgpu::BindGroupLayoutEntry {
                binding: 0,
                visibility: wgpu::ShaderStages::VERTEX,
                ty: wgpu::BindingType::Buffer {
                    ty: wgpu::BufferBindingType::Uniform,
                    has_dynamic_offset: false,
                    min_binding_size: None,
                },
                count: None,
            }],
        });
        let camera_buffer = gpu.device.create_buffer(&wgpu::BufferDescriptor {
            label: Some("a2d camera"),
            size: std::mem::size_of::<[f32; 4]>() as wgpu::BufferAddress,
            usage: wgpu::BufferUsages::UNIFORM | wgpu::BufferUsages::COPY_DST,
            mapped_at_creation: false,
        });
        let camera_bind_group = gpu.device.create_bind_group(&wgpu::BindGroupDescriptor {
            label: Some("a2d camera"),
            layout: &camera_layout,
            entries: &[wgpu::BindGroupEntry { binding: 0, resource: camera_buffer.as_entire_binding() }],
        });

        let textures = TextureCache::new(&gpu);
        let pipelines = Pipelines::new(&gpu, &camera_layout, textures.layout());

        let vertices = new_vertex_buffer(&gpu, 1024);
        let indices = new_index_buffer(&gpu, 2048);

        Renderer {
            gpu,
            pipelines,
            textures,
            geometry: FrameGeometry::default(),
            camera_buffer,
            camera_bind_group,
            vertices,
            vertex_capacity: 1024,
            indices,
            index_capacity: 2048,
            depth: None,
        }
    }

    pub fn gpu(&self) -> &GpuContext {
        &self.gpu
    }

    pub fn textures(&self) -> &TextureCache {
        &self.textures
    }

    pub fn textures_mut(&mut self) -> &mut TextureCache {
        &mut self.textures
    }

    pub fn pipelines(&self) -> &Pipelines {
        &self.pipelines
    }

    /// The geometry built for the most recent frame. Exposed for tests and for
    /// diagnostics; the renderer does not read it back.
    pub fn last_geometry(&self) -> &FrameGeometry {
        &self.geometry
    }

    /// Draws `list` into `view`.
    ///
    /// `format` must match the view's texture format, which the caller knows
    /// and the view does not expose.
    pub fn render(
        &mut self,
        view: &wgpu::TextureView,
        format: wgpu::TextureFormat,
        settings: FrameSettings,
        list: &RenderList,
    ) -> Result<FrameStats, RenderError> {
        let viewport = settings.viewport;
        if viewport.is_degenerate() {
            return Err(RenderError::Unsupported(format!(
                "viewport has no area: {}x{} at scale {}",
                viewport.width, viewport.height, viewport.scale_factor
            )));
        }

        batch::build(list, &mut self.geometry);
        let mut stats = FrameStats {
            triangles: self.geometry.triangle_count(),
            masks: self.geometry.masks.len(),
            skipped_meshes: self.geometry.skipped_meshes,
            ..Default::default()
        };

        self.gpu.queue.write_buffer(&self.camera_buffer, 0, bytemuck::cast_slice(&settings.camera.transform(viewport)));
        self.upload_geometry();
        self.ensure_depth(viewport);

        let depth_view = match &self.depth {
            Some(depth) => depth.view(),
            // `ensure_depth` always populates it; this keeps the borrow honest.
            None => return Err(RenderError::Unsupported("depth attachment missing".into())),
        };

        let mut encoder =
            self.gpu.device.create_command_encoder(&wgpu::CommandEncoderDescriptor { label: Some("a2d frame") });

        // Each contiguous run of batches sharing a mask becomes one pass, so
        // the stencil can be cleared between masks. A model with no clipping —
        // the common case — is a single pass.
        let runs = mask_runs(&self.geometry);
        let mut first_pass = true;

        if runs.is_empty() {
            // Nothing to draw, but the target still has to be cleared.
            self.clear_only(&mut encoder, view, depth_view, settings.clear_color);
        }

        for run in &runs {
            let load =
                if first_pass { wgpu::LoadOp::Clear(clear_color(settings.clear_color)) } else { wgpu::LoadOp::Load };
            first_pass = false;

            let mut pass = encoder.begin_render_pass(&wgpu::RenderPassDescriptor {
                label: Some("a2d pass"),
                color_attachments: &[Some(wgpu::RenderPassColorAttachment {
                    view,
                    depth_slice: None,
                    resolve_target: None,
                    ops: wgpu::Operations { load, store: wgpu::StoreOp::Store },
                })],
                depth_stencil_attachment: Some(wgpu::RenderPassDepthStencilAttachment {
                    view: depth_view,
                    depth_ops: Some(wgpu::Operations {
                        load: wgpu::LoadOp::Clear(1.0),
                        // Depth is never read; discarding saves the write-back.
                        store: wgpu::StoreOp::Discard,
                    }),
                    stencil_ops: Some(wgpu::Operations {
                        // Clearing per pass is what isolates one mask from the next.
                        load: wgpu::LoadOp::Clear(0),
                        store: wgpu::StoreOp::Store,
                    }),
                }),
                timestamp_writes: None,
                occlusion_query_set: None,
            });

            pass.set_bind_group(0, &self.camera_bind_group, &[]);
            pass.set_vertex_buffer(0, self.vertices.slice(..));
            pass.set_index_buffer(self.indices.slice(..), wgpu::IndexFormat::Uint32);
            pass.set_stencil_reference(STENCIL_REFERENCE);

            if let Some(mask) = run.mask {
                if let Some(shape) = self.geometry.masks.iter().find(|s| s.id == mask) {
                    pass.set_pipeline(self.pipelines.mask(&self.gpu, format));
                    pass.draw_indexed(shape.indices.clone(), 0, 0..1);
                    stats.draw_calls += 1;
                }
            }

            for batch in &self.geometry.batches[run.range.clone()] {
                let Some(texture) = self.textures.resolve(batch.texture) else {
                    stats.missing_textures += 1;
                    continue;
                };
                let pipeline = self.pipelines.mesh(
                    &self.gpu,
                    format,
                    batch.blend_mode,
                    texture.premultiplied_alpha,
                    run.mask.is_some(),
                );
                pass.set_pipeline(pipeline);
                pass.set_bind_group(1, texture.bind_group(), &[]);
                pass.draw_indexed(batch.indices.clone(), 0, 0..1);
                stats.draw_calls += 1;
            }
        }

        self.gpu.queue.submit(Some(encoder.finish()));
        Ok(stats)
    }

    fn clear_only(
        &self,
        encoder: &mut wgpu::CommandEncoder,
        view: &wgpu::TextureView,
        depth_view: &wgpu::TextureView,
        color: Rgba,
    ) {
        let _ = encoder.begin_render_pass(&wgpu::RenderPassDescriptor {
            label: Some("a2d clear"),
            color_attachments: &[Some(wgpu::RenderPassColorAttachment {
                view,
                depth_slice: None,
                resolve_target: None,
                ops: wgpu::Operations { load: wgpu::LoadOp::Clear(clear_color(color)), store: wgpu::StoreOp::Store },
            })],
            depth_stencil_attachment: Some(wgpu::RenderPassDepthStencilAttachment {
                view: depth_view,
                depth_ops: Some(wgpu::Operations { load: wgpu::LoadOp::Clear(1.0), store: wgpu::StoreOp::Discard }),
                stencil_ops: Some(wgpu::Operations { load: wgpu::LoadOp::Clear(0), store: wgpu::StoreOp::Discard }),
            }),
            timestamp_writes: None,
            occlusion_query_set: None,
        });
    }

    fn ensure_depth(&mut self, viewport: Viewport) {
        let needs_new = match &self.depth {
            Some(depth) => !depth.matches(viewport.width, viewport.height),
            None => true,
        };
        if needs_new {
            self.depth = Some(DepthStencil::new(&self.gpu, viewport.width, viewport.height));
        }
    }

    fn upload_geometry(&mut self) {
        if self.geometry.vertices.len() > self.vertex_capacity {
            // Doubling keeps reallocation amortised as a model's mesh count
            // settles, rather than growing once per frame.
            self.vertex_capacity = self.geometry.vertices.len().next_power_of_two();
            self.vertices = new_vertex_buffer(&self.gpu, self.vertex_capacity);
        }
        if self.geometry.indices.len() > self.index_capacity {
            self.index_capacity = self.geometry.indices.len().next_power_of_two();
            self.indices = new_index_buffer(&self.gpu, self.index_capacity);
        }
        if !self.geometry.vertices.is_empty() {
            self.gpu.queue.write_buffer(&self.vertices, 0, bytemuck::cast_slice(&self.geometry.vertices));
        }
        if !self.geometry.indices.is_empty() {
            self.gpu.queue.write_buffer(&self.indices, 0, bytemuck::cast_slice(&self.geometry.indices));
        }
    }
}

/// A contiguous span of batches sharing one clipping mask.
#[derive(Debug, Clone, PartialEq, Eq)]
struct MaskRun {
    mask: Option<MaskId>,
    range: std::ops::Range<usize>,
}

/// Splits batches into runs at every mask change.
fn mask_runs(geometry: &FrameGeometry) -> Vec<MaskRun> {
    let mut runs: Vec<MaskRun> = Vec::new();
    for (i, batch) in geometry.batches.iter().enumerate() {
        match runs.last_mut() {
            Some(last) if last.mask == batch.mask => last.range.end = i + 1,
            _ => runs.push(MaskRun { mask: batch.mask, range: i..i + 1 }),
        }
    }
    runs
}

fn clear_color(color: Rgba) -> wgpu::Color {
    wgpu::Color { r: color.r as f64, g: color.g as f64, b: color.b as f64, a: color.a as f64 }
}

fn new_vertex_buffer(gpu: &GpuContext, capacity: usize) -> wgpu::Buffer {
    gpu.device.create_buffer(&wgpu::BufferDescriptor {
        label: Some("a2d vertices"),
        size: (capacity * std::mem::size_of::<Vertex>()) as wgpu::BufferAddress,
        usage: wgpu::BufferUsages::VERTEX | wgpu::BufferUsages::COPY_DST,
        mapped_at_creation: false,
    })
}

fn new_index_buffer(gpu: &GpuContext, capacity: usize) -> wgpu::Buffer {
    gpu.device.create_buffer(&wgpu::BufferDescriptor {
        label: Some("a2d indices"),
        size: (capacity * std::mem::size_of::<u32>()) as wgpu::BufferAddress,
        usage: wgpu::BufferUsages::INDEX | wgpu::BufferUsages::COPY_DST,
        mapped_at_creation: false,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use a2d_core::{BlendMode, RenderMesh, TextureId, Vec2};

    fn geometry_with(masks: &[Option<u32>]) -> FrameGeometry {
        let mut list = RenderList::new();
        // One mask per distinct id, so the ids the batches reference exist.
        let highest = masks.iter().flatten().copied().max();
        for _ in 0..=highest.unwrap_or(0) {
            list.push_mask(vec![Vec2::ZERO, Vec2::new(1.0, 0.0), Vec2::new(0.0, 1.0)]);
        }
        for (z, mask) in masks.iter().enumerate() {
            list.push_mesh(RenderMesh {
                vertices: vec![Vec2::ZERO, Vec2::new(1.0, 0.0), Vec2::new(0.0, 1.0)],
                uvs: vec![Vec2::ZERO; 3],
                indices: vec![0, 1, 2],
                // A differing texture per mesh keeps every mesh its own batch.
                texture: TextureId(z as u32),
                blend_mode: BlendMode::Normal,
                clipping_mask: mask.map(MaskId),
                z_order: z as u32,
                ..Default::default()
            });
        }
        let mut geometry = FrameGeometry::default();
        batch::build(&list, &mut geometry);
        geometry
    }

    #[test]
    fn an_unmasked_frame_is_a_single_pass() {
        let runs = mask_runs(&geometry_with(&[None, None, None]));
        assert_eq!(runs.len(), 1);
        assert_eq!(runs[0].mask, None);
        assert_eq!(runs[0].range, 0..3);
    }

    #[test]
    fn a_mask_change_starts_a_new_pass() {
        let runs = mask_runs(&geometry_with(&[None, Some(0), None]));
        assert_eq!(runs.len(), 3);
        assert_eq!(runs[0].mask, None);
        assert_eq!(runs[1].mask, Some(MaskId(0)));
        assert_eq!(runs[2].mask, None);
    }

    #[test]
    fn consecutive_batches_under_one_mask_share_a_pass() {
        let runs = mask_runs(&geometry_with(&[Some(0), Some(0), Some(0)]));
        assert_eq!(runs.len(), 1);
        assert_eq!(runs[0].range, 0..3);
    }

    #[test]
    fn two_different_masks_get_their_own_passes_so_the_stencil_is_isolated() {
        let runs = mask_runs(&geometry_with(&[Some(0), Some(1)]));
        assert_eq!(runs.len(), 2);
        assert_eq!(runs[0].mask, Some(MaskId(0)));
        assert_eq!(runs[1].mask, Some(MaskId(1)));
    }

    #[test]
    fn an_empty_frame_has_no_runs_so_the_caller_only_clears() {
        assert!(mask_runs(&FrameGeometry::default()).is_empty());
    }

    #[test]
    fn every_batch_belongs_to_exactly_one_run() {
        let geometry = geometry_with(&[None, Some(0), Some(0), None, Some(1)]);
        let runs = mask_runs(&geometry);
        let covered: usize = runs.iter().map(|r| r.range.len()).sum();
        assert_eq!(covered, geometry.batches.len());
        for pair in runs.windows(2) {
            assert_eq!(pair[0].range.end, pair[1].range.start, "runs must be contiguous");
        }
    }

    #[test]
    fn a_transparent_clear_stays_transparent() {
        let c = clear_color(Rgba::TRANSPARENT);
        assert_eq!(c.a, 0.0);
    }

    #[test]
    fn frame_settings_default_to_a_transparent_background() {
        let settings = FrameSettings::new(Viewport::new(100, 100), Camera::default());
        assert_eq!(settings.clear_color, Rgba::TRANSPARENT);
        assert_eq!(settings.with_clear_color(Rgba::WHITE).clear_color, Rgba::WHITE);
    }
}
