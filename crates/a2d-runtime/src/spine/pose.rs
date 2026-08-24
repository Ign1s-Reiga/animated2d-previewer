//! Skeleton pose state and world transform evaluation.
//!
//! The transform maths follows the reference Spine runtime closely and
//! deliberately: matching it term-for-term is the only practical way to hit the
//! "nearly identical geometry at the same timestamp" bar in spec §17.4. Where
//! this file departs from the reference, the comment says why.

use std::sync::Arc;

use a2d_core::ir::ids::{AttachmentId, BoneId, SkinId, SlotId};
use a2d_core::ir::spine::{BoneLocal, ConstraintKind, SpineIr, TransformInherit, DEFAULT_SKIN};
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
        self.reset_draw_order();
        self.reset_attachments_to_setup();
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
                // Path constraints are spec §6.5 priority 3 and are not
                // evaluated yet. `degradations` reports it exactly once.
                ConstraintKind::Path => self.note_unsupported("path constraint"),
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

        if constraint.local || constraint.relative {
            // Absolute-world is the mode target assets use. The other three are
            // parsed and reported rather than silently mis-evaluated.
            self.note_unsupported(if constraint.local {
                "transform constraint in local mode"
            } else {
                "transform constraint in relative mode"
            });
            return;
        }

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

        if lowest != usize::MAX {
            // The constraint wrote world transforms directly, so descendants
            // must be recomputed — but not the constrained bones themselves.
            let highest = constraint.bones.iter().map(|b| b.index()).max().unwrap_or(lowest);
            self.update_world_from(highest + 1);
        }
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
    use a2d_core::ir::spine::{Bone, IkConstraint, Slot};

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

    #[test]
    fn path_constraints_are_reported_as_unsupported_exactly_once() {
        use a2d_core::ir::spine::{PathConstraint, PathPositionMode, PathRotateMode, PathSpacingMode};
        let mut ir = SpineIr {
            bones: vec![bone("root", None, BoneLocal::default(), 0.0)],
            slots: vec![Slot::new("route", BoneId(0))],
            ..Default::default()
        };
        ir.path_constraints.push(PathConstraint {
            name: "pc".into(),
            order: 0,
            skin_required: false,
            bones: vec![BoneId(0)],
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
        let mut pose = SkeletonPose::new(Arc::new(ir));
        for _ in 0..10 {
            pose.update_world_transforms();
        }
        assert_eq!(pose.degradations().len(), 1);
        let mut report = LoadReport::new();
        pose.absorb_degradations(&mut report);
        assert!(report.to_string().contains("path constraint"), "{report}");
    }
}
