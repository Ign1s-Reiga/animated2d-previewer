//! Axis-aligned bounding box.

use super::Vec2;

/// Axis-aligned bounding box in model space.
///
/// An empty box is represented by inverted bounds so that [`Aabb::extend`] on a
/// fresh [`Aabb::EMPTY`] yields exactly the first point.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Aabb {
    pub min: Vec2,
    pub max: Vec2,
}

impl Default for Aabb {
    fn default() -> Self {
        Aabb::EMPTY
    }
}

impl Aabb {
    pub const EMPTY: Aabb = Aabb {
        min: Vec2 { x: f32::INFINITY, y: f32::INFINITY },
        max: Vec2 { x: f32::NEG_INFINITY, y: f32::NEG_INFINITY },
    };

    #[inline]
    pub const fn new(min: Vec2, max: Vec2) -> Self {
        Aabb { min, max }
    }

    #[inline]
    pub fn is_empty(&self) -> bool {
        self.min.x > self.max.x || self.min.y > self.max.y
    }

    #[inline]
    pub fn extend(&mut self, p: Vec2) {
        if !p.is_finite() {
            return;
        }
        self.min = self.min.min(p);
        self.max = self.max.max(p);
    }

    #[inline]
    pub fn union(&mut self, other: &Aabb) {
        if other.is_empty() {
            return;
        }
        self.extend(other.min);
        self.extend(other.max);
    }

    #[inline]
    pub fn size(&self) -> Vec2 {
        if self.is_empty() {
            Vec2::ZERO
        } else {
            self.max - self.min
        }
    }

    #[inline]
    pub fn center(&self) -> Vec2 {
        if self.is_empty() {
            Vec2::ZERO
        } else {
            (self.min + self.max) * 0.5
        }
    }

    #[inline]
    pub fn contains(&self, p: Vec2) -> bool {
        !self.is_empty() && p.x >= self.min.x && p.x <= self.max.x && p.y >= self.min.y && p.y <= self.max.y
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn empty_box_reports_empty_and_zero_size() {
        let b = Aabb::EMPTY;
        assert!(b.is_empty());
        assert_eq!(b.size(), Vec2::ZERO);
        assert!(!b.contains(Vec2::ZERO));
    }

    #[test]
    fn extending_empty_box_yields_a_degenerate_box_at_the_point() {
        let mut b = Aabb::EMPTY;
        b.extend(Vec2::new(3.0, 4.0));
        assert!(!b.is_empty());
        assert_eq!(b.min, Vec2::new(3.0, 4.0));
        assert_eq!(b.max, Vec2::new(3.0, 4.0));
    }

    #[test]
    fn non_finite_points_are_ignored() {
        let mut b = Aabb::EMPTY;
        b.extend(Vec2::new(1.0, 1.0));
        b.extend(Vec2::new(f32::NAN, 0.0));
        assert_eq!(b.max, Vec2::new(1.0, 1.0));
    }

    #[test]
    fn union_with_empty_is_a_no_op() {
        let mut b = Aabb::new(Vec2::ZERO, Vec2::ONE);
        b.union(&Aabb::EMPTY);
        assert_eq!(b, Aabb::new(Vec2::ZERO, Vec2::ONE));
    }

    #[test]
    fn center_and_size_of_a_real_box() {
        let b = Aabb::new(Vec2::new(-2.0, -4.0), Vec2::new(2.0, 4.0));
        assert_eq!(b.center(), Vec2::ZERO);
        assert_eq!(b.size(), Vec2::new(4.0, 8.0));
    }
}
