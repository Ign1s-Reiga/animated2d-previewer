//! Colour types. Components are non-premultiplied, linear-in-asset (i.e. exactly
//! the values the source format stored) and clamped only at use sites.

/// Opaque RGB triple, used for Spine "dark colour" (two-colour tint).
#[derive(Debug, Clone, Copy, PartialEq, Default)]
pub struct Rgb {
    pub r: f32,
    pub g: f32,
    pub b: f32,
}

impl Rgb {
    pub const BLACK: Rgb = Rgb { r: 0.0, g: 0.0, b: 0.0 };

    #[inline]
    pub const fn new(r: f32, g: f32, b: f32) -> Self {
        Rgb { r, g, b }
    }
}

/// Non-premultiplied RGBA.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Rgba {
    pub r: f32,
    pub g: f32,
    pub b: f32,
    pub a: f32,
}

impl Default for Rgba {
    fn default() -> Self {
        Rgba::WHITE
    }
}

impl Rgba {
    pub const WHITE: Rgba = Rgba { r: 1.0, g: 1.0, b: 1.0, a: 1.0 };
    pub const TRANSPARENT: Rgba = Rgba { r: 0.0, g: 0.0, b: 0.0, a: 0.0 };

    #[inline]
    pub const fn new(r: f32, g: f32, b: f32, a: f32) -> Self {
        Rgba { r, g, b, a }
    }

    #[inline]
    pub const fn rgb(self) -> Rgb {
        Rgb { r: self.r, g: self.g, b: self.b }
    }

    /// Parses the `RRGGBBAA` / `RRGGBB` hex strings used by Spine JSON.
    ///
    /// Returns `None` for any other length or a non-hex digit; callers report
    /// that as a corrupt asset rather than substituting white.
    pub fn from_hex(s: &str) -> Option<Rgba> {
        let s = s.strip_prefix('#').unwrap_or(s);
        if s.len() != 6 && s.len() != 8 {
            return None;
        }
        let byte =
            |i: usize| -> Option<f32> { u8::from_str_radix(s.get(i..i + 2)?, 16).ok().map(|v| v as f32 / 255.0) };
        Some(Rgba { r: byte(0)?, g: byte(2)?, b: byte(4)?, a: if s.len() == 8 { byte(6)? } else { 1.0 } })
    }

    /// Unpacks a Spine binary `RGBA8888` word.
    #[inline]
    pub fn from_rgba8888(v: u32) -> Rgba {
        Rgba {
            r: ((v >> 24) & 0xff) as f32 / 255.0,
            g: ((v >> 16) & 0xff) as f32 / 255.0,
            b: ((v >> 8) & 0xff) as f32 / 255.0,
            a: (v & 0xff) as f32 / 255.0,
        }
    }

    /// Unpacks a Spine binary `RGB888` word (dark colour).
    #[inline]
    pub fn rgb_from_rgb888(v: u32) -> Rgb {
        Rgb { r: ((v >> 16) & 0xff) as f32 / 255.0, g: ((v >> 8) & 0xff) as f32 / 255.0, b: (v & 0xff) as f32 / 255.0 }
    }

    #[inline]
    pub fn modulate(self, rhs: Rgba) -> Rgba {
        Rgba::new(self.r * rhs.r, self.g * rhs.g, self.b * rhs.b, self.a * rhs.a)
    }

    #[inline]
    pub fn lerp(self, rhs: Rgba, t: f32) -> Rgba {
        Rgba::new(
            self.r + (rhs.r - self.r) * t,
            self.g + (rhs.g - self.g) * t,
            self.b + (rhs.b - self.b) * t,
            self.a + (rhs.a - self.a) * t,
        )
    }

    #[inline]
    pub fn to_array(self) -> [f32; 4] {
        [self.r, self.g, self.b, self.a]
    }
}

impl Rgb {
    #[inline]
    pub fn lerp(self, rhs: Rgb, t: f32) -> Rgb {
        Rgb::new(self.r + (rhs.r - self.r) * t, self.g + (rhs.g - self.g) * t, self.b + (rhs.b - self.b) * t)
    }

    #[inline]
    pub fn to_array(self) -> [f32; 3] {
        [self.r, self.g, self.b]
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn hex_with_alpha_parses() {
        let c = Rgba::from_hex("ff8000cc").unwrap();
        assert!((c.r - 1.0).abs() < 1e-6);
        assert!((c.g - 128.0 / 255.0).abs() < 1e-6);
        assert_eq!(c.b, 0.0);
        assert!((c.a - 204.0 / 255.0).abs() < 1e-6);
    }

    #[test]
    fn hex_without_alpha_is_opaque() {
        assert_eq!(Rgba::from_hex("000000").unwrap(), Rgba::new(0.0, 0.0, 0.0, 1.0));
    }

    #[test]
    fn leading_hash_is_accepted() {
        assert_eq!(Rgba::from_hex("#ffffff"), Rgba::from_hex("ffffff"));
    }

    #[test]
    fn malformed_hex_is_rejected_rather_than_defaulted() {
        assert!(Rgba::from_hex("fff").is_none());
        assert!(Rgba::from_hex("gggggg").is_none());
        assert!(Rgba::from_hex("").is_none());
        assert!(Rgba::from_hex("ff8000ccff").is_none());
    }

    #[test]
    fn binary_rgba8888_matches_hex_parse() {
        assert_eq!(Rgba::from_rgba8888(0xff8000cc), Rgba::from_hex("ff8000cc").unwrap());
    }

    #[test]
    fn binary_rgb888_drops_alpha() {
        assert_eq!(
            Rgba::rgb_from_rgb888(0x336699),
            Rgb::new(0x33 as f32 / 255.0, 0x66 as f32 / 255.0, 0x99 as f32 / 255.0)
        );
    }
}
