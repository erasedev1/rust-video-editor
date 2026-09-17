use serde::{Deserialize, Serialize};

/// A 2D vector, used for position, scale and anchor points.
#[derive(Debug, Clone, Copy, PartialEq, Default, Serialize, Deserialize)]
pub struct Vec2 {
    pub x: f64,
    pub y: f64,
}

impl Vec2 {
    pub const ZERO: Vec2 = Vec2 { x: 0.0, y: 0.0 };
    pub const ONE: Vec2 = Vec2 { x: 1.0, y: 1.0 };

    #[inline]
    pub const fn new(x: f64, y: f64) -> Self {
        Vec2 { x, y }
    }

    #[inline]
    pub const fn splat(v: f64) -> Self {
        Vec2 { x: v, y: v }
    }
}

/// Straight (non-premultiplied) RGBA, each channel in `0.0..=1.0`.
#[derive(Debug, Clone, Copy, PartialEq, Serialize, Deserialize)]
pub struct Rgba {
    pub r: f64,
    pub g: f64,
    pub b: f64,
    pub a: f64,
}

impl Rgba {
    pub const TRANSPARENT: Rgba = Rgba { r: 0.0, g: 0.0, b: 0.0, a: 0.0 };
    pub const BLACK: Rgba = Rgba { r: 0.0, g: 0.0, b: 0.0, a: 1.0 };
    pub const WHITE: Rgba = Rgba { r: 1.0, g: 1.0, b: 1.0, a: 1.0 };

    #[inline]
    pub const fn new(r: f64, g: f64, b: f64, a: f64) -> Self {
        Rgba { r, g, b, a }
    }

    pub fn to_f32_array(self) -> [f32; 4] {
        [self.r as f32, self.g as f32, self.b as f32, self.a as f32]
    }
}

impl Default for Rgba {
    fn default() -> Self {
        Rgba::TRANSPARENT
    }
}

/// Pixel dimensions of a frame or render target.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub struct Size {
    pub width: u32,
    pub height: u32,
}

impl Size {
    pub const fn new(width: u32, height: u32) -> Self {
        Size { width, height }
    }

    pub fn aspect_ratio(self) -> f64 {
        if self.height == 0 {
            1.0
        } else {
            self.width as f64 / self.height as f64
        }
    }

    pub fn pixel_count(self) -> u64 {
        self.width as u64 * self.height as u64
    }
}

impl Default for Size {
    fn default() -> Self {
        Size::new(1920, 1080)
    }
}

/// How colour is treated when layers are combined.
///
/// Video arrives gamma-encoded: an 8-bit `128` is roughly half of *perceived*
/// brightness, not half the light. Which of those two a compositor blends is a
/// real choice, and both answers are defensible, so it is a project setting
/// rather than a constant.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Default, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ColorSpace {
    /// Blend the encoded values directly, as the established editors do by
    /// default. A 50% dissolve lands halfway between the two pictures *as they
    /// look*, which is what an editor coming from those tools expects, and what
    /// makes a fade feel even from end to end.
    #[default]
    Perceptual,
    /// Convert to linear light before blending and back on store, which is how
    /// light actually adds. Physically correct, and visibly different: a 50%
    /// dissolve sits brighter than the perceptual midpoint, and additive
    /// compositing of highlights stops clipping early.
    Linear,
}

impl ColorSpace {
    pub const ALL: [ColorSpace; 2] = [ColorSpace::Perceptual, ColorSpace::Linear];

    pub fn label(self) -> &'static str {
        match self {
            ColorSpace::Perceptual => "Perceptual",
            ColorSpace::Linear => "Linear light",
        }
    }

    /// What the control's tooltip says, so the difference is discoverable
    /// rather than folklore.
    pub fn description(self) -> &'static str {
        match self {
            ColorSpace::Perceptual => {
                "Blend encoded values. Matches the established editors; fades \
                 feel even."
            }
            ColorSpace::Linear => {
                "Blend in linear light. Physically correct; dissolves and \
                 additive highlights come out brighter."
            }
        }
    }
}
