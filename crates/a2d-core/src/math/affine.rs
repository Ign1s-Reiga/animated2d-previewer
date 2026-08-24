//! 2D affine transform.
//!
//! The component layout deliberately matches the convention used by Spine's own
//! bone math (`a b` on the first row, `c d` on the second) so that decoder and
//! runtime code can be audited line-by-line against the reference runtimes:
//!
//! ```text
//! x' = a*x + b*y + tx
//! y' = c*x + d*y + ty
//! ```

use super::Vec2;

/// A 2D affine transform stored as a 2x3 matrix.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Affine2 {
    pub a: f32,
    pub b: f32,
    pub c: f32,
    pub d: f32,
    pub tx: f32,
    pub ty: f32,
}

impl Default for Affine2 {
    fn default() -> Self {
        Affine2::IDENTITY
    }
}

impl Affine2 {
    pub const IDENTITY: Affine2 = Affine2 { a: 1.0, b: 0.0, c: 0.0, d: 1.0, tx: 0.0, ty: 0.0 };

    #[inline]
    pub const fn new(a: f32, b: f32, c: f32, d: f32, tx: f32, ty: f32) -> Self {
        Affine2 { a, b, c, d, tx, ty }
    }

    #[inline]
    pub const fn from_translation(t: Vec2) -> Self {
        Affine2 { a: 1.0, b: 0.0, c: 0.0, d: 1.0, tx: t.x, ty: t.y }
    }

    #[inline]
    pub const fn from_scale(s: Vec2) -> Self {
        Affine2 { a: s.x, b: 0.0, c: 0.0, d: s.y, tx: 0.0, ty: 0.0 }
    }

    #[inline]
    pub fn from_rotation_rad(rad: f32) -> Self {
        let (sin, cos) = rad.sin_cos();
        Affine2 { a: cos, b: -sin, c: sin, d: cos, tx: 0.0, ty: 0.0 }
    }

    /// Composes two transforms: apply `rhs` first, then `self`.
    ///
    /// Deliberately not `std::ops::Mul`, so that the argument order is spelled
    /// out at every call site — getting it backwards is the classic transform
    /// bug and an operator hides it.
    #[inline]
    pub fn then(self, rhs: Affine2) -> Affine2 {
        Affine2 {
            a: self.a * rhs.a + self.b * rhs.c,
            b: self.a * rhs.b + self.b * rhs.d,
            c: self.c * rhs.a + self.d * rhs.c,
            d: self.c * rhs.b + self.d * rhs.d,
            tx: self.a * rhs.tx + self.b * rhs.ty + self.tx,
            ty: self.c * rhs.tx + self.d * rhs.ty + self.ty,
        }
    }

    /// Transforms a point (translation applies).
    #[inline]
    pub fn transform_point(self, p: Vec2) -> Vec2 {
        Vec2::new(self.a * p.x + self.b * p.y + self.tx, self.c * p.x + self.d * p.y + self.ty)
    }

    /// Transforms a direction (translation is ignored).
    #[inline]
    pub fn transform_vector(self, v: Vec2) -> Vec2 {
        Vec2::new(self.a * v.x + self.b * v.y, self.c * v.x + self.d * v.y)
    }

    #[inline]
    pub fn determinant(self) -> f32 {
        self.a * self.d - self.b * self.c
    }

    /// Inverse transform, or `None` when the matrix is singular.
    pub fn inverse(self) -> Option<Affine2> {
        let det = self.determinant();
        if det.abs() <= 1e-12 {
            return None;
        }
        let inv_det = 1.0 / det;
        let (a, b, c, d) = (self.d * inv_det, -self.b * inv_det, -self.c * inv_det, self.a * inv_det);
        Some(Affine2 { a, b, c, d, tx: -(a * self.tx + b * self.ty), ty: -(c * self.tx + d * self.ty) })
    }

    /// Transforms a point by the inverse of `self`, without materialising the inverse.
    ///
    /// Returns `None` for a singular matrix, which is what a zero-scaled bone produces.
    pub fn world_to_local(self, p: Vec2) -> Option<Vec2> {
        let det = self.determinant();
        if det.abs() <= 1e-12 {
            return None;
        }
        let inv_det = 1.0 / det;
        let x = p.x - self.tx;
        let y = p.y - self.ty;
        Some(Vec2::new((x * self.d - y * self.b) * inv_det, (y * self.a - x * self.c) * inv_det))
    }

    #[inline]
    pub fn translation(self) -> Vec2 {
        Vec2::new(self.tx, self.ty)
    }

    /// Rotation of the local `+X` axis, in radians.
    #[inline]
    pub fn rotation_x_rad(self) -> f32 {
        self.c.atan2(self.a)
    }

    /// Rotation of the local `+Y` axis, in radians.
    #[inline]
    pub fn rotation_y_rad(self) -> f32 {
        self.d.atan2(self.b)
    }

    /// Lengths of the local `+X` and `+Y` axes.
    #[inline]
    pub fn scale(self) -> Vec2 {
        Vec2::new((self.a * self.a + self.c * self.c).sqrt(), (self.b * self.b + self.d * self.d).sqrt())
    }

    #[inline]
    pub fn is_finite(self) -> bool {
        self.a.is_finite()
            && self.b.is_finite()
            && self.c.is_finite()
            && self.d.is_finite()
            && self.tx.is_finite()
            && self.ty.is_finite()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn assert_close(l: Vec2, r: Vec2) {
        assert!((l.x - r.x).abs() < 1e-4 && (l.y - r.y).abs() < 1e-4, "{l:?} != {r:?}");
    }

    #[test]
    fn identity_is_a_no_op() {
        let p = Vec2::new(3.0, -7.0);
        assert_eq!(Affine2::IDENTITY.transform_point(p), p);
    }

    #[test]
    fn rotation_is_counter_clockwise() {
        let m = Affine2::from_rotation_rad(std::f32::consts::FRAC_PI_2);
        assert_close(m.transform_point(Vec2::new(1.0, 0.0)), Vec2::new(0.0, 1.0));
    }

    #[test]
    fn mul_applies_right_hand_side_first() {
        // Scale by 2, then translate by (10, 0).
        let m = Affine2::from_translation(Vec2::new(10.0, 0.0)).then(Affine2::from_scale(Vec2::splat(2.0)));
        assert_close(m.transform_point(Vec2::new(1.0, 1.0)), Vec2::new(12.0, 2.0));
    }

    #[test]
    fn transform_vector_ignores_translation() {
        let m = Affine2::from_translation(Vec2::new(100.0, 100.0));
        assert_eq!(m.transform_vector(Vec2::new(1.0, 2.0)), Vec2::new(1.0, 2.0));
    }

    #[test]
    fn world_to_local_inverts_transform_point() {
        let m = Affine2::from_translation(Vec2::new(5.0, -2.0))
            .then(Affine2::from_rotation_rad(0.7))
            .then(Affine2::from_scale(Vec2::new(2.0, 3.0)));
        let p = Vec2::new(1.5, -4.25);
        assert_close(m.world_to_local(m.transform_point(p)).unwrap(), p);
    }

    #[test]
    fn inverse_matches_world_to_local() {
        let m = Affine2::new(0.6, -0.8, 0.8, 0.6, 12.0, -3.0);
        let p = Vec2::new(2.0, 9.0);
        assert_close(m.inverse().unwrap().transform_point(p), m.world_to_local(p).unwrap());
    }

    #[test]
    fn singular_matrix_has_no_inverse() {
        let m = Affine2::from_scale(Vec2::ZERO);
        assert!(m.inverse().is_none());
        assert!(m.world_to_local(Vec2::ONE).is_none());
    }

    #[test]
    fn decomposition_recovers_rotation_and_scale() {
        let rot = 0.9f32;
        let m = Affine2::from_rotation_rad(rot).then(Affine2::from_scale(Vec2::new(2.0, 5.0)));
        assert!((m.rotation_x_rad() - rot).abs() < 1e-5);
        let s = m.scale();
        assert!((s.x - 2.0).abs() < 1e-5 && (s.y - 5.0).abs() < 1e-5, "{s:?}");
    }
}
