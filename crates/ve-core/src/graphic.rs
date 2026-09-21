//! Pictures the editor draws rather than decodes: shapes and text.
//!
//! # A graphic is a source, not a clip
//!
//! A title or a shape is *placed in time, trimmed, transformed, blended,
//! keyframed and given effects* — every one of which a [`Clip`] already does.
//! So a graphic is not a new kind of timeline object; it is a new kind of
//! [`Source`], and a clip drawing one is an ordinary clip. Nothing that walks
//! clips has to learn about graphics, and every feature a clip already has
//! arrives for free.
//!
//! That is the opposite of the decision captions took — see [`crate::caption`]
//! — and for the opposite reason. A cue carries no window into a source, no
//! speed, no blend mode and no effect chain, so writing one as a clip would
//! mean six fields that mean nothing. A shape has all six.
//!
//! [`Clip`]: crate::Clip
//! [`Source`]: crate::Source
//!
//! # Where a graphic lives
//!
//! In the project, beside assets and compositions, rather than inline on the
//! clip. Two things force it: a [`CompositionLayer`] draws from a `Source` too,
//! so inline data would have to be stored in two places, and `Source` is a
//! `Copy` handle that every consumer passes around by value. A project-level
//! table also gives a graphic the same shape as everything else referenced by a
//! source — a name, an ID, a refusal to be deleted while something still draws
//! it, and a duplicate that makes an independent copy.
//!
//! The consequence is worth saying plainly: **two clips drawing the same
//! graphic show the same graphic**, exactly as two clips drawing the same
//! composition do. Editing the text changes both. `Duplicate` is how you get a
//! second one that goes its own way.
//!
//! [`CompositionLayer`]: crate::CompositionLayer
//!
//! # Everything that can be a number is a `Property`
//!
//! Sizes, colours, radii, stroke widths, tracking and line height are all
//! [`Property`], the same animated value the transform and every effect
//! parameter use. So keyframing a rectangle's corner radius needs no
//! graphic-specific machinery: it is the same command, the same graph editor
//! and the same evaluation as opacity.
//!
//! There is deliberately **no switch for "has a fill"** and none for "has a
//! stroke". A fill is a colour, and a transparent colour is no fill; a stroke
//! is a width, and a zero width is no stroke. A switch cannot be keyframed, so
//! adding one would mean a shape could not fade its outline in — which is
//! precisely the kind of thing motion graphics is for.

use serde::{Deserialize, Serialize};
use ve_time::Ticks;

use crate::animation::Property;
use crate::geometry::{Rgba, Vec2};
use crate::id::GraphicId;

/// A picture the editor synthesises: a shape or a run of text.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Graphic {
    pub id: GraphicId,
    pub name: String,
    pub content: GraphicContent,
}

impl Graphic {
    /// How long a clip drawing a graphic is when it is first placed.
    ///
    /// Five seconds, which is what the established editors use for a still and
    /// for a title, and long enough that the first thing a user does is trim it
    /// rather than discover it was a single frame.
    pub const DEFAULT_DURATION_SECONDS: i64 = 5;

    /// The last instant a graphic can be trimmed to.
    ///
    /// A synthesised picture has no last frame — a rectangle is as long as you
    /// want it to be — but trim arithmetic needs *a* number to bound against,
    /// and an unbounded one would have to be special-cased at every call site
    /// that compares against a source's length. A day is past the length of any
    /// edit, and far short of what a `Ticks` can hold.
    pub const MAX_DURATION_HOURS: i64 = 24;

    pub fn shape(id: GraphicId, name: impl Into<String>, shape: Shape) -> Self {
        Graphic { id, name: name.into(), content: GraphicContent::Shape(shape) }
    }

    pub fn text(id: GraphicId, name: impl Into<String>, text: Text) -> Self {
        Graphic { id, name: name.into(), content: GraphicContent::Text(text) }
    }

    /// Resolves every animated parameter at clip-relative time `t`.
    ///
    /// The same rule the transform and effect parameters follow: `t` is
    /// measured from the start of the clip drawing this graphic, not from the
    /// start of the sequence. So one graphic used twice animates from each
    /// clip's own beginning, and moving a clip along the timeline does not
    /// touch its animation.
    pub fn evaluate(&self, t: Ticks) -> GraphicState {
        match &self.content {
            GraphicContent::Shape(shape) => GraphicState::Shape(shape.evaluate(t)),
            GraphicContent::Text(text) => GraphicState::Text(text.evaluate(t)),
        }
    }

    /// Whether anything about this graphic changes over time.
    ///
    /// What it buys is a rasterisation held across a whole clip rather than one
    /// per frame: a static title is drawn once however long it is on screen.
    pub fn is_animated(&self) -> bool {
        match &self.content {
            GraphicContent::Shape(shape) => shape.is_animated(),
            GraphicContent::Text(text) => text.is_animated(),
        }
    }

    pub fn as_shape(&self) -> Option<&Shape> {
        match &self.content {
            GraphicContent::Shape(shape) => Some(shape),
            GraphicContent::Text(_) => None,
        }
    }

    pub fn as_shape_mut(&mut self) -> Option<&mut Shape> {
        match &mut self.content {
            GraphicContent::Shape(shape) => Some(shape),
            GraphicContent::Text(_) => None,
        }
    }

    pub fn as_text(&self) -> Option<&Text> {
        match &self.content {
            GraphicContent::Text(text) => Some(text),
            GraphicContent::Shape(_) => None,
        }
    }

    pub fn as_text_mut(&mut self) -> Option<&mut Text> {
        match &mut self.content {
            GraphicContent::Text(text) => Some(text),
            GraphicContent::Shape(_) => None,
        }
    }

    /// What kind of thing this is, for a list in the interface.
    pub fn kind_label(&self) -> &'static str {
        match &self.content {
            GraphicContent::Shape(shape) => shape.kind.label(),
            GraphicContent::Text(_) => "Text",
        }
    }
}

/// What a graphic actually is.
///
/// Externally tagged, so a file says which kind it holds rather than relying on
/// the shape of the payload — the same rule [`crate::Source`] follows, and for
/// the same reason: a third kind added later must not be ambiguous with the
/// first two.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum GraphicContent {
    Shape(Shape),
    Text(Text),
}

/// Which outline a shape draws.
///
/// The counts are here rather than on [`Shape`] because they are not numbers
/// that can be interpolated: a polygon does not pass through four and a half
/// sides on its way from four to five. Everything that *can* be interpolated is
/// a [`Property`] on the shape itself.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum ShapeKind {
    Rectangle,
    Ellipse,
    /// A regular polygon with `sides` sides, point up.
    Polygon {
        sides: u32,
    },
    /// A star with `points` points, point up. How deep the notches go is
    /// [`Shape::inner_radius`], which is animatable and so does not live here.
    Star {
        points: u32,
    },
}

impl ShapeKind {
    /// The kinds, in the order a menu offers them.
    pub const ALL: [ShapeKind; 4] = [
        ShapeKind::Rectangle,
        ShapeKind::Ellipse,
        ShapeKind::Polygon { sides: 6 },
        ShapeKind::Star { points: 5 },
    ];

    /// The fewest sides or points that enclose an area. Two lines cannot.
    pub const MIN_POINTS: u32 = 3;

    pub fn label(self) -> &'static str {
        match self {
            ShapeKind::Rectangle => "Rectangle",
            ShapeKind::Ellipse => "Ellipse",
            ShapeKind::Polygon { .. } => "Polygon",
            ShapeKind::Star { .. } => "Star",
        }
    }

    /// The side or point count, for the kinds that have one.
    pub fn count(self) -> Option<u32> {
        match self {
            ShapeKind::Polygon { sides } => Some(sides),
            ShapeKind::Star { points } => Some(points),
            ShapeKind::Rectangle | ShapeKind::Ellipse => None,
        }
    }

    /// The same kind with a different side or point count, clamped to something
    /// that encloses an area.
    ///
    /// A count below three is not a rejected edit worth a message — a slider
    /// dragged to two is a slider dragged too far — so it clamps rather than
    /// failing, which is also what keeps a hand-edited file from drawing
    /// nothing.
    pub fn with_count(self, count: u32) -> Self {
        let count = count.max(Self::MIN_POINTS);
        match self {
            ShapeKind::Polygon { .. } => ShapeKind::Polygon { sides: count },
            ShapeKind::Star { .. } => ShapeKind::Star { points: count },
            other => other,
        }
    }
}

/// A filled and stroked outline.
///
/// Sizes are in the pixels of the canvas the graphic is drawn on, so a shape
/// authored in a 1920×1080 sequence keeps its size when that sequence is
/// exported at half resolution — compositing happens at the sequence's own
/// resolution and the picture is scaled on the way out, which is the rule
/// [`export`] already follows for everything else.
///
/// [`export`]: https://docs.rs/ve-export
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Shape {
    pub kind: ShapeKind,
    /// Width and height of the shape's bounding box, before the clip's own
    /// transform. A stroke is drawn centred on the outline, so half of it falls
    /// outside this box.
    pub size: Property<Vec2>,
    /// Corner rounding in pixels. Rectangles only; clamped to half the shorter
    /// side, where a rounded rectangle becomes a stadium.
    #[serde(default)]
    pub corner_radius: Property<f64>,
    /// How deep a star's notches go, as a fraction of the outer radius. Stars
    /// only.
    #[serde(default = "default_inner_radius")]
    pub inner_radius: Property<f64>,
    /// The interior colour. Transparent is no fill.
    pub fill: Property<Rgba>,
    /// The outline colour. Drawn only where `stroke_width` is positive.
    #[serde(default)]
    pub stroke: Property<Rgba>,
    /// Outline thickness in pixels, centred on the outline. Zero is no stroke.
    #[serde(default)]
    pub stroke_width: Property<f64>,
}

fn default_inner_radius() -> Property<f64> {
    Property::constant(Shape::DEFAULT_INNER_RADIUS)
}

impl Shape {
    /// A five-pointed star's notches sit at 38% of its outer radius. Pentagram
    /// geometry puts them at `1/φ²` ≈ 0.382, which is the star everyone draws.
    pub const DEFAULT_INNER_RADIUS: f64 = 0.382;

    /// A shape of `kind` at `size`, filled with `fill` and not stroked.
    pub fn new(kind: ShapeKind, size: Vec2, fill: Rgba) -> Self {
        Shape {
            kind,
            size: Property::constant(size),
            corner_radius: Property::constant(0.0),
            inner_radius: default_inner_radius(),
            fill: Property::constant(fill),
            stroke: Property::constant(Rgba::TRANSPARENT),
            stroke_width: Property::constant(0.0),
        }
    }

    pub fn evaluate(&self, t: Ticks) -> ShapeState {
        ShapeState {
            kind: self.kind,
            size: self.size.evaluate(t),
            corner_radius: self.corner_radius.evaluate(t),
            inner_radius: self.inner_radius.evaluate(t),
            fill: self.fill.evaluate(t),
            stroke: self.stroke.evaluate(t),
            stroke_width: self.stroke_width.evaluate(t),
        }
    }

    pub fn is_animated(&self) -> bool {
        self.size.is_animated()
            || self.corner_radius.is_animated()
            || self.inner_radius.is_animated()
            || self.fill.is_animated()
            || self.stroke.is_animated()
            || self.stroke_width.is_animated()
    }
}

/// Which font a run of text asks for.
///
/// A family name and two attributes rather than a path to a file: a project
/// moved to another machine should find Helvetica rather than fail to open a
/// file that was never part of the edit. What happens when the family is not
/// installed is the rasteriser's business — it substitutes and says so — not
/// the model's.
#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub struct FontSpec {
    pub family: String,
    /// CSS numeric weight: 400 is regular, 700 is bold.
    #[serde(default = "default_weight")]
    pub weight: u16,
    #[serde(default)]
    pub italic: bool,
}

fn default_weight() -> u16 {
    FontSpec::REGULAR
}

impl FontSpec {
    pub const REGULAR: u16 = 400;
    pub const BOLD: u16 = 700;

    pub fn new(family: impl Into<String>) -> Self {
        FontSpec { family: family.into(), weight: Self::REGULAR, italic: false }
    }

    pub fn bold(mut self) -> Self {
        self.weight = Self::BOLD;
        self
    }

    pub fn italic(mut self) -> Self {
        self.italic = true;
        self
    }
}

impl Default for FontSpec {
    /// The family a new title starts on: whatever the system calls its sans
    /// serif. Naming a real family here would be a lie on the machines that do
    /// not have it.
    fn default() -> Self {
        FontSpec::new("sans-serif")
    }
}

/// Where the lines of a text graphic sit relative to each other.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Default, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum TextAlign {
    #[default]
    Left,
    Center,
    Right,
}

impl TextAlign {
    pub const ALL: [TextAlign; 3] = [TextAlign::Left, TextAlign::Center, TextAlign::Right];

    pub fn label(self) -> &'static str {
        match self {
            TextAlign::Left => "Left",
            TextAlign::Center => "Centre",
            TextAlign::Right => "Right",
        }
    }
}

/// A run of text, laid out and drawn as an outline like any other shape.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Text {
    /// What it says. **Not** a [`Property`]: there is no halfway point between
    /// two strings, so there is nothing for an interpolation to return. Text
    /// that appears a character at a time is a text animator over the glyphs of
    /// one string, not a keyframed string.
    pub text: String,
    #[serde(default)]
    pub font: FontSpec,
    /// Em size in pixels.
    pub size: Property<f64>,
    pub fill: Property<Rgba>,
    #[serde(default)]
    pub stroke: Property<Rgba>,
    #[serde(default)]
    pub stroke_width: Property<f64>,
    /// Extra space between characters, in thousandths of an em — the unit every
    /// type tool uses, so a value copied from one means the same here.
    #[serde(default)]
    pub tracking: Property<f64>,
    /// Distance between baselines, as a multiple of the em size.
    #[serde(default = "default_line_height")]
    pub line_height: Property<f64>,
    #[serde(default)]
    pub align: TextAlign,
    /// Width in pixels at which lines wrap, or `None` to wrap only where the
    /// text itself says to.
    ///
    /// A box that wraps is a different object from a run that does not: with no
    /// width there is nothing for [`TextAlign`] to align against but the text's
    /// own extent, which is why the two are one option rather than two.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub wrap_width: Option<f64>,
}

fn default_line_height() -> Property<f64> {
    Property::constant(Text::DEFAULT_LINE_HEIGHT)
}

impl Text {
    /// 1.2 em, the default leading of every type system since metal.
    pub const DEFAULT_LINE_HEIGHT: f64 = 1.2;

    /// A run of text at `size` pixels, filled with `fill`.
    pub fn new(text: impl Into<String>, size: f64, fill: Rgba) -> Self {
        Text {
            text: text.into(),
            font: FontSpec::default(),
            size: Property::constant(size),
            fill: Property::constant(fill),
            stroke: Property::constant(Rgba::TRANSPARENT),
            stroke_width: Property::constant(0.0),
            tracking: Property::constant(0.0),
            line_height: default_line_height(),
            align: TextAlign::default(),
            wrap_width: None,
        }
    }

    pub fn with_font(mut self, font: FontSpec) -> Self {
        self.font = font;
        self
    }

    pub fn with_align(mut self, align: TextAlign) -> Self {
        self.align = align;
        self
    }

    pub fn evaluate(&self, t: Ticks) -> TextState {
        TextState {
            text: self.text.clone(),
            font: self.font.clone(),
            size: self.size.evaluate(t),
            fill: self.fill.evaluate(t),
            stroke: self.stroke.evaluate(t),
            stroke_width: self.stroke_width.evaluate(t),
            tracking: self.tracking.evaluate(t),
            line_height: self.line_height.evaluate(t),
            align: self.align,
            wrap_width: self.wrap_width,
        }
    }

    pub fn is_animated(&self) -> bool {
        self.size.is_animated()
            || self.fill.is_animated()
            || self.stroke.is_animated()
            || self.stroke_width.is_animated()
            || self.tracking.is_animated()
            || self.line_height.is_animated()
    }
}

/// A graphic with every animated parameter resolved at one instant.
///
/// This is what crosses into the rasteriser, and it is deliberately a separate
/// type from [`Graphic`] rather than the same one with the keyframes ignored:
/// by the time a picture is being drawn, *when* is settled, exactly as it is
/// for [`crate::TransformState`] and [`crate::EffectState`].
///
/// It hashes, which is what makes a rasterisation content-addressed: two
/// instants that would draw the same picture have the same hash by
/// construction, so an unchanging title is drawn once however long it is on
/// screen, and anything that would change the picture changes the key. The same
/// argument the render cache makes, one level up.
#[derive(Debug, Clone, PartialEq)]
pub enum GraphicState {
    Shape(ShapeState),
    Text(TextState),
}

impl GraphicState {
    pub fn as_shape(&self) -> Option<&ShapeState> {
        match self {
            GraphicState::Shape(shape) => Some(shape),
            GraphicState::Text(_) => None,
        }
    }

    pub fn as_text(&self) -> Option<&TextState> {
        match self {
            GraphicState::Text(text) => Some(text),
            GraphicState::Shape(_) => None,
        }
    }

    /// A 64-bit identity for what this state draws.
    ///
    /// Everything the rasteriser reads goes into it, so a cached picture cannot
    /// be stale: a parameter that reaches the rasteriser without reaching the
    /// hash would be a picture that does not change when it should, which is
    /// the worst class of bug a cache can have.
    pub fn content_hash(&self) -> u64 {
        use std::hash::{Hash, Hasher};
        let mut hasher = std::collections::hash_map::DefaultHasher::new();
        self.hash(&mut hasher);
        hasher.finish()
    }
}

/// Hashing resolved floats.
///
/// By bits, which makes `-0.0` and `0.0` different keys and every `NaN` its
/// own. Both are the safe direction for a cache: two states that hash apart
/// draw twice, while two that hash together and differ would show the wrong
/// picture.
#[inline]
fn hash_f64<H: std::hash::Hasher>(v: f64, state: &mut H) {
    std::hash::Hash::hash(&v.to_bits(), state);
}

fn hash_vec2<H: std::hash::Hasher>(v: Vec2, state: &mut H) {
    hash_f64(v.x, state);
    hash_f64(v.y, state);
}

fn hash_rgba<H: std::hash::Hasher>(v: Rgba, state: &mut H) {
    hash_f64(v.r, state);
    hash_f64(v.g, state);
    hash_f64(v.b, state);
    hash_f64(v.a, state);
}

/// A [`Shape`] resolved at one instant.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct ShapeState {
    pub kind: ShapeKind,
    pub size: Vec2,
    pub corner_radius: f64,
    pub inner_radius: f64,
    pub fill: Rgba,
    pub stroke: Rgba,
    pub stroke_width: f64,
}

impl ShapeState {
    /// Whether a stroke is actually drawn: a width and a colour that is not
    /// fully transparent.
    pub fn has_stroke(&self) -> bool {
        self.stroke_width > 0.0 && self.stroke.a > 0.0
    }

    pub fn has_fill(&self) -> bool {
        self.fill.a > 0.0
    }
}

impl std::hash::Hash for ShapeState {
    fn hash<H: std::hash::Hasher>(&self, state: &mut H) {
        self.kind.hash(state);
        hash_vec2(self.size, state);
        hash_f64(self.corner_radius, state);
        hash_f64(self.inner_radius, state);
        hash_rgba(self.fill, state);
        hash_rgba(self.stroke, state);
        hash_f64(self.stroke_width, state);
    }
}

/// A [`Text`] resolved at one instant.
#[derive(Debug, Clone, PartialEq)]
pub struct TextState {
    pub text: String,
    pub font: FontSpec,
    pub size: f64,
    pub fill: Rgba,
    pub stroke: Rgba,
    pub stroke_width: f64,
    pub tracking: f64,
    pub line_height: f64,
    pub align: TextAlign,
    pub wrap_width: Option<f64>,
}

impl TextState {
    pub fn has_stroke(&self) -> bool {
        self.stroke_width > 0.0 && self.stroke.a > 0.0
    }

    pub fn has_fill(&self) -> bool {
        self.fill.a > 0.0
    }
}

impl std::hash::Hash for TextState {
    fn hash<H: std::hash::Hasher>(&self, state: &mut H) {
        self.text.hash(state);
        self.font.hash(state);
        hash_f64(self.size, state);
        hash_rgba(self.fill, state);
        hash_rgba(self.stroke, state);
        hash_f64(self.stroke_width, state);
        hash_f64(self.tracking, state);
        hash_f64(self.line_height, state);
        self.align.hash(state);
        match self.wrap_width {
            Some(w) => {
                true.hash(state);
                hash_f64(w, state);
            }
            None => false.hash(state),
        }
    }
}

impl std::hash::Hash for GraphicState {
    fn hash<H: std::hash::Hasher>(&self, state: &mut H) {
        match self {
            // Tagged, so a shape and a run of text cannot collide on the same
            // key by having the same numbers in them.
            GraphicState::Shape(shape) => {
                0u8.hash(state);
                shape.hash(state);
            }
            GraphicState::Text(text) => {
                1u8.hash(state);
                text.hash(state);
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::animation::Interpolation;
    use ve_time::Rate;

    fn a_shape() -> Shape {
        Shape::new(ShapeKind::Rectangle, Vec2::new(400.0, 200.0), Rgba::WHITE)
    }

    #[test]
    fn a_static_graphic_says_it_is_not_animated() {
        let g = Graphic::shape(GraphicId::from_raw(1), "Box", a_shape());
        assert!(!g.is_animated());
    }

    #[test]
    fn keyframing_any_parameter_makes_the_graphic_animated() {
        let mut shape = a_shape();
        shape.corner_radius.set_keyframe(Ticks::ZERO, 0.0, Interpolation::Linear);
        shape.corner_radius.set_keyframe(
            Rate::FPS_25.frame_to_ticks(50),
            40.0,
            Interpolation::Linear,
        );
        let g = Graphic::shape(GraphicId::from_raw(1), "Box", shape);
        assert!(g.is_animated());

        let half = Rate::FPS_25.frame_to_ticks(25);
        let state = g.evaluate(half).as_shape().copied().expect("a shape");
        assert!((state.corner_radius - 20.0).abs() < 1e-9, "{}", state.corner_radius);
    }

    #[test]
    fn two_instants_of_a_static_graphic_hash_the_same() {
        let g = Graphic::shape(GraphicId::from_raw(1), "Box", a_shape());
        let a = g.evaluate(Ticks::ZERO).content_hash();
        let b = g.evaluate(Rate::FPS_25.frame_to_ticks(120)).content_hash();
        assert_eq!(a, b, "nothing changes, so nothing should be redrawn");
    }

    #[test]
    fn every_parameter_reaches_the_hash() {
        // A parameter that reached the rasteriser without reaching the key
        // would draw a stale picture. Each of these changes one thing.
        let base = a_shape();
        let key = |s: &Shape| GraphicState::Shape(s.evaluate(Ticks::ZERO)).content_hash();
        let original = key(&base);

        let mut size = base.clone();
        size.size = Property::constant(Vec2::new(401.0, 200.0));
        assert_ne!(key(&size), original);

        let mut radius = base.clone();
        radius.corner_radius = Property::constant(8.0);
        assert_ne!(key(&radius), original);

        let mut inner = base.clone();
        inner.inner_radius = Property::constant(0.5);
        assert_ne!(key(&inner), original);

        let mut fill = base.clone();
        fill.fill = Property::constant(Rgba::new(1.0, 1.0, 1.0, 0.99));
        assert_ne!(key(&fill), original);

        let mut stroke = base.clone();
        stroke.stroke = Property::constant(Rgba::BLACK);
        assert_ne!(key(&stroke), original);

        let mut width = base.clone();
        width.stroke_width = Property::constant(2.0);
        assert_ne!(key(&width), original);

        let mut kind = base.clone();
        kind.kind = ShapeKind::Ellipse;
        assert_ne!(key(&kind), original);
    }

    #[test]
    fn a_shape_and_a_text_with_the_same_numbers_hash_apart() {
        let shape = GraphicState::Shape(a_shape().evaluate(Ticks::ZERO));
        let text =
            GraphicState::Text(Text::new("hello", 48.0, Rgba::WHITE).evaluate(Ticks::ZERO));
        assert_ne!(shape.content_hash(), text.content_hash());
    }

    #[test]
    fn changing_the_words_changes_the_key() {
        let one = Text::new("hello", 48.0, Rgba::WHITE);
        let mut two = one.clone();
        two.text = "hellp".into();
        assert_ne!(
            GraphicState::Text(one.evaluate(Ticks::ZERO)).content_hash(),
            GraphicState::Text(two.evaluate(Ticks::ZERO)).content_hash()
        );
    }

    #[test]
    fn a_point_count_cannot_go_below_a_triangle() {
        assert_eq!(
            ShapeKind::Polygon { sides: 6 }.with_count(1),
            ShapeKind::Polygon { sides: 3 }
        );
        assert_eq!(ShapeKind::Star { points: 5 }.with_count(0), ShapeKind::Star { points: 3 });
        // A kind with no count is left alone rather than growing one.
        assert_eq!(ShapeKind::Ellipse.with_count(9), ShapeKind::Ellipse);
    }

    #[test]
    fn a_graphic_round_trips_through_json() {
        let g = Graphic::text(
            GraphicId::from_raw(3),
            "Title",
            Text::new("Verge", 96.0, Rgba::WHITE).with_align(TextAlign::Center),
        );
        let json = serde_json::to_string(&g).unwrap();
        let back: Graphic = serde_json::from_str(&json).unwrap();
        assert_eq!(back, g);
    }

    #[test]
    fn a_graphic_written_without_the_optional_fields_still_reads() {
        // A hand-written or older file: only the parameters that have no
        // sensible default are required.
        let json = r#"{
            "id": 4,
            "name": "Box",
            "content": {"shape": {
                "kind": {"kind": "rectangle"},
                "size": {"value": {"x": 100.0, "y": 50.0}},
                "fill": {"value": {"r": 1.0, "g": 0.0, "b": 0.0, "a": 1.0}}
            }}
        }"#;
        let g: Graphic = serde_json::from_str(json).unwrap();
        let shape = g.as_shape().expect("a shape");
        assert_eq!(shape.stroke_width.value, 0.0);
        assert_eq!(shape.inner_radius.value, Shape::DEFAULT_INNER_RADIUS);
    }
}
