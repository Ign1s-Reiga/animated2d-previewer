//! Skeleton pose state and world transform evaluation.
//!
//! The transform maths follows the reference Spine runtime closely and
//! deliberately: matching it term-for-term is the only practical way to hit the
//! "nearly identical geometry at the same timestamp" bar in spec §17.4. Where
//! this file departs from the reference, the comment says why.

use std::sync::Arc;

use a2d_core::ir::ids::{AttachmentId, BoneId, SkinId, SlotId};
use a2d_core::ir::spine::{
    AttachmentKind, BoneLocal, ConstraintKind, PathConstraint, PathPositionMode, PathRotateMode, PathSpacingMode,
    SpineIr, TransformConstraint, TransformInherit, VertexData, DEFAULT_SKIN,
};
use a2d_core::{Affine2, Degradation, LoadReport, Rgb, Rgba, Vec2};

/// Per-bone pose: the applied local transform plus the derived world transform.
#[derive(Debug, Clone, PartialEq)]
pub struct BonePose {
    /// Local transform after animation and after any constraint that writes
    /// back to local space. This is the reference runtime's "applied" transform.
    pub local: BoneLocal,
    pub world: Affine2,
}

/// Per-slot pose.
#[derive(Debug, Clone, PartialEq)]
pub struct SlotPose {
    pub color: Rgba,
    pub dark_color: Option<Rgb>,
    /// Resolved attachment, or `None` when the slot shows nothing.
    pub attachment: Option<AttachmentId>,
    /// Deform offsets for the current attachment, indexed the way
    /// [`VertexData::deform_len`] describes. Empty means "no deformation".
    ///
    /// [`VertexData::deform_len`]: a2d_core::ir::spine::VertexData::deform_len
    pub deform: Vec<f32>,
}

/// Runtime mix values for an IK constraint, which timelines can animate.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct IkPose {
    pub mix: f32,
    pub softness: f32,
    pub bend_positive: bool,
    pub compress: bool,
    pub stretch: bool,
}

/// Runtime mix values for a transform constraint.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct TransformPose {
    pub mix_rotate: f32,
    pub mix_x: f32,
    pub mix_y: f32,
    pub mix_scale_x: f32,
    pub mix_scale_y: f32,
    pub mix_shear_y: f32,
}

/// Runtime values for a path constraint.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct PathPose {
    /// Distance along the path of the first bone, in world units or as a
    /// fraction of the path length depending on the position mode.
    pub position: f32,
    /// Gap between consecutive bones, interpreted by the spacing mode.
    pub spacing: f32,
    pub mix_rotate: f32,
    pub mix_x: f32,
    pub mix_y: f32,
}

/// The mutable pose of one skeleton instance.
///
/// Several instances can share one [`SpineIr`], which is why the data is behind
/// an `Arc` and everything mutable lives here.
#[derive(Debug, Clone)]
pub struct SkeletonPose {
    ir: Arc<SpineIr>,
    pub bones: Vec<BonePose>,
    pub slots: Vec<SlotPose>,
    /// Current draw order; defaults to the setup-pose slot order.
    pub draw_order: Vec<SlotId>,
    skin: SkinId,
    /// Whole-skeleton offset, applied to the root.
    pub position: Vec2,
    /// Whole-skeleton scale. Negative components flip the character.
    pub scale: Vec2,
    pub ik: Vec<IkPose>,
    pub transform: Vec<TransformPose>,
    pub path: Vec<PathPose>,
    /// Set once when an unsupported constraint mode is first encountered, so
    /// the report is not spammed once per frame.
    reported_unsupported: Vec<String>,
}

impl SkeletonPose {
    pub fn new(ir: Arc<SpineIr>) -> Self {
        let bones = ir.bones.iter().map(|b| BonePose { local: b.setup, world: Affine2::IDENTITY }).collect();
        let slots = ir
            .slots
            .iter()
            .map(|s| SlotPose { color: s.color, dark_color: s.dark_color, attachment: None, deform: Vec::new() })
            .collect();
        let draw_order = (0..ir.slots.len()).filter_map(SlotId::from_index).collect();
        let ik = ir
            .ik_constraints
            .iter()
            .map(|c| IkPose {
                mix: c.mix,
                softness: c.softness,
                bend_positive: c.bend_positive,
                compress: c.compress,
                stretch: c.stretch,
            })
            .collect();
        let transform = ir
            .transform_constraints
            .iter()
            .map(|c| TransformPose {
                mix_rotate: c.mix_rotate,
                mix_x: c.mix_x,
                mix_y: c.mix_y,
                mix_scale_x: c.mix_scale_x,
                mix_scale_y: c.mix_scale_y,
                mix_shear_y: c.mix_shear_y,
            })
            .collect();
        let path = ir
            .path_constraints
            .iter()
            .map(|c| PathPose {
                position: c.position,
                spacing: c.spacing,
                mix_rotate: c.mix_rotate,
                mix_x: c.mix_x,
                mix_y: c.mix_y,
            })
            .collect();

        let mut pose = SkeletonPose {
            ir,
            bones,
            slots,
            draw_order,
            skin: DEFAULT_SKIN,
            position: Vec2::ZERO,
            scale: Vec2::ONE,
            ik,
            transform,
            path,
            reported_unsupported: Vec::new(),
        };
        pose.reset_to_setup();
        pose.update_world_transforms();
        pose
    }

    pub fn ir(&self) -> &Arc<SpineIr> {
        &self.ir
    }

    pub fn skin(&self) -> SkinId {
        self.skin
    }

    /// Switches the active skin and re-resolves every slot's attachment.
    pub fn set_skin(&mut self, skin: SkinId) {
        self.skin = skin;
        self.reset_attachments_to_setup();
    }

    /// Restores every animatable value to the setup pose.
    ///
    /// Called at the start of every evaluation, so that timelines describe an
    /// absolute pose rather than accumulating frame to frame.
    pub fn reset_to_setup(&mut self) {
        for (pose, data) in self.bones.iter_mut().zip(&self.ir.bones) {
            pose.local = data.setup;
        }
        for (pose, data) in self.slots.iter_mut().zip(&self.ir.slots) {
            pose.color = data.color;
            pose.dark_color = data.dark_color;
            pose.deform.clear();
        }
        for (pose, data) in self.ik.iter_mut().zip(&self.ir.ik_constraints) {
            pose.mix = data.mix;
            pose.softness = data.softness;
            pose.bend_positive = data.bend_positive;
            pose.compress = data.compress;
            pose.stretch = data.stretch;
        }
        for (pose, data) in self.transform.iter_mut().zip(&self.ir.transform_constraints) {
            pose.mix_rotate = data.mix_rotate;
            pose.mix_x = data.mix_x;
            pose.mix_y = data.mix_y;
            pose.mix_scale_x = data.mix_scale_x;
            pose.mix_scale_y = data.mix_scale_y;
            pose.mix_shear_y = data.mix_shear_y;
        }
        for (pose, data) in self.path.iter_mut().zip(&self.ir.path_constraints) {
            pose.position = data.position;
            pose.spacing = data.spacing;
            pose.mix_rotate = data.mix_rotate;
            pose.mix_x = data.mix_x;
            pose.mix_y = data.mix_y;
        }
        self.reset_draw_order();
        self.reset_attachments_to_setup();
    }

    /// World positions for a vertex set, applying deform offsets and skinning.
    ///
    /// Rigid vertices ride the slot's bone; weighted ones are a weighted sum
    /// over the bones that influence them. Deform offsets index vertices in the
    /// rigid case and *influences* in the weighted one, which is why the two
    /// branches read `deform` differently.
    pub fn world_vertices(&self, vertices: &VertexData, deform: &[f32], slot_bone: Affine2) -> Vec<Vec2> {
        match vertices {
            VertexData::Rigid(local) => local
                .iter()
                .enumerate()
                .map(|(i, v)| {
                    let dx = deform.get(i * 2).copied().unwrap_or(0.0);
                    let dy = deform.get(i * 2 + 1).copied().unwrap_or(0.0);
                    slot_bone.transform_point(Vec2::new(v.x + dx, v.y + dy))
                })
                .collect(),
            VertexData::Weighted(w) => {
                let mut out = Vec::with_capacity(w.vertex_count());
                for vertex in 0..w.vertex_count() {
                    let start = w.offsets.get(vertex).copied().unwrap_or(0) as usize;
                    let mut sum = Vec2::ZERO;
                    for (n, influence) in w.influences_for(vertex).iter().enumerate() {
                        let f = (start + n) * 2;
                        let dx = deform.get(f).copied().unwrap_or(0.0);
                        let dy = deform.get(f + 1).copied().unwrap_or(0.0);
                        let local = Vec2::new(influence.position.x + dx, influence.position.y + dy);
                        let Some(bone) = self.bones.get(influence.bone.index()) else { continue };
                        sum += bone.world.transform_point(local) * influence.weight;
                    }
                    out.push(sum);
                }
                out
            }
        }
    }

    pub fn reset_draw_order(&mut self) {
        self.draw_order.clear();
        self.draw_order.extend((0..self.ir.slots.len()).filter_map(SlotId::from_index));
    }

    pub fn reset_attachments_to_setup(&mut self) {
        for (i, data) in self.ir.slots.iter().enumerate() {
            let resolved = data.setup_attachment.as_ref().and_then(|name| {
                SlotId::from_index(i).and_then(|slot| self.ir.resolve_attachment(self.skin, slot, name))
            });
            self.slots[i].attachment = resolved;
        }
    }

    /// Sets a slot's attachment from a placeholder name, resolving through the
    /// active skin. `None` hides the slot.
    pub fn set_slot_attachment(&mut self, slot: SlotId, name: Option<&str>) {
        let resolved = name.and_then(|n| self.ir.resolve_attachment(self.skin, slot, n));
        if let Some(s) = self.slots.get_mut(slot.index()) {
            s.attachment = resolved;
        }
    }

    /// Recomputes every bone's world transform, then applies constraints.
    pub fn update_world_transforms(&mut self) {
        for i in 0..self.bones.len() {
            self.update_bone_world(i);
        }
        self.apply_constraints();
    }

    /// Computes one bone's world transform from its applied local transform and
    /// its parent's world transform.
    fn update_bone_world(&mut self, index: usize) {
        let data = &self.ir.bones[index];
        let local = self.bones[index].local;
        let (sx, sy) = (self.scale.x, self.scale.y);

        let Some(parent_id) = data.parent else {
            // Root: the skeleton transform stands in for a parent.
            let rotation_y = local.rotation + 90.0 + local.shear.y;
            let rx = (local.rotation + local.shear.x).to_radians();
            let ry = rotation_y.to_radians();
            self.bones[index].world = Affine2 {
                a: rx.cos() * local.scale.x * sx,
                b: ry.cos() * local.scale.y * sx,
                c: rx.sin() * local.scale.x * sy,
                d: ry.sin() * local.scale.y * sy,
                tx: local.position.x * sx + self.position.x,
                ty: local.position.y * sy + self.position.y,
            };
            return;
        };

        let p = self.bones[parent_id.index()].world;
        let (mut pa, mut pb, mut pc, mut pd) = (p.a, p.b, p.c, p.d);
        let tx = pa * local.position.x + pb * local.position.y + p.tx;
        let ty = pc * local.position.x + pd * local.position.y + p.ty;

        let (a, b, c, d) = match data.inherit {
            TransformInherit::Normal => {
                let rx = (local.rotation + local.shear.x).to_radians();
                let ry = (local.rotation + 90.0 + local.shear.y).to_radians();
                let la = rx.cos() * local.scale.x;
                let lb = ry.cos() * local.scale.y;
                let lc = rx.sin() * local.scale.x;
                let ld = ry.sin() * local.scale.y;
                self.bones[index].world = Affine2 {
                    a: pa * la + pb * lc,
                    b: pa * lb + pb * ld,
                    c: pc * la + pd * lc,
                    d: pc * lb + pd * ld,
                    tx,
                    ty,
                };
                return;
            }
            TransformInherit::OnlyTranslation => {
                let rx = (local.rotation + local.shear.x).to_radians();
                let ry = (local.rotation + 90.0 + local.shear.y).to_radians();
                (rx.cos() * local.scale.x, ry.cos() * local.scale.y, rx.sin() * local.scale.x, ry.sin() * local.scale.y)
            }
            TransformInherit::NoRotationOrReflection => {
                let mut s = pa * pa + pc * pc;
                let prx;
                if s > 0.0001 {
                    s = (pa * pd - pb * pc).abs() / s;
                    pa /= sx;
                    pc /= sy;
                    pb = pc * s;
                    pd = pa * s;
                    prx = pc.atan2(pa).to_degrees();
                } else {
                    pa = 0.0;
                    pc = 0.0;
                    prx = 90.0 - pd.atan2(pb).to_degrees();
                }
                let rx = (local.rotation + local.shear.x - prx).to_radians();
                let ry = (local.rotation + local.shear.y - prx + 90.0).to_radians();
                let la = rx.cos() * local.scale.x;
                let lb = ry.cos() * local.scale.y;
                let lc = rx.sin() * local.scale.x;
                let ld = ry.sin() * local.scale.y;
                (pa * la - pb * lc, pa * lb - pb * ld, pc * la + pd * lc, pc * lb + pd * ld)
            }
            TransformInherit::NoScale | TransformInherit::NoScaleOrReflection => {
                let rad = local.rotation.to_radians();
                let (sin, cos) = rad.sin_cos();
                let mut za = (pa * cos + pb * sin) / sx;
                let mut zc = (pc * cos + pd * sin) / sy;
                let mut s = (za * za + zc * zc).sqrt();
                if s > 0.00001 {
                    s = 1.0 / s;
                }
                za *= s;
                zc *= s;
                s = (za * za + zc * zc).sqrt();
                // Reflection is preserved only in `NoScale` mode.
                if data.inherit == TransformInherit::NoScale
                    && ((pa * pd - pb * pc < 0.0) != ((sx < 0.0) != (sy < 0.0)))
                {
                    s = -s;
                }
                let r = std::f32::consts::FRAC_PI_2 + zc.atan2(za);
                let zb = r.cos() * s;
                let zd = r.sin() * s;
                let la = local.shear.x.to_radians().cos() * local.scale.x;
                let lb = (90.0 + local.shear.y).to_radians().cos() * local.scale.y;
                let lc = local.shear.x.to_radians().sin() * local.scale.x;
                let ld = (90.0 + local.shear.y).to_radians().sin() * local.scale.y;
                (za * la + zb * lc, za * lb + zb * ld, zc * la + zd * lc, zc * lb + zd * ld)
            }
        };

        // Every non-`Normal` mode ignores part of the parent's transform, so the
        // skeleton scale has to be reapplied here.
        self.bones[index].world = Affine2 { a: a * sx, b: b * sx, c: c * sy, d: d * sy, tx, ty };
    }

    /// Recomputes world transforms for every bone at or after `from`.
    ///
    /// Bones are stored parent-before-child, so this is enough to propagate a
    /// change made to bone `from - 1` down its whole subtree.
    fn update_world_from(&mut self, from: usize) {
        for i in from..self.bones.len() {
            self.update_bone_world(i);
        }
    }

    fn apply_constraints(&mut self) {
        for entry in self.ir.constraint_order.clone() {
            match entry.kind {
                ConstraintKind::Ik => self.apply_ik(entry.index as usize),
                ConstraintKind::Transform => self.apply_transform_constraint(entry.index as usize),
                ConstraintKind::Path => self.apply_path_constraint(entry.index as usize),
            }
        }
    }

    fn note_unsupported(&mut self, what: &str) {
        if !self.reported_unsupported.iter().any(|s| s == what) {
            self.reported_unsupported.push(what.to_string());
        }
    }

    /// Degradations discovered while posing, for the caller's load report.
    pub fn degradations(&self) -> Vec<Degradation> {
        self.reported_unsupported
            .iter()
            .map(|what| Degradation::UnsupportedConstraint { name: "*".into(), kind: what.clone() })
            .collect()
    }

    /// Folds pose-time degradations into a report.
    pub fn absorb_degradations(&self, report: &mut LoadReport) {
        for d in self.degradations() {
            report.warn(d);
        }
    }

    // ------------------------------------------------------------ IK

    fn apply_ik(&mut self, index: usize) {
        let Some(constraint) = self.ir.ik_constraints.get(index).cloned() else { return };
        let Some(pose) = self.ik.get(index).copied() else { return };
        if pose.mix == 0.0 || constraint.bones.is_empty() {
            return;
        }
        let target = self.bones[constraint.target.index()].world.translation();
        let bend_dir = if pose.bend_positive { 1.0f32 } else { -1.0 };

        let lowest = match constraint.bones.len() {
            1 => {
                let b = constraint.bones[0];
                self.apply_ik_one(b, target, pose.compress, pose.stretch, constraint.uniform, pose.mix);
                b.index()
            }
            _ => {
                let (parent, child) = (constraint.bones[0], constraint.bones[1]);
                self.apply_ik_two(parent, child, target, bend_dir, pose.stretch, pose.softness, pose.mix);
                parent.index().min(child.index())
            }
        };
        self.update_world_from(lowest + 1);
    }

    /// Single-bone IK: rotate the bone so its `+X` axis points at the target.
    fn apply_ik_one(&mut self, bone: BoneId, target: Vec2, compress: bool, stretch: bool, uniform: bool, alpha: f32) {
        let i = bone.index();
        let data = &self.ir.bones[i];
        let local = self.bones[i].local;
        let mut rotation_ik = -local.shear.x - local.rotation;

        let (tx, ty) = match data.parent {
            None => {
                let world = self.bones[i].world;
                (target.x - world.tx, target.y - world.ty)
            }
            Some(parent_id) => {
                let p = self.bones[parent_id.index()].world;
                let (mut pa, mut pb, mut pc, mut pd) = (p.a, p.b, p.c, p.d);
                if data.inherit == TransformInherit::NoRotationOrReflection {
                    let s = (pa * pd - pb * pc).abs() / (pa * pa + pc * pc).max(1e-9);
                    let sa = pa / self.scale.x;
                    let sc = pc / self.scale.y;
                    pb = -sc * s * self.scale.x;
                    pd = sa * s * self.scale.y;
                    rotation_ik += sc.atan2(sa).to_degrees();
                    pa = sa * self.scale.x;
                    pc = sc * self.scale.y;
                }
                let x = target.x - p.tx;
                let y = target.y - p.ty;
                let det = pa * pd - pb * pc;
                if det.abs() < 1e-9 {
                    return;
                }
                ((x * pd - y * pb) / det - local.position.x, (y * pa - x * pc) / det - local.position.y)
            }
        };

        rotation_ik += ty.atan2(tx).to_degrees();
        if local.scale.x < 0.0 {
            rotation_ik += 180.0;
        }
        rotation_ik = a2d_core::math::wrap_degrees(rotation_ik);

        let mut sx = local.scale.x;
        let mut sy = local.scale.y;
        if compress || stretch {
            let length = data.length * sx;
            let dist = (tx * tx + ty * ty).sqrt();
            if length > 0.0001 && ((compress && dist < length) || (stretch && dist > length)) {
                let s = (dist / length - 1.0) * alpha + 1.0;
                sx *= s;
                if uniform {
                    sy *= s;
                }
            }
        }

        self.bones[i].local =
            BoneLocal { rotation: local.rotation + rotation_ik * alpha, scale: Vec2::new(sx, sy), ..local };
        self.update_bone_world(i);
    }

    /// Two-bone IK. Ported from the reference solver, including its handling of
    /// non-uniform parent scale, softness and stretch.
    #[allow(clippy::too_many_arguments)]
    fn apply_ik_two(
        &mut self,
        parent: BoneId,
        child: BoneId,
        target: Vec2,
        bend_dir: f32,
        stretch: bool,
        softness: f32,
        alpha: f32,
    ) {
        let (pi, ci) = (parent.index(), child.index());
        let Some(grandparent) = self.ir.bones[pi].parent else {
            // Without a grandparent the solver has no space to work in; fall
            // back to the single-bone case rather than producing garbage.
            self.apply_ik_one(parent, target, false, stretch, false, alpha);
            return;
        };

        let p_local = self.bones[pi].local;
        let c_local = self.bones[ci].local;
        let (px, py) = (p_local.position.x, p_local.position.y);
        let mut sx = p_local.scale.x;
        let mut psx = p_local.scale.x;
        let mut psy = p_local.scale.y;
        let mut csx = c_local.scale.x;

        let (os1, mut s2) = if psx < 0.0 {
            psx = -psx;
            (180.0f32, -1.0f32)
        } else {
            (0.0, 1.0)
        };
        if psy < 0.0 {
            psy = -psy;
            s2 = -s2;
        }
        let os2 = if csx < 0.0 {
            csx = -csx;
            180.0f32
        } else {
            0.0
        };

        let pw = self.bones[pi].world;
        let cx = c_local.position.x;
        // With non-uniform parent scale the child's local Y is folded away, so
        // the solver works on the bone's own axis instead.
        let uniform_parent = (psx - psy).abs() <= 0.0001;
        let (cy, cwx, cwy) = if uniform_parent {
            let cy = c_local.position.y;
            (cy, pw.a * cx + pw.b * cy + pw.tx, pw.c * cx + pw.d * cy + pw.ty)
        } else {
            (0.0, pw.a * cx + pw.tx, pw.c * cx + pw.ty)
        };

        let gp = self.bones[grandparent.index()].world;
        let det = gp.a * gp.d - gp.b * gp.c;
        if det.abs() < 1e-9 {
            return;
        }
        let id = 1.0 / det;
        let x = cwx - gp.tx;
        let y = cwy - gp.ty;
        let dx = (x * gp.d - y * gp.b) * id - px;
        let dy = (y * gp.a - x * gp.c) * id - py;
        let l1 = (dx * dx + dy * dy).sqrt();
        let l2 = self.ir.bones[ci].length * csx;

        if l1 < 0.0001 {
            self.apply_ik_one(parent, target, false, stretch, false, alpha);
            self.bones[ci].local = BoneLocal { rotation: 0.0, ..c_local };
            self.update_bone_world(ci);
            return;
        }

        let x = target.x - gp.tx;
        let y = target.y - gp.ty;
        let mut tx = (x * gp.d - y * gp.b) * id - px;
        let mut ty = (y * gp.a - x * gp.c) * id - py;
        let mut dd = tx * tx + ty * ty;

        if softness != 0.0 {
            let softness = softness * (psx * (csx + 1.0) / 2.0);
            if softness > 0.0 {
                let td = dd.sqrt();
                let sd = td - l1 - l2 * psx + softness;
                if sd > 0.0 {
                    let mut p = (sd / (softness * 2.0)).min(1.0) - 1.0;
                    p = (sd - softness * (1.0 - p * p)) / td;
                    tx -= p * tx;
                    ty -= p * ty;
                    dd = tx * tx + ty * ty;
                }
            }
        }

        let (a1, a2) = if uniform_parent {
            let l2 = l2 * psx;
            let mut cos = (dd - l1 * l1 - l2 * l2) / (2.0 * l1 * l2);
            if cos < -1.0 {
                cos = -1.0;
            } else if cos > 1.0 {
                cos = 1.0;
                if stretch {
                    sx *= (dd.sqrt() / (l1 + l2) - 1.0) * alpha + 1.0;
                }
            }
            let a2 = cos.acos() * bend_dir;
            let a = l1 + l2 * cos;
            let b = l2 * a2.sin();
            ((ty * a - tx * b).atan2(tx * a + ty * b), a2)
        } else {
            solve_two_bone_nonuniform(psx, psy, l1, l2, tx, ty, dd, bend_dir)
        };

        let os = cy.atan2(cx) * s2;
        let rotation = p_local.rotation;
        let mut a1 = (a1 - os).to_degrees() + os1 - rotation;
        a1 = a2d_core::math::wrap_degrees(a1);
        self.bones[pi].local = BoneLocal {
            rotation: rotation + a1 * alpha,
            scale: Vec2::new(sx, p_local.scale.y),
            shear: Vec2::ZERO,
            ..p_local
        };
        self.update_bone_world(pi);

        let rotation = c_local.rotation;
        let mut a2 = ((a2 + os).to_degrees() - c_local.shear.x) * s2 + os2 - rotation;
        a2 = a2d_core::math::wrap_degrees(a2);
        self.bones[ci].local = BoneLocal { rotation: rotation + a2 * alpha, ..c_local };
        self.update_bone_world(ci);
    }

    // ------------------------------------------------------------ transform constraints

    fn apply_transform_constraint(&mut self, index: usize) {
        let Some(constraint) = self.ir.transform_constraints.get(index).cloned() else { return };
        let Some(mix) = self.transform.get(index).copied() else { return };

        // The reference runtime splits these four cases into four functions
        // and so does this: they share the meaning of the fields and nothing
        // else. `local` reads and writes the local transform of the bone where
        // `world` works on the composed matrix; `relative` adds the transform
        // of the target on top where absolute replaces it.
        match (constraint.local, constraint.relative) {
            (false, false) => self.apply_transform_absolute_world(&constraint, mix),
            (false, true) => self.apply_transform_relative_world(&constraint, mix),
            (true, false) => self.apply_transform_absolute_local(&constraint, mix),
            (true, true) => self.apply_transform_relative_local(&constraint, mix),
        }
    }

    /// Replaces the world transform of each bone with that of the target.
    fn apply_transform_absolute_world(&mut self, constraint: &TransformConstraint, mix: TransformPose) {
        let t = self.bones[constraint.target.index()].world;
        let reflect = if t.a * t.d - t.b * t.c > 0.0 { 1.0f32 } else { -1.0 };
        let offset_rotation = constraint.offset_rotation.to_radians() * reflect;
        let offset_shear_y = constraint.offset_shear_y.to_radians() * reflect;
        let offset_world = t.transform_point(Vec2::new(constraint.offset_x, constraint.offset_y));

        let mut lowest = usize::MAX;
        for bone_id in &constraint.bones {
            let i = bone_id.index();
            let Some(pose) = self.bones.get_mut(i) else { continue };
            let mut w = pose.world;
            let mut modified = false;

            if mix.mix_rotate != 0.0 {
                let mut r = t.c.atan2(t.a) - w.c.atan2(w.a) + offset_rotation;
                r = wrap_radians(r) * mix.mix_rotate;
                let (sin, cos) = r.sin_cos();
                let (a, b, c, d) = (w.a, w.b, w.c, w.d);
                w.a = cos * a - sin * c;
                w.b = cos * b - sin * d;
                w.c = sin * a + cos * c;
                w.d = sin * b + cos * d;
                modified = true;
            }
            if mix.mix_x != 0.0 || mix.mix_y != 0.0 {
                w.tx += (offset_world.x - w.tx) * mix.mix_x;
                w.ty += (offset_world.y - w.ty) * mix.mix_y;
                modified = true;
            }
            if mix.mix_scale_x > 0.0 || mix.mix_scale_y > 0.0 {
                let mut s = (w.a * w.a + w.c * w.c).sqrt();
                if s != 0.0 {
                    s = (s + ((t.a * t.a + t.c * t.c).sqrt() - s + constraint.offset_scale_x) * mix.mix_scale_x) / s;
                }
                w.a *= s;
                w.c *= s;
                let mut s = (w.b * w.b + w.d * w.d).sqrt();
                if s != 0.0 {
                    s = (s + ((t.b * t.b + t.d * t.d).sqrt() - s + constraint.offset_scale_y) * mix.mix_scale_y) / s;
                }
                w.b *= s;
                w.d *= s;
                modified = true;
            }
            if mix.mix_shear_y > 0.0 {
                let by = w.d.atan2(w.b);
                let mut r = t.d.atan2(t.b) - t.c.atan2(t.a) - (by - w.c.atan2(w.a));
                r = wrap_radians(r);
                let r = by + (r + offset_shear_y) * mix.mix_shear_y;
                let s = (w.b * w.b + w.d * w.d).sqrt();
                w.b = r.cos() * s;
                w.d = r.sin() * s;
                modified = true;
            }

            if modified {
                pose.world = w;
                lowest = lowest.min(i);
            }
        }

        self.rebuild_below(&constraint.bones, lowest);
    }

    /// Adds the world transform of the target on top, rather than replacing.
    fn apply_transform_relative_world(&mut self, constraint: &TransformConstraint, mix: TransformPose) {
        let t = self.bones[constraint.target.index()].world;
        let reflect = if t.a * t.d - t.b * t.c > 0.0 { 1.0f32 } else { -1.0 };
        let offset_rotation = constraint.offset_rotation.to_radians() * reflect;
        let offset_shear_y = constraint.offset_shear_y.to_radians() * reflect;

        let mut lowest = usize::MAX;
        for bone_id in &constraint.bones {
            let i = bone_id.index();
            let Some(pose) = self.bones.get_mut(i) else { continue };
            let mut w = pose.world;
            let mut modified = false;

            if mix.mix_rotate != 0.0 {
                // The rotation the bone already has is not subtracted. That
                // single omission is the whole difference from absolute mode:
                // the rotation of the target is added on top instead of matched.
                let r = wrap_radians(t.c.atan2(t.a) + offset_rotation) * mix.mix_rotate;
                let (sin, cos) = r.sin_cos();
                let (a, b, c, d) = (w.a, w.b, w.c, w.d);
                w.a = cos * a - sin * c;
                w.b = cos * b - sin * d;
                w.c = sin * a + cos * c;
                w.d = sin * b + cos * d;
                modified = true;
            }
            if mix.mix_x != 0.0 || mix.mix_y != 0.0 {
                // The offset is rotated into the basis of the target and added,
                // where absolute mode moves towards its world position.
                let (x, y) = (constraint.offset_x, constraint.offset_y);
                w.tx += (t.a * x + t.b * y) * mix.mix_x;
                w.ty += (t.c * x + t.d * y) * mix.mix_y;
                modified = true;
            }
            if mix.mix_scale_x != 0.0 {
                let s = ((t.a * t.a + t.c * t.c).sqrt() - 1.0 + constraint.offset_scale_x) * mix.mix_scale_x + 1.0;
                w.a *= s;
                w.c *= s;
                modified = true;
            }
            if mix.mix_scale_y != 0.0 {
                let s = ((t.b * t.b + t.d * t.d).sqrt() - 1.0 + constraint.offset_scale_y) * mix.mix_scale_y + 1.0;
                w.b *= s;
                w.d *= s;
                modified = true;
            }
            if mix.mix_shear_y > 0.0 {
                let r = wrap_radians(t.d.atan2(t.b) - t.c.atan2(t.a));
                let by = w.d.atan2(w.b);
                let r = by + (r - std::f32::consts::FRAC_PI_2 + offset_shear_y) * mix.mix_shear_y;
                let s = (w.b * w.b + w.d * w.d).sqrt();
                w.b = r.cos() * s;
                w.d = r.sin() * s;
                modified = true;
            }

            if modified {
                pose.world = w;
                lowest = lowest.min(i);
            }
        }

        self.rebuild_below(&constraint.bones, lowest);
    }

    /// Replaces the *local* transform of each bone with that of the target.
    ///
    /// Local modes are easier to reason about but more expensive to apply: the
    /// constrained bones themselves have to be recomposed, not just whatever
    /// hangs off them.
    fn apply_transform_absolute_local(&mut self, constraint: &TransformConstraint, mix: TransformPose) {
        let t = self.bones[constraint.target.index()].local;

        let mut lowest = usize::MAX;
        for bone_id in &constraint.bones {
            let i = bone_id.index();
            let Some(pose) = self.bones.get_mut(i) else { continue };
            let mut l = pose.local;

            if mix.mix_rotate != 0.0 {
                let r = wrap_degrees(t.rotation - l.rotation + constraint.offset_rotation);
                l.rotation += r * mix.mix_rotate;
            }
            l.position.x += (t.position.x - l.position.x + constraint.offset_x) * mix.mix_x;
            l.position.y += (t.position.y - l.position.y + constraint.offset_y) * mix.mix_y;
            if mix.mix_scale_x != 0.0 {
                l.scale.x += (t.scale.x - l.scale.x + constraint.offset_scale_x) * mix.mix_scale_x;
            }
            if mix.mix_scale_y != 0.0 {
                l.scale.y += (t.scale.y - l.scale.y + constraint.offset_scale_y) * mix.mix_scale_y;
            }
            if mix.mix_shear_y != 0.0 {
                let r = wrap_degrees(t.shear.y - l.shear.y + constraint.offset_shear_y);
                l.shear.y += r * mix.mix_shear_y;
            }

            pose.local = l;
            lowest = lowest.min(i);
        }

        if lowest != usize::MAX {
            self.update_world_from(lowest);
        }
    }

    /// Adds the local transform of the target to the one the bone already has.
    fn apply_transform_relative_local(&mut self, constraint: &TransformConstraint, mix: TransformPose) {
        let t = self.bones[constraint.target.index()].local;

        let mut lowest = usize::MAX;
        for bone_id in &constraint.bones {
            let i = bone_id.index();
            let Some(pose) = self.bones.get_mut(i) else { continue };
            let mut l = pose.local;

            l.rotation += (t.rotation + constraint.offset_rotation) * mix.mix_rotate;
            l.position.x += (t.position.x + constraint.offset_x) * mix.mix_x;
            l.position.y += (t.position.y + constraint.offset_y) * mix.mix_y;
            // Scale composes by multiplication, so the neutral value is 1 and
            // what the target contributes is its deviation from it.
            l.scale.x *= (t.scale.x - 1.0 + constraint.offset_scale_x) * mix.mix_scale_x + 1.0;
            l.scale.y *= (t.scale.y - 1.0 + constraint.offset_scale_y) * mix.mix_scale_y + 1.0;
            l.shear.y += (t.shear.y + constraint.offset_shear_y) * mix.mix_shear_y;

            pose.local = l;
            lowest = lowest.min(i);
        }

        if lowest != usize::MAX {
            self.update_world_from(lowest);
        }
    }

    // ------------------------------------------------------------ path

    /// Places a chain of bones along a path attachment.
    ///
    /// The path is whatever the target slot currently has attached, so it moves
    /// with the skeleton and deforms with it. A slot with no path attached is
    /// not an error: a skin may legitimately leave it off, and the constraint
    /// then has nothing to follow.
    fn apply_path_constraint(&mut self, index: usize) {
        let Some(constraint) = self.ir.path_constraints.get(index).cloned() else { return };
        let Some(pose) = self.path.get(index).copied() else { return };
        if constraint.bones.is_empty() || (pose.mix_rotate == 0.0 && pose.mix_x == 0.0 && pose.mix_y == 0.0) {
            return;
        }

        // Everything is read from the pose before anything is written back, so
        // the whole chain is placed against one consistent skeleton.
        let Some((geometry, slot_bone)) = self.path_geometry(&constraint) else { return };
        if geometry.is_empty() {
            self.note_unsupported("path attachment with too few control points");
            return;
        }

        let tangents = constraint.rotate_mode == PathRotateMode::Tangent;
        let chain_scale = constraint.rotate_mode == PathRotateMode::ChainScale;
        let bone_count = constraint.bones.len();
        // The chain modes aim each bone at where the next one sits, so they need
        // one sample beyond the last bone. Tangent mode reads the direction of
        // the path itself and does not.
        let sample_count = if tangents { bone_count } else { bone_count + 1 };

        // Setup lengths, and the same lengths under the current world scale.
        let setup_lengths: Vec<f32> =
            constraint.bones.iter().map(|b| self.ir.bones.get(b.index()).map_or(0.0, |x| x.length)).collect();
        let world_lengths: Vec<f32> = constraint
            .bones
            .iter()
            .zip(&setup_lengths)
            .map(|(id, setup)| {
                if *setup < PATH_EPSILON {
                    return 0.0;
                }
                let w = self.bones[id.index()].world;
                Vec2::new(setup * w.a, setup * w.c).length()
            })
            .collect();

        let path_length = geometry.length();
        let spaces = path_spaces(&constraint, pose.spacing, path_length, sample_count, &setup_lengths, &world_lengths);

        // Sample the path once per bone, walking forward by the gaps.
        let mut distance = match constraint.position_mode {
            PathPositionMode::Percent => pose.position * path_length,
            PathPositionMode::Fixed => pose.position,
        };
        let mut samples = Vec::with_capacity(sample_count);
        for space in spaces.iter().take(sample_count) {
            distance += space;
            let Some(sample) = geometry.sample(distance) else { return };
            samples.push(sample);
        }

        // A non-zero offset rotates the bone away from the path, and reflects
        // with the slot so a mirrored skeleton bends the same way.
        let offset_rotation = if constraint.offset_rotation == 0.0 {
            0.0
        } else {
            let reflect = if slot_bone.a * slot_bone.d - slot_bone.b * slot_bone.c > 0.0 { 1.0f32 } else { -1.0 };
            constraint.offset_rotation.to_radians() * reflect
        };
        // With no offset, a chain also drags the next bone onto its own tip
        // instead of onto the path, which is what keeps the chain joined up
        // rather than merely parallel to the path.
        let tip = constraint.offset_rotation == 0.0 && constraint.rotate_mode == PathRotateMode::Chain;

        let mut here = samples[0].point;
        let mut lowest = usize::MAX;
        for i in 0..bone_count {
            let bone_index = constraint.bones[i].index();
            let Some(bone) = self.bones.get_mut(bone_index) else { continue };
            let mut w = bone.world;

            w.tx += (here.x - w.tx) * pose.mix_x;
            w.ty += (here.y - w.ty) * pose.mix_y;

            // Where the next bone goes, before any tip correction. Tangent
            // mode samples one point per bone and so has none to spare past the
            // last one, which only the chain modes would have read anyway.
            let mut next = samples.get(i + 1).map_or(here, |s| s.point);
            let delta = next - here;

            if pose.mix_rotate != 0.0 {
                // A chain aims at the next sample; a degenerate gap has no
                // direction of its own, so the path tangent stands in.
                let aim = if tangents {
                    // Tangent mode follows the path itself rather than the
                    // shape of the chain.
                    samples[i].angle
                } else if delta.length_squared() < PATH_EPSILON * PATH_EPSILON {
                    // Two samples coincide, so there is no direction between
                    // them; the path's own direction stands in.
                    samples.get(i + 1).map_or(samples[i].angle, |s| s.angle)
                } else {
                    delta.angle_rad()
                };

                if chain_scale && world_lengths[i] > PATH_EPSILON {
                    let s = (delta.length() / world_lengths[i] - 1.0) * pose.mix_rotate + 1.0;
                    w.a *= s;
                    w.c *= s;
                }

                let mut r = aim - w.c.atan2(w.a);
                if tip {
                    // Blend the next position between the path sample and where
                    // this bone's tip actually lands once rotated.
                    let (sin, cos) = r.sin_cos();
                    let length = setup_lengths[i];
                    let point = Vec2::new(length * (cos * w.a - sin * w.c), length * (sin * w.a + cos * w.c));
                    next += (point - delta) * pose.mix_rotate;
                } else {
                    r += offset_rotation;
                }
                r = wrap_radians(r) * pose.mix_rotate;

                let (sin, cos) = r.sin_cos();
                let (a, b, c, d) = (w.a, w.b, w.c, w.d);
                w.a = cos * a - sin * c;
                w.b = cos * b - sin * d;
                w.c = sin * a + cos * c;
                w.d = sin * b + cos * d;
            }

            bone.world = w;
            lowest = lowest.min(bone_index);
            here = next;
        }

        self.rebuild_below(&constraint.bones, lowest);
    }

    /// Resolves the path a constraint follows and measures it in world space.
    fn path_geometry(&self, constraint: &PathConstraint) -> Option<(PathGeometry, Affine2)> {
        let slot_index = constraint.target_slot.index();
        let slot_pose = self.slots.get(slot_index)?;
        let attachment = self.ir.attachment(slot_pose.attachment?)?;
        let AttachmentKind::Path(path) = &attachment.kind else { return None };
        let slot_bone = self.bones.get(self.ir.slots.get(slot_index)?.bone.index())?.world;
        let world = self.world_vertices(&path.vertices, &slot_pose.deform, slot_bone);
        let geometry = PathGeometry::measure(&world, path.closed, path.constant_speed, &path.lengths);
        Some((geometry, slot_bone))
    }

    /// Rebuilds everything below a set of bones whose world matrices were
    /// written directly, leaving those bones as the constraint left them.
    fn rebuild_below(&mut self, bones: &[BoneId], lowest: usize) {
        if lowest == usize::MAX {
            return;
        }
        let highest = bones.iter().map(|b| b.index()).max().unwrap_or(lowest);
        self.update_world_from(highest + 1);
    }

    /// World-space bounds of the current pose's bone origins.
    ///
    /// Attachment geometry gives tighter bounds; this is the cheap version used
    /// before anything is emitted.
    pub fn bone_bounds(&self) -> a2d_core::Aabb {
        let mut b = a2d_core::Aabb::EMPTY;
        for bone in &self.bones {
            b.extend(bone.world.translation());
        }
        b
    }
}

/// The non-uniform-parent-scale branch of the two-bone solver.
///
/// Split out because the reference implementation uses a labelled break that
/// has no direct Rust equivalent.
#[allow(clippy::too_many_arguments)]
fn solve_two_bone_nonuniform(
    psx: f32,
    psy: f32,
    l1: f32,
    l2: f32,
    tx: f32,
    ty: f32,
    dd: f32,
    bend_dir: f32,
) -> (f32, f32) {
    let a = psx * l2;
    let b = psy * l2;
    let aa = a * a;
    let bb = b * b;
    let ta = ty.atan2(tx);
    let c = bb * l1 * l1 + aa * dd - aa * bb;
    let c1 = -2.0 * bb * l1;
    let c2 = bb - aa;
    let d = c1 * c1 - 4.0 * c2 * c;

    if d >= 0.0 && c2 != 0.0 {
        let mut q = d.sqrt();
        if c1 < 0.0 {
            q = -q;
        }
        q = -(c1 + q) / 2.0;
        if q != 0.0 {
            let r0 = q / c2;
            let r1 = c / q;
            let r = if r0.abs() < r1.abs() { r0 } else { r1 };
            if r * r <= dd {
                let y = (dd - r * r).sqrt() * bend_dir;
                return (ta - y.atan2(r), (y / psy).atan2((r - l1) / psx));
            }
        }
    }

    // No exact solution: pick whichever extreme of the reachable arc is closer
    // to the target distance, exactly as the reference does.
    let mut min_angle = std::f32::consts::PI;
    let mut min_x = l1 - a;
    let mut min_dist = min_x * min_x;
    let mut min_y = 0.0f32;
    let mut max_angle = 0.0f32;
    let mut max_x = l1 + a;
    let mut max_dist = max_x * max_x;
    let mut max_y = 0.0f32;

    if aa - bb != 0.0 {
        let c = -a * l1 / (aa - bb);
        if (-1.0..=1.0).contains(&c) {
            let c = c.acos();
            let x = a * c.cos() + l1;
            let y = b * c.sin();
            let d = x * x + y * y;
            if d < min_dist {
                min_angle = c;
                min_dist = d;
                min_x = x;
                min_y = y;
            }
            if d > max_dist {
                max_angle = c;
                max_dist = d;
                max_x = x;
                max_y = y;
            }
        }
    }

    if dd <= (min_dist + max_dist) / 2.0 {
        (ta - (min_y * bend_dir).atan2(min_x), min_angle * bend_dir)
    } else {
        (ta - (max_y * bend_dir).atan2(max_x), max_angle * bend_dir)
    }
}

/// Distances below this are treated as zero.
const PATH_EPSILON: f32 = 1e-5;
/// Samples per curve used to build a constant-speed arc-length table.
///
/// Sixteen is enough that the residual error is far below a pixel for the curve
/// sizes a character rig uses, and the table is built once per frame per path.
const PATH_SAMPLES: usize = 16;

/// One place on a path: where it is, and which way the path points there.
#[derive(Debug, Clone, Copy, PartialEq)]
struct PathSample {
    point: Vec2,
    angle: f32,
}

/// A path attachment measured in world space, ready to be sampled by distance.
struct PathGeometry {
    curves: Vec<[Vec2; 4]>,
    /// Cumulative length at the end of each curve.
    ends: Vec<f32>,
    /// Per-curve arc-length tables, cumulative within the curve. Empty when the
    /// authored lengths are used instead of measured ones.
    tables: Vec<Vec<f32>>,
    closed: bool,
}

impl PathGeometry {
    fn measure(world: &[Vec2], closed: bool, constant_speed: bool, authored: &[f32]) -> PathGeometry {
        let curves = path_curves(world, closed);
        let mut ends = Vec::with_capacity(curves.len());
        let mut tables = Vec::new();

        // A path authored without constant speed carries its own cumulative
        // lengths and is walked by curve parameter; one with constant speed has
        // to be measured, so that equal distances are equal along the curve
        // rather than equal in parameter.
        if constant_speed || authored.len() < curves.len() {
            let mut total = 0.0;
            for curve in &curves {
                let mut table = Vec::with_capacity(PATH_SAMPLES + 1);
                table.push(0.0);
                let mut run = 0.0;
                let mut previous = bezier_point(curve, 0.0);
                for step in 1..=PATH_SAMPLES {
                    let point = bezier_point(curve, step as f32 / PATH_SAMPLES as f32);
                    run += (point - previous).length();
                    table.push(run);
                    previous = point;
                }
                total += run;
                ends.push(total);
                tables.push(table);
            }
        } else {
            ends.extend_from_slice(&authored[..curves.len()]);
        }

        PathGeometry { curves, ends, tables, closed }
    }

    fn is_empty(&self) -> bool {
        self.curves.is_empty()
    }

    fn length(&self) -> f32 {
        self.ends.last().copied().unwrap_or(0.0)
    }

    /// The place `distance` along the path.
    ///
    /// A closed path wraps. An open one extends along its end tangents, so a
    /// bone pushed past either end keeps going in a straight line rather than
    /// piling up at the last control point.
    fn sample(&self, distance: f32) -> Option<PathSample> {
        let total = self.length();
        let mut distance = distance;

        if self.closed {
            if total <= PATH_EPSILON {
                return None;
            }
            distance = distance.rem_euclid(total);
        } else if distance < 0.0 {
            let curve = self.curves.first()?;
            let direction = unit_tangent(curve, 0.0);
            return Some(PathSample {
                point: bezier_point(curve, 0.0) + direction * distance,
                angle: direction.angle_rad(),
            });
        } else if distance > total {
            let curve = self.curves.last()?;
            let direction = unit_tangent(curve, 1.0);
            return Some(PathSample {
                point: bezier_point(curve, 1.0) + direction * (distance - total),
                angle: direction.angle_rad(),
            });
        }

        let mut index = 0;
        while index + 1 < self.ends.len() && distance > self.ends[index] {
            index += 1;
        }
        let start = if index == 0 { 0.0 } else { self.ends[index - 1] };
        let span = self.ends[index] - start;
        let fraction = if span > PATH_EPSILON { (distance - start) / span } else { 0.0 };

        let curve = self.curves.get(index)?;
        let t = match self.tables.get(index) {
            Some(table) => arc_length_to_t(table, fraction),
            None => fraction,
        };
        Some(PathSample { point: bezier_point(curve, t), angle: unit_tangent(curve, t).angle_rad() })
    }
}

/// Splits a path attachment's control points into cubic segments.
///
/// Anchors sit at vertices 1, 4, 7, ..., with two handles between each pair.
/// Vertex 0 and the final vertex are the leading and trailing handles: unused
/// by an open path, and the handles of the wrapping segment when closed.
fn path_curves(world: &[Vec2], closed: bool) -> Vec<[Vec2; 4]> {
    let count = world.len();
    let mut curves = Vec::new();
    if count < 3 {
        return curves;
    }
    let mut i = 1;
    while i + 3 < count {
        curves.push([world[i], world[i + 1], world[i + 2], world[i + 3]]);
        i += 3;
    }
    if closed {
        curves.push([world[count - 2], world[count - 1], world[0], world[1]]);
    }
    curves
}

/// The gap in world units before each sample, with `spaces[0]` always zero so
/// the first bone lands exactly on the position the constraint asks for.
fn path_spaces(
    constraint: &PathConstraint,
    spacing: f32,
    path_length: f32,
    sample_count: usize,
    setup_lengths: &[f32],
    world_lengths: &[f32],
) -> Vec<f32> {
    let mut spaces = vec![0.0f32; sample_count];
    // `spaces[0]` stays zero: the gaps sit *before* each sample, so the first
    // bone lands exactly on the position the constraint asks for.
    for (b, space) in spaces.iter_mut().enumerate().skip(1).map(|(i, s)| (i - 1, s)) {
        let (setup, world) =
            (setup_lengths.get(b).copied().unwrap_or(0.0), world_lengths.get(b).copied().unwrap_or(0.0));
        *space = match constraint.spacing_mode {
            // A fraction of the whole path, so a chain keeps its share of the
            // path as the path grows or shrinks.
            PathSpacingMode::Percent => spacing * path_length,
            // Proportional to bone length, normalised below.
            PathSpacingMode::Proportional => world,
            // World units, scaled by how much the bone itself is scaled.
            _ if setup < PATH_EPSILON => spacing,
            PathSpacingMode::Length => (setup + spacing) * world / setup,
            PathSpacingMode::Fixed => spacing * world / setup,
        };
    }

    if constraint.spacing_mode == PathSpacingMode::Proportional {
        // Spread the bones over `spacing` of the path, sharing it out in
        // proportion to their lengths.
        let sum: f32 = spaces.iter().sum();
        let factor = if sum > PATH_EPSILON { spacing * path_length / sum } else { 0.0 };
        for space in spaces.iter_mut() {
            *space *= factor;
        }
    }
    spaces
}

/// Maps a fraction of a curve's arc length to its Bézier parameter.
fn arc_length_to_t(table: &[f32], fraction: f32) -> f32 {
    let steps = table.len().saturating_sub(1);
    let total = table.last().copied().unwrap_or(0.0);
    if steps == 0 || total <= PATH_EPSILON {
        return fraction;
    }
    let target = fraction.clamp(0.0, 1.0) * total;
    // The table is monotonic, so walking it and interpolating inside the
    // bracketing pair is both correct and cheap at this resolution.
    for i in 1..=steps {
        if table[i] >= target {
            let span = table[i] - table[i - 1];
            let within = if span > PATH_EPSILON { (target - table[i - 1]) / span } else { 0.0 };
            return (i as f32 - 1.0 + within) / steps as f32;
        }
    }
    1.0
}

fn bezier_point(c: &[Vec2; 4], t: f32) -> Vec2 {
    let u = 1.0 - t;
    c[0] * (u * u * u) + c[1] * (3.0 * u * u * t) + c[2] * (3.0 * u * t * t) + c[3] * (t * t * t)
}

/// Unit direction of the curve at `t`.
///
/// A cubic can have a zero-length derivative where two control points coincide,
/// which is common at the ends of an authored path; the neighbouring chord
/// stands in there so the direction is never arbitrary.
fn unit_tangent(c: &[Vec2; 4], t: f32) -> Vec2 {
    let u = 1.0 - t;
    let d = (c[1] - c[0]) * (3.0 * u * u) + (c[2] - c[1]) * (6.0 * u * t) + (c[3] - c[2]) * (3.0 * t * t);
    if d.length() > PATH_EPSILON {
        return d / d.length();
    }
    let chord = c[3] - c[0];
    if chord.length() > PATH_EPSILON {
        return chord / chord.length();
    }
    Vec2::new(1.0, 0.0)
}

/// Wraps degrees into `[-180, 180)`.
///
/// The local modes interpolate rotations in degrees, and without this a
/// constraint would take the long way round whenever the two angles straddle
/// the wrap point.
fn wrap_degrees(r: f32) -> f32 {
    r - (r / 360.0 + 0.5).floor() * 360.0
}

#[inline]
fn wrap_radians(mut r: f32) -> f32 {
    const PI2: f32 = std::f32::consts::PI * 2.0;
    if r > std::f32::consts::PI {
        r -= PI2;
    } else if r < -std::f32::consts::PI {
        r += PI2;
    }
    r
}

#[cfg(test)]
mod tests {
    use super::*;
    use a2d_core::ir::spine::{Attachment, Bone, IkConstraint, PathAttachment, Skin, SkinEntry, Slot};

    fn assert_close(a: f32, b: f32, what: &str) {
        assert!((a - b).abs() < 1e-3, "{what}: {a} != {b}");
    }

    fn chain_ir(bones: Vec<Bone>) -> Arc<SpineIr> {
        let mut ir = SpineIr { bones, ..Default::default() };
        ir.rebuild_derived();
        Arc::new(ir)
    }

    fn bone(name: &str, parent: Option<u16>, local: BoneLocal, length: f32) -> Bone {
        Bone { length, setup: local, ..Bone::new(name, parent.map(BoneId)) }
    }

    fn at(x: f32, y: f32) -> BoneLocal {
        BoneLocal { position: Vec2::new(x, y), ..BoneLocal::default() }
    }

    // ---------------------------------------------------- transform constraints
    //
    // These pin down the four modes by the property that distinguishes each,
    // not by numbers copied from a reference run: absolute replaces, relative
    // adds, local works in the space the bone was authored in, world works on
    // the composed matrix. Spec §17.4 cross-implementation comparison against
    // the official runtime is still outstanding, and would be the thing to
    // catch a term that is subtly in the wrong place.

    struct Tc {
        local: bool,
        relative: bool,
        target: BoneLocal,
        constrained: BoneLocal,
        mix: f32,
        mix_shear_y: f32,
        child: bool,
    }

    impl Tc {
        fn new(local: bool, relative: bool) -> Self {
            Tc {
                local,
                relative,
                target: BoneLocal::default(),
                constrained: BoneLocal::default(),
                mix: 1.0,
                mix_shear_y: 0.0,
                child: false,
            }
        }

        /// Bone 0 is the root, bone 1 the target, bone 2 the constrained bone,
        /// and bone 3 its child when asked for. The target precedes the
        /// constrained bone, which is the order a real skeleton is sorted into.
        fn pose(self) -> SkeletonPose {
            let mut bones = vec![
                bone("root", None, BoneLocal::default(), 0.0),
                bone("target", Some(0), self.target, 0.0),
                bone("bone", Some(0), self.constrained, 0.0),
            ];
            if self.child {
                bones.push(bone("child", Some(2), at(10.0, 0.0), 0.0));
            }
            let mut ir = SpineIr { bones, ..Default::default() };
            ir.transform_constraints.push(TransformConstraint {
                name: "tc".into(),
                order: 0,
                skin_required: false,
                bones: vec![BoneId(2)],
                target: BoneId(1),
                offset_rotation: 0.0,
                offset_x: 0.0,
                offset_y: 0.0,
                offset_scale_x: 0.0,
                offset_scale_y: 0.0,
                offset_shear_y: 0.0,
                mix_rotate: self.mix,
                mix_x: self.mix,
                mix_y: self.mix,
                mix_scale_x: self.mix,
                mix_scale_y: self.mix,
                mix_shear_y: self.mix_shear_y,
                relative: self.relative,
                local: self.local,
            });
            ir.rebuild_derived();
            SkeletonPose::new(Arc::new(ir))
        }
    }

    fn spun(rotation: f32) -> BoneLocal {
        BoneLocal { rotation, ..BoneLocal::default() }
    }

    #[test]
    fn a_zero_mix_leaves_every_mode_alone() {
        for (local, relative) in [(false, false), (false, true), (true, false), (true, true)] {
            let mut tc = Tc::new(local, relative);
            tc.target = BoneLocal {
                position: Vec2::new(50.0, 60.0),
                rotation: 40.0,
                scale: Vec2::new(3.0, 3.0),
                shear: Vec2::ZERO,
            };
            tc.constrained = at(7.0, 8.0);
            tc.mix = 0.0;
            let pose = tc.pose();
            let p = pose.bones[2].world.translation();
            assert_close(p.x, 7.0, "x");
            assert_close(p.y, 8.0, "y");
            assert_close(pose.bones[2].world.rotation_x_rad(), 0.0, "rotation");
        }
    }

    #[test]
    fn absolute_world_matches_the_rotation_of_the_target() {
        let mut tc = Tc::new(false, false);
        tc.target = spun(30.0);
        tc.constrained = spun(20.0);
        let pose = tc.pose();
        assert_close(pose.bones[2].world.rotation_x_rad().to_degrees(), 30.0, "matched");
    }

    #[test]
    fn relative_world_adds_the_rotation_of_the_target() {
        let mut tc = Tc::new(false, true);
        tc.target = spun(30.0);
        tc.constrained = spun(20.0);
        let pose = tc.pose();
        // 20 of its own plus 30 from the target, where absolute mode would
        // have landed on 30 exactly.
        assert_close(pose.bones[2].world.rotation_x_rad().to_degrees(), 50.0, "added");
    }

    #[test]
    fn absolute_local_copies_the_local_transform_of_the_target() {
        let mut tc = Tc::new(true, false);
        tc.target =
            BoneLocal { position: Vec2::new(5.0, 7.0), rotation: 30.0, scale: Vec2::new(2.0, 3.0), shear: Vec2::ZERO };
        tc.constrained =
            BoneLocal { position: Vec2::new(1.0, 2.0), rotation: 10.0, scale: Vec2::new(4.0, 5.0), shear: Vec2::ZERO };
        let pose = tc.pose();
        let l = pose.bones[2].local;
        assert_close(l.rotation, 30.0, "rotation");
        assert_close(l.position.x, 5.0, "x");
        assert_close(l.position.y, 7.0, "y");
        assert_close(l.scale.x, 2.0, "scale x");
        assert_close(l.scale.y, 3.0, "scale y");
        // The world transform must have been rebuilt from the new local one.
        assert_close(pose.bones[2].world.translation().x, 5.0, "world x");
    }

    #[test]
    fn relative_local_adds_the_local_transform_of_the_target() {
        let mut tc = Tc::new(true, true);
        tc.target =
            BoneLocal { position: Vec2::new(5.0, 7.0), rotation: 30.0, scale: Vec2::new(3.0, 1.0), shear: Vec2::ZERO };
        tc.constrained =
            BoneLocal { position: Vec2::new(1.0, 2.0), rotation: 10.0, scale: Vec2::new(2.0, 1.0), shear: Vec2::ZERO };
        let pose = tc.pose();
        let l = pose.bones[2].local;
        assert_close(l.rotation, 40.0, "rotation adds");
        assert_close(l.position.x, 6.0, "x adds");
        assert_close(l.position.y, 9.0, "y adds");
        // Scale composes by multiplication: 2 * ((3 - 1) * 1 + 1).
        assert_close(l.scale.x, 6.0, "scale multiplies");
        assert_close(l.scale.y, 1.0, "a neutral target scale changes nothing");
    }

    #[test]
    fn half_a_mix_lands_half_way_in_local_mode() {
        let mut tc = Tc::new(true, false);
        tc.target = spun(40.0);
        tc.constrained = spun(20.0);
        tc.mix = 0.5;
        let pose = tc.pose();
        assert_close(pose.bones[2].local.rotation, 30.0, "half way");
    }

    #[test]
    fn a_local_mode_constraint_carries_its_children_with_it() {
        // Regression: local modes write local transforms, so the constrained
        // bone itself has to be recomposed and not only its descendants. A
        // rebuild that started one bone too late would leave the child behind.
        let mut tc = Tc::new(true, false);
        tc.target = spun(90.0);
        tc.child = true;
        let pose = tc.pose();
        let p = pose.bones[3].world.translation();
        assert_close(p.x, 0.0, "child x");
        assert_close(p.y, 10.0, "child y");
    }

    #[test]
    fn the_local_modes_take_the_short_way_round() {
        // 350 and 10 degrees are 20 apart, not 340.
        let mut tc = Tc::new(true, false);
        tc.target = spun(10.0);
        tc.constrained = spun(350.0);
        tc.mix = 0.5;
        let pose = tc.pose();
        assert_close(pose.bones[2].local.rotation, 360.0, "halfway the short way");
    }

    #[test]
    fn wrapping_degrees_picks_the_nearer_direction() {
        assert_close(wrap_degrees(0.0), 0.0, "zero");
        assert_close(wrap_degrees(90.0), 90.0, "inside");
        assert_close(wrap_degrees(190.0), -170.0, "just over");
        assert_close(wrap_degrees(-190.0), 170.0, "just under");
        assert_close(wrap_degrees(710.0), -10.0, "twice round");
    }

    #[test]
    fn no_mode_is_reported_as_unsupported_any_more() {
        for (local, relative) in [(false, false), (false, true), (true, false), (true, true)] {
            let mut tc = Tc::new(local, relative);
            tc.target = spun(30.0);
            let pose = tc.pose();
            assert!(pose.degradations().is_empty(), "local={local} relative={relative}: {:?}", pose.degradations());
        }
    }

    #[test]
    fn a_lone_root_sits_at_its_local_position() {
        let ir = chain_ir(vec![bone("root", None, at(10.0, 20.0), 0.0)]);
        let pose = SkeletonPose::new(ir);
        assert_eq!(pose.bones[0].world.translation(), Vec2::new(10.0, 20.0));
    }

    #[test]
    fn a_child_inherits_its_parents_translation() {
        let ir = chain_ir(vec![bone("root", None, at(10.0, 0.0), 0.0), bone("a", Some(0), at(5.0, 0.0), 0.0)]);
        let pose = SkeletonPose::new(ir);
        assert_eq!(pose.bones[1].world.translation(), Vec2::new(15.0, 0.0));
    }

    #[test]
    fn a_child_inherits_its_parents_rotation() {
        let root = BoneLocal { rotation: 90.0, ..BoneLocal::default() };
        let ir = chain_ir(vec![bone("root", None, root, 0.0), bone("a", Some(0), at(10.0, 0.0), 0.0)]);
        let pose = SkeletonPose::new(ir);
        let p = pose.bones[1].world.translation();
        assert_close(p.x, 0.0, "x");
        assert_close(p.y, 10.0, "y");
    }

    #[test]
    fn a_child_inherits_its_parents_scale() {
        let root = BoneLocal { scale: Vec2::new(2.0, 3.0), ..BoneLocal::default() };
        let ir = chain_ir(vec![bone("root", None, root, 0.0), bone("a", Some(0), at(10.0, 10.0), 0.0)]);
        let pose = SkeletonPose::new(ir);
        let p = pose.bones[1].world.translation();
        assert_close(p.x, 20.0, "x");
        assert_close(p.y, 30.0, "y");
    }

    #[test]
    fn only_translation_mode_ignores_parent_rotation() {
        let root = BoneLocal { rotation: 90.0, ..BoneLocal::default() };
        let mut child = bone("a", Some(0), at(10.0, 0.0), 0.0);
        child.inherit = TransformInherit::OnlyTranslation;
        let ir = chain_ir(vec![bone("root", None, root, 0.0), child]);
        let pose = SkeletonPose::new(ir);
        // The origin still moves with the parent...
        let p = pose.bones[1].world.translation();
        assert_close(p.x, 0.0, "x");
        assert_close(p.y, 10.0, "y");
        // ...but the axes do not rotate.
        assert_close(pose.bones[1].world.rotation_x_rad(), 0.0, "rotation");
    }

    #[test]
    fn no_scale_mode_ignores_parent_scale() {
        let root = BoneLocal { scale: Vec2::new(4.0, 4.0), ..BoneLocal::default() };
        let mut child = bone("a", Some(0), at(1.0, 0.0), 0.0);
        child.inherit = TransformInherit::NoScale;
        let ir = chain_ir(vec![bone("root", None, root, 0.0), child]);
        let pose = SkeletonPose::new(ir);
        // Position still scales, because it comes from the parent's matrix.
        assert_close(pose.bones[1].world.tx, 4.0, "x");
        // The child's own axes stay unit length.
        let s = pose.bones[1].world.scale();
        assert_close(s.x, 1.0, "scale x");
        assert_close(s.y, 1.0, "scale y");
    }

    #[test]
    fn skeleton_scale_flips_the_whole_rig() {
        let ir = chain_ir(vec![bone("root", None, at(10.0, 20.0), 0.0), bone("a", Some(0), at(5.0, 0.0), 0.0)]);
        let mut pose = SkeletonPose::new(ir);
        pose.scale = Vec2::new(-1.0, 1.0);
        pose.update_world_transforms();
        assert_close(pose.bones[0].world.tx, -10.0, "root x");
        assert_close(pose.bones[1].world.tx, -15.0, "child x");
        assert_close(pose.bones[1].world.ty, 20.0, "child y");
    }

    #[test]
    fn skeleton_position_offsets_the_root() {
        let ir = chain_ir(vec![bone("root", None, at(0.0, 0.0), 0.0)]);
        let mut pose = SkeletonPose::new(ir);
        pose.position = Vec2::new(100.0, -50.0);
        pose.update_world_transforms();
        assert_eq!(pose.bones[0].world.translation(), Vec2::new(100.0, -50.0));
    }

    /// Root at the origin, a 100-long bone pointing along +X, and a target.
    fn one_bone_ik(target: Vec2, mix: f32) -> SkeletonPose {
        let mut ir = SpineIr {
            bones: vec![
                bone("root", None, BoneLocal::default(), 0.0),
                bone("arm", Some(0), BoneLocal::default(), 100.0),
                bone("target", Some(0), at(target.x, target.y), 0.0),
            ],
            ..Default::default()
        };
        ir.ik_constraints.push(IkConstraint {
            name: "ik".into(),
            order: 0,
            skin_required: false,
            bones: vec![BoneId(1)],
            target: BoneId(2),
            mix,
            softness: 0.0,
            bend_positive: true,
            compress: false,
            stretch: false,
            uniform: false,
        });
        ir.rebuild_derived();
        SkeletonPose::new(Arc::new(ir))
    }

    #[test]
    fn one_bone_ik_points_the_bone_at_the_target() {
        let pose = one_bone_ik(Vec2::new(0.0, 50.0), 1.0);
        // The bone should now point straight up.
        assert_close(pose.bones[1].world.rotation_x_rad().to_degrees(), 90.0, "rotation");
    }

    #[test]
    fn one_bone_ik_points_backwards_when_the_target_is_behind() {
        let pose = one_bone_ik(Vec2::new(-50.0, 0.0), 1.0);
        let deg = pose.bones[1].world.rotation_x_rad().to_degrees();
        assert!((deg.abs() - 180.0).abs() < 1e-3, "expected +/-180, got {deg}");
    }

    #[test]
    fn one_bone_ik_with_zero_mix_leaves_the_bone_alone() {
        let pose = one_bone_ik(Vec2::new(0.0, 50.0), 0.0);
        assert_close(pose.bones[1].world.rotation_x_rad(), 0.0, "rotation");
    }

    #[test]
    fn one_bone_ik_with_half_mix_rotates_half_way() {
        let pose = one_bone_ik(Vec2::new(0.0, 50.0), 0.5);
        assert_close(pose.bones[1].world.rotation_x_rad().to_degrees(), 45.0, "rotation");
    }

    /// A two-bone chain of 100 + 100 along +X, plus a target bone.
    fn two_bone_ik(target: Vec2, bend_positive: bool) -> SkeletonPose {
        let mut ir = SpineIr {
            bones: vec![
                bone("root", None, BoneLocal::default(), 0.0),
                bone("upper", Some(0), BoneLocal::default(), 100.0),
                bone("lower", Some(1), at(100.0, 0.0), 100.0),
                bone("target", Some(0), at(target.x, target.y), 0.0),
            ],
            ..Default::default()
        };
        ir.ik_constraints.push(IkConstraint {
            name: "ik".into(),
            order: 0,
            skin_required: false,
            bones: vec![BoneId(1), BoneId(2)],
            target: BoneId(3),
            mix: 1.0,
            softness: 0.0,
            bend_positive,
            compress: false,
            stretch: false,
            uniform: false,
        });
        ir.rebuild_derived();
        SkeletonPose::new(Arc::new(ir))
    }

    /// World position of the chain's end effector.
    fn end_effector(pose: &SkeletonPose) -> Vec2 {
        pose.bones[2].world.transform_point(Vec2::new(pose.ir().bones[2].length, 0.0))
    }

    #[test]
    fn two_bone_ik_reaches_a_target_inside_its_range() {
        let target = Vec2::new(100.0, 100.0);
        let pose = two_bone_ik(target, true);
        let tip = end_effector(&pose);
        assert!((tip - target).length() < 0.5, "tip {tip:?} should reach {target:?}");
    }

    #[test]
    fn two_bone_ik_reaches_a_target_behind_the_root() {
        let target = Vec2::new(-80.0, 60.0);
        let pose = two_bone_ik(target, true);
        let tip = end_effector(&pose);
        assert!((tip - target).length() < 0.5, "tip {tip:?} should reach {target:?}");
    }

    #[test]
    fn two_bone_ik_straightens_towards_an_unreachable_target() {
        let target = Vec2::new(500.0, 0.0);
        let pose = two_bone_ik(target, true);
        let tip = end_effector(&pose);
        // It cannot reach 500, but it should extend to its full 200 length.
        assert_close(tip.length(), 200.0, "extended length");
        assert!(tip.x > 199.0, "should point at the target, got {tip:?}");
    }

    #[test]
    fn the_bend_direction_selects_between_the_two_solutions() {
        let target = Vec2::new(100.0, 100.0);
        let positive = two_bone_ik(target, true);
        let negative = two_bone_ik(target, false);
        // Both reach the target...
        assert!((end_effector(&positive) - target).length() < 0.5);
        assert!((end_effector(&negative) - target).length() < 0.5);
        // ...but the elbow is on opposite sides.
        let elbow_p = positive.bones[2].world.translation();
        let elbow_n = negative.bones[2].world.translation();
        assert!((elbow_p - elbow_n).length() > 1.0, "elbows should differ: {elbow_p:?} {elbow_n:?}");
    }

    #[test]
    fn ik_propagates_to_descendants_of_the_chain() {
        let target = Vec2::new(0.0, 200.0);
        let mut ir = SpineIr {
            bones: vec![
                bone("root", None, BoneLocal::default(), 0.0),
                bone("upper", Some(0), BoneLocal::default(), 100.0),
                bone("lower", Some(1), at(100.0, 0.0), 100.0),
                bone("hand", Some(2), at(100.0, 0.0), 0.0),
                bone("target", Some(0), at(target.x, target.y), 0.0),
            ],
            ..Default::default()
        };
        ir.ik_constraints.push(IkConstraint {
            name: "ik".into(),
            order: 0,
            skin_required: false,
            bones: vec![BoneId(1), BoneId(2)],
            target: BoneId(4),
            mix: 1.0,
            softness: 0.0,
            bend_positive: true,
            compress: false,
            stretch: false,
            uniform: false,
        });
        ir.rebuild_derived();
        let pose = SkeletonPose::new(Arc::new(ir));
        let hand = pose.bones[3].world.translation();
        assert!((hand - target).length() < 0.5, "the hand bone should follow the chain, got {hand:?}");
    }

    #[test]
    fn attachments_resolve_from_the_setup_pose() {
        use a2d_core::ir::spine::{Attachment, AttachmentKind, PointAttachment, Skin, SkinEntry};
        let mut ir = SpineIr {
            bones: vec![bone("root", None, BoneLocal::default(), 0.0)],
            slots: vec![Slot { setup_attachment: Some("shirt".into()), ..Slot::new("body", BoneId(0)) }],
            skins: vec![Skin::new("default")],
            attachments: vec![Attachment {
                name: "shirt".into(),
                kind: AttachmentKind::Point(PointAttachment {
                    position: Vec2::ZERO,
                    rotation: 0.0,
                    color: Rgba::WHITE,
                }),
            }],
            ..Default::default()
        };
        ir.skins[0].entries.push(SkinEntry { slot: SlotId(0), name: "shirt".into(), attachment: AttachmentId(0) });
        ir.rebuild_derived();
        let mut pose = SkeletonPose::new(Arc::new(ir));
        assert_eq!(pose.slots[0].attachment, Some(AttachmentId(0)));

        pose.set_slot_attachment(SlotId(0), None);
        assert_eq!(pose.slots[0].attachment, None);
        pose.set_slot_attachment(SlotId(0), Some("shirt"));
        assert_eq!(pose.slots[0].attachment, Some(AttachmentId(0)));
        pose.set_slot_attachment(SlotId(0), Some("nonexistent"));
        assert_eq!(pose.slots[0].attachment, None);
    }

    #[test]
    fn reset_restores_the_setup_pose_after_mutation() {
        let ir = chain_ir(vec![bone("root", None, at(1.0, 2.0), 0.0)]);
        let mut pose = SkeletonPose::new(ir);
        pose.bones[0].local.rotation = 45.0;
        pose.reset_to_setup();
        assert_eq!(pose.bones[0].local.rotation, 0.0);
        assert_eq!(pose.bones[0].local.position, Vec2::new(1.0, 2.0));
    }

    // ------------------------------------------------------- path constraints
    //
    // A straight path is the fixture of choice: its arc length is exactly the
    // distance between its ends, so where each bone should land can be worked
    // out by hand instead of copied from a reference run. The control-point
    // layout -- anchors at vertices 1, 4, 7, ... with the first and last
    // vertices as unused handles -- is inferred from the two places the
    // reference runtime indexes into it, and is what the §11 cross-check
    // against the official runtime would confirm.

    /// A straight horizontal path from `(0, 0)` to `(length, 0)` as one cubic,
    /// with handles placed so the parameterisation is uniform.
    fn straight_path(length: f32) -> Vec<Vec2> {
        vec![
            Vec2::new(-1.0, 0.0),               // leading handle, unused
            Vec2::new(0.0, 0.0),                // anchor: the start
            Vec2::new(length / 3.0, 0.0),       // handle
            Vec2::new(length * 2.0 / 3.0, 0.0), // handle
            Vec2::new(length, 0.0),             // anchor: the end
            Vec2::new(length + 1.0, 0.0),       // trailing handle, unused
        ]
    }

    struct Pc {
        vertices: Vec<Vec2>,
        closed: bool,
        constant_speed: bool,
        lengths: Vec<f32>,
        bones: usize,
        bone_length: f32,
        position: f32,
        spacing: f32,
        position_mode: PathPositionMode,
        spacing_mode: PathSpacingMode,
        rotate_mode: PathRotateMode,
        offset_rotation: f32,
        mix: f32,
    }

    impl Pc {
        fn new(bones: usize) -> Self {
            Pc {
                vertices: straight_path(100.0),
                closed: false,
                constant_speed: true,
                lengths: Vec::new(),
                bones,
                bone_length: 10.0,
                position: 0.0,
                spacing: 10.0,
                position_mode: PathPositionMode::Fixed,
                spacing_mode: PathSpacingMode::Fixed,
                rotate_mode: PathRotateMode::Tangent,
                offset_rotation: 0.0,
                mix: 1.0,
            }
        }

        /// Bone 0 carries the path slot; the constrained bones follow it.
        fn pose(self) -> SkeletonPose {
            let mut bones = vec![bone("root", None, BoneLocal::default(), 0.0)];
            for i in 0..self.bones {
                bones.push(bone(&format!("b{i}"), Some(0), BoneLocal::default(), self.bone_length));
            }
            let count = self.vertices.len();
            let mut ir = SpineIr {
                bones,
                slots: vec![Slot { setup_attachment: Some("route".into()), ..Slot::new("route", BoneId(0)) }],
                attachments: vec![Attachment {
                    name: "route".into(),
                    kind: AttachmentKind::Path(PathAttachment {
                        closed: self.closed,
                        constant_speed: self.constant_speed,
                        lengths: self.lengths.clone(),
                        vertices: VertexData::Rigid(self.vertices.clone()),
                        color: Rgba::WHITE,
                    }),
                }],
                skins: vec![Skin::new("default")],
                ..Default::default()
            };
            let _ = count;
            ir.skins[0].entries.push(SkinEntry {
                slot: SlotId(0),
                name: "route".into(),
                attachment: a2d_core::ir::ids::AttachmentId(0),
            });
            ir.path_constraints.push(PathConstraint {
                name: "pc".into(),
                order: 0,
                skin_required: false,
                bones: (0..self.bones).map(|i| BoneId(i as u16 + 1)).collect(),
                target_slot: SlotId(0),
                position_mode: self.position_mode,
                spacing_mode: self.spacing_mode,
                rotate_mode: self.rotate_mode,
                offset_rotation: self.offset_rotation,
                position: self.position,
                spacing: self.spacing,
                mix_rotate: self.mix,
                mix_x: self.mix,
                mix_y: self.mix,
            });
            ir.rebuild_derived();
            SkeletonPose::new(Arc::new(ir))
        }
    }

    #[test]
    fn a_chain_is_laid_out_along_a_straight_path() {
        // Fixed spacing with unit bone scale means the gaps are the spacing
        // outright: bones land at 0, 10, 20 along a 100-unit path.
        let mut pc = Pc::new(3);
        pc.spacing = 10.0;
        let pose = pc.pose();
        for (i, expected) in [0.0, 10.0, 20.0].into_iter().enumerate() {
            let p = pose.bones[i + 1].world.translation();
            assert_close(p.x, expected, &format!("bone {i} x"));
            assert_close(p.y, 0.0, &format!("bone {i} y"));
        }
    }

    #[test]
    fn the_position_offsets_the_whole_chain() {
        let mut pc = Pc::new(2);
        pc.position = 25.0;
        pc.spacing = 10.0;
        let pose = pc.pose();
        assert_close(pose.bones[1].world.translation().x, 25.0, "first");
        assert_close(pose.bones[2].world.translation().x, 35.0, "second");
    }

    #[test]
    fn percent_position_is_a_fraction_of_the_path() {
        let mut pc = Pc::new(1);
        pc.position_mode = PathPositionMode::Percent;
        pc.position = 0.25;
        let pose = pc.pose();
        assert_close(pose.bones[1].world.translation().x, 25.0, "quarter of the way");
    }

    #[test]
    fn percent_spacing_is_a_fraction_of_the_path() {
        let mut pc = Pc::new(2);
        pc.spacing_mode = PathSpacingMode::Percent;
        pc.spacing = 0.2;
        let pose = pc.pose();
        assert_close(pose.bones[1].world.translation().x, 0.0, "first");
        assert_close(pose.bones[2].world.translation().x, 20.0, "a fifth of 100");
    }

    #[test]
    fn length_spacing_adds_the_bone_length_to_the_gap() {
        // Length mode places bones a bone-length plus the spacing apart, which
        // is what keeps a chain of bones touching end to end.
        let mut pc = Pc::new(2);
        pc.spacing_mode = PathSpacingMode::Length;
        pc.bone_length = 10.0;
        pc.spacing = 5.0;
        let pose = pc.pose();
        assert_close(pose.bones[2].world.translation().x, 15.0, "length plus spacing");
    }

    #[test]
    fn tangent_mode_points_every_bone_along_the_path() {
        let mut pc = Pc::new(2);
        pc.rotate_mode = PathRotateMode::Tangent;
        pc.vertices = straight_path(100.0).into_iter().map(|v| Vec2::new(v.y, v.x)).collect();
        let pose = pc.pose();
        // The path now runs straight up, so the bones should too.
        for i in 1..=2 {
            assert_close(pose.bones[i].world.rotation_x_rad().to_degrees(), 90.0, "points up");
        }
    }

    #[test]
    fn a_zero_mix_leaves_the_chain_where_it_was() {
        let mut pc = Pc::new(2);
        pc.position = 50.0;
        pc.mix = 0.0;
        let pose = pc.pose();
        assert_close(pose.bones[1].world.translation().x, 0.0, "untouched");
        assert_close(pose.bones[2].world.translation().x, 0.0, "untouched");
    }

    #[test]
    fn half_a_translation_mix_lands_half_way_there() {
        let mut pc = Pc::new(1);
        pc.position = 40.0;
        pc.mix = 0.5;
        let pose = pc.pose();
        // From 0 towards 40, half way.
        assert_close(pose.bones[1].world.translation().x, 20.0, "half way");
    }

    #[test]
    fn running_off_the_end_continues_in_a_straight_line() {
        // An open path must extrapolate rather than pile bones up on the last
        // control point, or a chain longer than its path collapses.
        let mut pc = Pc::new(2);
        pc.position = 95.0;
        pc.spacing = 20.0;
        let pose = pc.pose();
        assert_close(pose.bones[1].world.translation().x, 95.0, "still on the path");
        assert_close(pose.bones[2].world.translation().x, 115.0, "past the end");
    }

    #[test]
    fn a_negative_position_extrapolates_backwards() {
        let mut pc = Pc::new(1);
        pc.position = -15.0;
        let pose = pc.pose();
        assert_close(pose.bones[1].world.translation().x, -15.0, "before the start");
    }

    #[test]
    fn a_closed_path_wraps_instead_of_extrapolating() {
        // Four cubics round a square, 100 a side: the corners are the anchors.
        let mut pc = Pc::new(1);
        pc.closed = true;
        pc.vertices = square_path(100.0);
        pc.position = 410.0; // once round (400) plus 10
        let pose = pc.pose();
        let p = pose.bones[1].world.translation();
        assert_close(p.x, 10.0, "wrapped x");
        assert_close(p.y, 0.0, "wrapped y");
    }

    /// A closed square path, side `side`, anticlockwise from the origin.
    ///
    /// Closed layout, for four segments: anchors at 1, 4, 7, 10, and the
    /// wrapping segment is `[10, 11, 0, 1]` -- so its two handles sit at
    /// opposite ends of the array. Handles lie on the edges, keeping each side
    /// straight and every side exactly `side` long.
    fn square_path(side: f32) -> Vec<Vec2> {
        let a = [Vec2::ZERO, Vec2::new(side, 0.0), Vec2::new(side, side), Vec2::new(0.0, side)];
        vec![
            lerp_vec(a[3], a[0], 2.0 / 3.0), // second handle of the wrapping segment
            a[0],
            lerp_vec(a[0], a[1], 1.0 / 3.0),
            lerp_vec(a[0], a[1], 2.0 / 3.0),
            a[1],
            lerp_vec(a[1], a[2], 1.0 / 3.0),
            lerp_vec(a[1], a[2], 2.0 / 3.0),
            a[2],
            lerp_vec(a[2], a[3], 1.0 / 3.0),
            lerp_vec(a[2], a[3], 2.0 / 3.0),
            a[3],
            lerp_vec(a[3], a[0], 1.0 / 3.0), // first handle of the wrapping segment
        ]
    }

    fn lerp_vec(a: Vec2, b: Vec2, t: f32) -> Vec2 {
        a + (b - a) * t
    }

    #[test]
    fn a_path_with_too_few_points_is_reported_rather_than_guessed() {
        let mut pc = Pc::new(1);
        pc.vertices = vec![Vec2::ZERO, Vec2::new(1.0, 0.0)];
        let pose = pc.pose();
        assert!(!pose.degradations().is_empty(), "a degenerate path should be reported");
    }

    #[test]
    fn a_slot_with_no_path_attached_is_not_an_error() {
        // A skin may leave the path off; the constraint then has nothing to do
        // and must not report a problem.
        let mut ir = SpineIr {
            bones: vec![bone("root", None, BoneLocal::default(), 0.0), bone("b", Some(0), at(5.0, 0.0), 10.0)],
            slots: vec![Slot::new("route", BoneId(0))],
            ..Default::default()
        };
        ir.path_constraints.push(PathConstraint {
            name: "pc".into(),
            order: 0,
            skin_required: false,
            bones: vec![BoneId(1)],
            target_slot: SlotId(0),
            position_mode: PathPositionMode::default(),
            spacing_mode: PathSpacingMode::default(),
            rotate_mode: PathRotateMode::default(),
            offset_rotation: 0.0,
            position: 0.0,
            spacing: 0.0,
            mix_rotate: 1.0,
            mix_x: 1.0,
            mix_y: 1.0,
        });
        ir.rebuild_derived();
        let pose = SkeletonPose::new(Arc::new(ir));
        assert!(pose.degradations().is_empty(), "{:?}", pose.degradations());
        // The bone keeps its own placement.
        assert_close(pose.bones[1].world.translation().x, 5.0, "left alone");
    }

    #[test]
    fn an_authored_length_is_used_when_the_path_is_not_constant_speed() {
        // The authored cumulative length stands in for measuring, so a path
        // declaring itself 200 long puts the half-way bone at parameter 0.5.
        let mut pc = Pc::new(1);
        pc.constant_speed = false;
        pc.lengths = vec![200.0];
        pc.position = 100.0;
        let pose = pc.pose();
        assert_close(pose.bones[1].world.translation().x, 50.0, "half the curve, not half the geometry");
    }
}
