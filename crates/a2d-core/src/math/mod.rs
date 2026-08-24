//! Math primitives shared by every layer.

mod aabb;
mod affine;
mod color;
mod curve;
mod vec2;

pub use aabb::Aabb;
pub use affine::Affine2;
pub use color::{Rgb, Rgba};
pub use curve::{lerp, Bezier, Interpolation};
pub use vec2::Vec2;

/// Wraps an angle in degrees into `-180..=180`.
///
/// Timeline rotation values accumulate without bound in authored data; every
/// consumer needs the shortest-arc form.
#[inline]
pub fn wrap_degrees(mut deg: f32) -> f32 {
    deg %= 360.0;
    if deg > 180.0 {
        deg -= 360.0;
    } else if deg < -180.0 {
        deg += 360.0;
    }
    deg
}

#[cfg(test)]
mod tests {
    use super::wrap_degrees;

    #[test]
    fn already_wrapped_angles_are_unchanged() {
        assert_eq!(wrap_degrees(0.0), 0.0);
        assert_eq!(wrap_degrees(90.0), 90.0);
        assert_eq!(wrap_degrees(-90.0), -90.0);
        assert_eq!(wrap_degrees(180.0), 180.0);
    }

    #[test]
    fn angles_past_half_turn_take_the_short_arc() {
        assert_eq!(wrap_degrees(270.0), -90.0);
        assert_eq!(wrap_degrees(-270.0), 90.0);
    }

    #[test]
    fn multiple_turns_collapse() {
        assert!((wrap_degrees(720.0 + 45.0) - 45.0).abs() < 1e-4);
        assert!((wrap_degrees(-720.0 - 45.0) + 45.0).abs() < 1e-4);
    }
}
