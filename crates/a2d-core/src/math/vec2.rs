//! Two-component vector.

use std::ops::{Add, AddAssign, Div, Mul, MulAssign, Neg, Sub, SubAssign};

/// A 2D vector / point in model space.
#[derive(Debug, Clone, Copy, PartialEq, Default)]
pub struct Vec2 {
    pub x: f32,
    pub y: f32,
}

impl Vec2 {
    pub const ZERO: Vec2 = Vec2 { x: 0.0, y: 0.0 };
    pub const ONE: Vec2 = Vec2 { x: 1.0, y: 1.0 };

    #[inline]
    pub const fn new(x: f32, y: f32) -> Self {
        Vec2 { x, y }
    }

    #[inline]
    pub const fn splat(v: f32) -> Self {
        Vec2 { x: v, y: v }
    }

    #[inline]
    pub fn length(self) -> f32 {
        self.length_squared().sqrt()
    }

    #[inline]
    pub fn length_squared(self) -> f32 {
        self.x * self.x + self.y * self.y
    }

    #[inline]
    pub fn dot(self, rhs: Vec2) -> f32 {
        self.x * rhs.x + self.y * rhs.y
    }

    /// 2D cross product magnitude (`z` of the 3D cross product).
    #[inline]
    pub fn cross(self, rhs: Vec2) -> f32 {
        self.x * rhs.y - self.y * rhs.x
    }

    /// Returns the unit vector, or [`Vec2::ZERO`] when the length is degenerate.
    #[inline]
    pub fn normalize_or_zero(self) -> Vec2 {
        let len = self.length();
        if len <= f32::EPSILON {
            Vec2::ZERO
        } else {
            self / len
        }
    }

    #[inline]
    pub fn lerp(self, rhs: Vec2, t: f32) -> Vec2 {
        Vec2::new(self.x + (rhs.x - self.x) * t, self.y + (rhs.y - self.y) * t)
    }

    #[inline]
    pub fn min(self, rhs: Vec2) -> Vec2 {
        Vec2::new(self.x.min(rhs.x), self.y.min(rhs.y))
    }

    #[inline]
    pub fn max(self, rhs: Vec2) -> Vec2 {
        Vec2::new(self.x.max(rhs.x), self.y.max(rhs.y))
    }

    /// Angle in radians, measured counter-clockwise from `+X`.
    #[inline]
    pub fn angle_rad(self) -> f32 {
        self.y.atan2(self.x)
    }

    #[inline]
    pub fn from_angle_rad(rad: f32) -> Vec2 {
        Vec2::new(rad.cos(), rad.sin())
    }

    #[inline]
    pub fn is_finite(self) -> bool {
        self.x.is_finite() && self.y.is_finite()
    }
}

impl Add for Vec2 {
    type Output = Vec2;
    #[inline]
    fn add(self, rhs: Vec2) -> Vec2 {
        Vec2::new(self.x + rhs.x, self.y + rhs.y)
    }
}

impl AddAssign for Vec2 {
    #[inline]
    fn add_assign(&mut self, rhs: Vec2) {
        self.x += rhs.x;
        self.y += rhs.y;
    }
}

impl Sub for Vec2 {
    type Output = Vec2;
    #[inline]
    fn sub(self, rhs: Vec2) -> Vec2 {
        Vec2::new(self.x - rhs.x, self.y - rhs.y)
    }
}

impl SubAssign for Vec2 {
    #[inline]
    fn sub_assign(&mut self, rhs: Vec2) {
        self.x -= rhs.x;
        self.y -= rhs.y;
    }
}

impl Mul<f32> for Vec2 {
    type Output = Vec2;
    #[inline]
    fn mul(self, rhs: f32) -> Vec2 {
        Vec2::new(self.x * rhs, self.y * rhs)
    }
}

impl MulAssign<f32> for Vec2 {
    #[inline]
    fn mul_assign(&mut self, rhs: f32) {
        self.x *= rhs;
        self.y *= rhs;
    }
}

/// Component-wise multiply. Used for non-uniform scale.
impl Mul<Vec2> for Vec2 {
    type Output = Vec2;
    #[inline]
    fn mul(self, rhs: Vec2) -> Vec2 {
        Vec2::new(self.x * rhs.x, self.y * rhs.y)
    }
}

impl Div<f32> for Vec2 {
    type Output = Vec2;
    #[inline]
    fn div(self, rhs: f32) -> Vec2 {
        Vec2::new(self.x / rhs, self.y / rhs)
    }
}

impl Neg for Vec2 {
    type Output = Vec2;
    #[inline]
    fn neg(self) -> Vec2 {
        Vec2::new(-self.x, -self.y)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn dot_and_cross_are_orthogonal_measures() {
        let a = Vec2::new(1.0, 0.0);
        let b = Vec2::new(0.0, 1.0);
        assert_eq!(a.dot(b), 0.0);
        assert_eq!(a.cross(b), 1.0);
        assert_eq!(b.cross(a), -1.0);
    }

    #[test]
    fn normalize_of_zero_vector_does_not_produce_nan() {
        assert_eq!(Vec2::ZERO.normalize_or_zero(), Vec2::ZERO);
    }

    #[test]
    fn angle_round_trips_through_from_angle() {
        for deg in [0.0f32, 30.0, 90.0, 179.0, -45.0] {
            let rad = deg.to_radians();
            let v = Vec2::from_angle_rad(rad);
            assert!((v.angle_rad() - rad).abs() < 1e-5, "deg={deg}");
        }
    }

    #[test]
    fn lerp_hits_both_endpoints() {
        let a = Vec2::new(-2.0, 4.0);
        let b = Vec2::new(6.0, -8.0);
        assert_eq!(a.lerp(b, 0.0), a);
        assert_eq!(a.lerp(b, 1.0), b);
        assert_eq!(a.lerp(b, 0.5), Vec2::new(2.0, -2.0));
    }
}
