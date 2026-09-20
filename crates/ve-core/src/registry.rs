//! What effects exist, what they are called, and what they take.
//!
//! # Why a registry rather than an enum
//!
//! An [`Effect`] names its kind with a string — `"verge.blur.gaussian"` — and
//! carries its parameters as a list of key/value pairs. Nothing in the edit
//! model knows what a blur *is*. What turns that open-ended pair into something
//! an interface can draw a control for, and a renderer can dispatch on, is this
//! registry: a table of [`EffectDescriptor`]s, each declaring the parameters its
//! effect takes, their types, their ranges and their defaults.
//!
//! The obvious alternative — an enum of every effect, with a struct of typed
//! fields each — is better in exactly one way: it cannot misspell a parameter.
//! It is worse in every other way that matters here. Adding an effect would
//! touch the core crate, the persistence format, the inspector and the
//! animation system, which is four places for a feature that is conceptually
//! one. A third-party effect could not exist at all without a fork. And a
//! project file holding an effect this build has never heard of would fail to
//! load, rather than loading with the effect intact and skipped.
//!
//! So the registry is a *runtime* table rather than a static match. The
//! built-ins fill it in [`EffectRegistry::builtin`]; a plugin host will fill it
//! with more, and nothing downstream will need to change to show or animate
//! them.
//!
//! # What an unknown effect does
//!
//! Nothing — and that is the point. An effect whose kind is not registered
//! keeps its parameters, is shown by name, and is skipped by the renderer, so
//! opening a project made with a plugin you do not have loses nothing and
//! saving it back preserves it. See [`EffectRegistry::describe`].
//!
//! # The descriptor is not the value
//!
//! A descriptor says a blur's radius is a scalar from 0 to 200 defaulting to 8.
//! The *value* on a particular effect is a [`Property<f64>`], which may be
//! animated. That split is what keeps effect parameters inside the one
//! animation system rather than beside it: the registry describes the control,
//! and the keyframes live where every other keyframe lives.
//!
//! [`Property<f64>`]: crate::Property

use std::collections::HashMap;
use std::sync::OnceLock;

use crate::effect::{Effect, EffectState, ParamValue};
use crate::geometry::{Rgba, Vec2};
use crate::id::EffectId;

/// What a parameter holds, with the bounds and default the interface needs to
/// build a control without knowing which effect it belongs to.
#[derive(Debug, Clone, PartialEq)]
pub enum ParamKind {
    /// A number, dragged on a slider between `min` and `max`.
    Scalar {
        default: f64,
        min: f64,
        max: f64,
    },
    /// A pair of numbers. `min` and `max` apply to both components.
    Point {
        default: Vec2,
        min: f64,
        max: f64,
    },
    Color {
        default: Rgba,
    },
    /// A switch. Not animatable — see [`ParamValue::Bool`].
    Bool {
        default: bool,
    },
    /// One of a fixed list, by index. Stepped, so a choice that needs to change
    /// over time does it with `Interpolation::Hold`.
    Choice {
        default: u32,
        options: Vec<String>,
    },
}

impl ParamKind {
    pub fn scalar(default: f64, min: f64, max: f64) -> Self {
        ParamKind::Scalar { default, min, max }
    }

    pub fn point(default: Vec2, min: f64, max: f64) -> Self {
        ParamKind::Point { default, min, max }
    }

    pub fn choice(default: u32, options: &[&str]) -> Self {
        ParamKind::Choice {
            default,
            options: options.iter().map(|s| (*s).to_string()).collect(),
        }
    }

    /// A fresh value at this parameter's default.
    pub fn instantiate(&self) -> ParamValue {
        match self {
            ParamKind::Scalar { default, .. } => ParamValue::scalar(*default),
            ParamKind::Point { default, .. } => ParamValue::point(*default),
            ParamKind::Color { default } => ParamValue::color(*default),
            ParamKind::Bool { default } => ParamValue::Bool(*default),
            ParamKind::Choice { default, .. } => ParamValue::Choice(*default),
        }
    }

    /// Whether a value is of this kind. A project file is untrusted input, and
    /// a radius that arrives as a colour has to be replaced rather than
    /// reinterpreted.
    pub fn accepts(&self, value: &ParamValue) -> bool {
        matches!(
            (self, value),
            (ParamKind::Scalar { .. }, ParamValue::Scalar(_))
                | (ParamKind::Point { .. }, ParamValue::Point(_))
                | (ParamKind::Color { .. }, ParamValue::Color(_))
                | (ParamKind::Bool { .. }, ParamValue::Bool(_))
                | (ParamKind::Choice { .. }, ParamValue::Choice(_))
        )
    }

    /// Clamps a resolved number into the declared range.
    ///
    /// Applied when the value is *read*, not when it is written, because a
    /// keyframed parameter is only a number once it has been evaluated — and
    /// because a curve that overshoots its range on the way between two
    /// keyframes should be caught here rather than being refused at the
    /// keyframe.
    pub fn clamp_scalar(&self, value: f64) -> f64 {
        match self {
            ParamKind::Scalar { min, max, .. } | ParamKind::Point { min, max, .. } => {
                value.clamp(*min, *max)
            }
            _ => value,
        }
    }

    /// Whether a property of this kind can carry keyframes at all.
    pub fn is_animatable(&self) -> bool {
        matches!(
            self,
            ParamKind::Scalar { .. } | ParamKind::Point { .. } | ParamKind::Color { .. }
        )
    }
}

/// One tunable input, as the registry declares it.
#[derive(Debug, Clone, PartialEq)]
pub struct ParamDescriptor {
    /// Stable key, stored in the project file. Never shown.
    pub key: String,
    /// Shown beside the control.
    pub label: String,
    pub kind: ParamKind,
    /// What the control's tooltip says.
    pub hint: String,
}

impl ParamDescriptor {
    pub fn new(key: &str, label: &str, kind: ParamKind, hint: &str) -> Self {
        ParamDescriptor {
            key: key.to_string(),
            label: label.to_string(),
            kind,
            hint: hint.to_string(),
        }
    }
}

/// Where an effect sits in the menu that offers it.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub enum EffectCategory {
    Blur,
    Color,
    Stylize,
    Distort,
    Matte,
}

impl EffectCategory {
    /// Every category, in the order the menu lists them.
    pub const ALL: [EffectCategory; 5] = [
        EffectCategory::Blur,
        EffectCategory::Color,
        EffectCategory::Stylize,
        EffectCategory::Distort,
        EffectCategory::Matte,
    ];

    pub fn label(self) -> &'static str {
        match self {
            EffectCategory::Blur => "Blur",
            EffectCategory::Color => "Colour",
            EffectCategory::Stylize => "Stylise",
            EffectCategory::Distort => "Distort",
            EffectCategory::Matte => "Matte",
        }
    }
}

/// Everything known about one kind of effect.
#[derive(Debug, Clone, PartialEq)]
pub struct EffectDescriptor {
    /// Registry key, e.g. `"verge.blur.gaussian"`. Namespaced by vendor so a
    /// plugin cannot take a name the editor might later want.
    pub kind: String,
    pub name: String,
    pub category: EffectCategory,
    /// One line, shown in the menu that offers the effect.
    pub summary: String,
    /// In the order the inspector shows them.
    pub params: Vec<ParamDescriptor>,
}

impl EffectDescriptor {
    pub fn new(kind: &str, name: &str, category: EffectCategory, summary: &str) -> Self {
        EffectDescriptor {
            kind: kind.to_string(),
            name: name.to_string(),
            category,
            summary: summary.to_string(),
            params: Vec::new(),
        }
    }

    pub fn with_param(mut self, param: ParamDescriptor) -> Self {
        self.params.push(param);
        self
    }

    pub fn param(&self, key: &str) -> Option<&ParamDescriptor> {
        self.params.iter().find(|p| p.key == key)
    }

    /// A fresh effect of this kind, every parameter at its default.
    pub fn instantiate(&self, id: EffectId) -> Effect {
        let mut effect = Effect::new(id, self.kind.clone(), self.name.clone());
        effect.params =
            self.params.iter().map(|p| (p.key.clone(), p.kind.instantiate())).collect();
        effect
    }

    /// Brings an effect's parameters into line with this descriptor: missing
    /// ones are added at their default, ones of the wrong type are replaced,
    /// and the order is the declared one.
    ///
    /// Parameters the descriptor does **not** declare are kept, at the end.
    /// That is what makes a project written by a newer build — or by a build
    /// with a newer version of the same plugin — load without quietly dropping
    /// the settings it will be saved back with.
    pub fn conform(&self, effect: &mut Effect) {
        let mut authored: HashMap<String, ParamValue> = effect.params.drain(..).collect();
        let mut conformed = Vec::with_capacity(self.params.len());
        for descriptor in &self.params {
            let value = match authored.remove(&descriptor.key) {
                Some(value) if descriptor.kind.accepts(&value) => value,
                _ => descriptor.kind.instantiate(),
            };
            conformed.push((descriptor.key.clone(), value));
        }
        // Whatever is left is undeclared. Sorted, because a `HashMap` hands
        // them back in an order that varies from run to run, and a project file
        // that changes every time it is saved is a file that cannot be diffed.
        let mut extra: Vec<(String, ParamValue)> = authored.into_iter().collect();
        extra.sort_by(|a, b| a.0.cmp(&b.0));
        conformed.extend(extra);
        effect.params = conformed;
    }
}

/// The table of effects this build can offer.
///
/// Lookup is by kind; iteration is in registration order, which puts the
/// built-ins in the order [`EffectRegistry::builtin`] lists them and anything
/// registered later after them.
#[derive(Debug, Clone, Default)]
pub struct EffectRegistry {
    entries: Vec<EffectDescriptor>,
    index: HashMap<String, usize>,
}

impl EffectRegistry {
    pub fn new() -> Self {
        EffectRegistry::default()
    }

    /// Adds a descriptor, returning `false` and changing nothing if its kind is
    /// already taken.
    ///
    /// Refusing rather than replacing is deliberate: two effects answering to
    /// one name would make which one a project file meant depend on load order.
    /// The refusal is the return value rather than a log line, because the only
    /// caller that can do anything about it — a plugin host — is the one that
    /// knows which plugin it was loading.
    pub fn register(&mut self, descriptor: EffectDescriptor) -> bool {
        if self.index.contains_key(&descriptor.kind) {
            return false;
        }
        self.index.insert(descriptor.kind.clone(), self.entries.len());
        self.entries.push(descriptor);
        true
    }

    pub fn get(&self, kind: &str) -> Option<&EffectDescriptor> {
        self.index.get(kind).map(|i| &self.entries[*i])
    }

    pub fn contains(&self, kind: &str) -> bool {
        self.index.contains_key(kind)
    }

    pub fn len(&self) -> usize {
        self.entries.len()
    }

    pub fn is_empty(&self) -> bool {
        self.entries.is_empty()
    }

    pub fn iter(&self) -> impl Iterator<Item = &EffectDescriptor> {
        self.entries.iter()
    }

    /// The effects in one category, in registration order.
    pub fn in_category(
        &self,
        category: EffectCategory,
    ) -> impl Iterator<Item = &EffectDescriptor> {
        self.entries.iter().filter(move |e| e.category == category)
    }

    /// A fresh effect of `kind`, or `None` if nothing answers to that name.
    pub fn instantiate(&self, kind: &str, id: EffectId) -> Option<Effect> {
        self.get(kind).map(|d| d.instantiate(id))
    }

    /// How an effect should be shown: its descriptor, or `None` for one this
    /// build does not know.
    ///
    /// The interface uses this rather than assuming a lookup succeeds, so an
    /// effect from a plugin that is not installed appears in the chain, named
    /// and switchable, instead of vanishing from a project that still contains
    /// it.
    pub fn describe(&self, effect: &Effect) -> Option<&EffectDescriptor> {
        self.get(&effect.kind)
    }

    /// Conforms every effect it recognises, leaving the rest untouched.
    ///
    /// Called by the project loader: a file is untrusted input, and a parameter
    /// of the wrong type would otherwise reach a shader as a number it is not.
    pub fn conform(&self, effects: &mut [Effect]) {
        for effect in effects {
            if let Some(descriptor) = self.get(&effect.kind) {
                descriptor.conform(effect);
            }
        }
    }

    /// Resolves a chain of effects at clip-relative time `t`, in order.
    ///
    /// Two kinds of effect are left out, and the difference matters:
    ///
    /// * **Disabled** — the user switched it off, so it contributes nothing and
    ///   costs nothing. The effect stays on the clip.
    /// * **Unregistered** — this build has no pass for it. Running the chain
    ///   without it is the only thing that can be done, and it is also the
    ///   right thing: the rest of the chain still applies, and the effect is
    ///   still there to be saved back and to work again on a build that has it.
    ///
    /// Both are silent. A warning per frame for an effect the user can see
    /// greyed out in the inspector would be noise thirty times a second.
    pub fn evaluate_chain(&self, effects: &[Effect], t: ve_time::Ticks) -> Vec<EffectState> {
        effects
            .iter()
            .filter(|e| e.enabled)
            .filter_map(|e| {
                let descriptor = self.get(&e.kind)?;
                Some(e.evaluate(t, Some(descriptor)))
            })
            .collect()
    }

    /// The built-in catalogue.
    ///
    /// Ranges are what the *control* offers, not what the shader tolerates: a
    /// blur radius stops at 200 px because a slider that reaches further is
    /// useless, not because 300 would break anything.
    pub fn builtin() -> Self {
        let mut registry = EffectRegistry::new();
        for descriptor in builtin_descriptors() {
            registry.register(descriptor);
        }
        registry
    }
}

/// The process-wide registry of built-in effects.
///
/// Built once on first use. A host that registers plugin effects will own a
/// registry of its own and pass it down; this is the convenience every caller
/// that only needs the built-ins uses, so that the catalogue is not rebuilt per
/// repaint of an inspector.
pub fn builtin_registry() -> &'static EffectRegistry {
    static REGISTRY: OnceLock<EffectRegistry> = OnceLock::new();
    REGISTRY.get_or_init(EffectRegistry::builtin)
}

/// Kind keys for the built-ins, so that callers naming one — the inspector's
/// "add blur" button, a test, the renderer's dispatch — cannot misspell it.
pub mod kinds {
    pub const GAUSSIAN_BLUR: &str = "verge.blur.gaussian";
    pub const COLOR_ADJUST: &str = "verge.color.adjust";
    pub const SHARPEN: &str = "verge.stylize.sharpen";
    pub const TRANSFORM: &str = "verge.distort.transform";
    pub const SHAPE_MASK: &str = "verge.matte.shape";
    pub const LUMA_KEY: &str = "verge.matte.luma";
}

fn builtin_descriptors() -> Vec<EffectDescriptor> {
    vec![
        EffectDescriptor::new(
            kinds::GAUSSIAN_BLUR,
            "Gaussian Blur",
            EffectCategory::Blur,
            "Softens the picture with a true Gaussian, separated into two passes",
        )
        .with_param(ParamDescriptor::new(
            "radius",
            "Radius",
            ParamKind::scalar(8.0, 0.0, 200.0),
            "Standard deviation of the blur, in composition pixels",
        ))
        .with_param(ParamDescriptor::new(
            "direction",
            "Direction",
            ParamKind::choice(0, &["Both", "Horizontal", "Vertical"]),
            "Blurring one axis only costs one pass instead of two",
        )),
        EffectDescriptor::new(
            kinds::COLOR_ADJUST,
            "Colour Adjust",
            EffectCategory::Color,
            "Exposure, contrast, saturation and gamma in one pass",
        )
        .with_param(ParamDescriptor::new(
            "exposure",
            "Exposure",
            ParamKind::scalar(0.0, -6.0, 6.0),
            "Stops of light. Each step doubles or halves the picture's brightness",
        ))
        .with_param(ParamDescriptor::new(
            "contrast",
            "Contrast",
            ParamKind::scalar(0.0, -1.0, 1.0),
            "Pushes values away from mid grey, or towards it",
        ))
        .with_param(ParamDescriptor::new(
            "saturation",
            "Saturation",
            ParamKind::scalar(1.0, 0.0, 3.0),
            "0 is monochrome, 1 leaves colour alone",
        ))
        .with_param(ParamDescriptor::new(
            "gamma",
            "Gamma",
            ParamKind::scalar(1.0, 0.1, 4.0),
            "Bends the midtones without moving black or white",
        ))
        .with_param(ParamDescriptor::new(
            "tint",
            "Tint",
            ParamKind::Color { default: Rgba::WHITE },
            "Multiplies every channel. White leaves the picture alone",
        )),
        EffectDescriptor::new(
            kinds::SHARPEN,
            "Sharpen",
            EffectCategory::Stylize,
            "Unsharp mask: adds back what a small blur took away",
        )
        .with_param(ParamDescriptor::new(
            "amount",
            "Amount",
            ParamKind::scalar(0.5, 0.0, 4.0),
            "How much of the difference to add back",
        ))
        .with_param(ParamDescriptor::new(
            "radius",
            "Radius",
            ParamKind::scalar(1.0, 0.5, 16.0),
            "How far the detail being sharpened extends, in pixels",
        )),
        EffectDescriptor::new(
            kinds::TRANSFORM,
            "Transform",
            EffectCategory::Distort,
            "Moves, scales and rotates inside the chain, after earlier effects",
        )
        .with_param(ParamDescriptor::new(
            "position",
            "Position",
            ParamKind::point(Vec2::ZERO, -16384.0, 16384.0),
            "Offset from the centre, in composition pixels",
        ))
        .with_param(ParamDescriptor::new(
            "scale",
            "Scale",
            ParamKind::point(Vec2::ONE, -32.0, 32.0),
            "1 is the picture's own size. A negative value mirrors it",
        ))
        .with_param(ParamDescriptor::new(
            "rotation",
            "Rotation",
            ParamKind::scalar(0.0, -3600.0, 3600.0),
            "Clockwise degrees",
        ))
        .with_param(ParamDescriptor::new(
            "anchor",
            "Anchor",
            ParamKind::point(Vec2::splat(0.5), -8.0, 8.0),
            "The point it turns and scales about, (0.5, 0.5) being the centre",
        ))
        .with_param(ParamDescriptor::new(
            "opacity",
            "Opacity",
            ParamKind::scalar(1.0, 0.0, 1.0),
            "Fades what the chain has produced so far",
        )),
        EffectDescriptor::new(
            kinds::SHAPE_MASK,
            "Shape Mask",
            EffectCategory::Matte,
            "Keeps what falls inside a rectangle or ellipse and hides the rest",
        )
        .with_param(ParamDescriptor::new(
            "shape",
            "Shape",
            ParamKind::choice(1, &["Rectangle", "Ellipse"]),
            "The outline the mask is cut to",
        ))
        .with_param(ParamDescriptor::new(
            "center",
            "Centre",
            ParamKind::point(Vec2::splat(0.5), -4.0, 4.0),
            "Where the shape sits, in fractions of the frame",
        ))
        .with_param(ParamDescriptor::new(
            "size",
            "Size",
            ParamKind::point(Vec2::splat(0.5), 0.0, 4.0),
            "Width and height of the shape, in fractions of the frame",
        ))
        .with_param(ParamDescriptor::new(
            "rotation",
            "Rotation",
            ParamKind::scalar(0.0, -3600.0, 3600.0),
            "Clockwise degrees about the shape's centre",
        ))
        .with_param(ParamDescriptor::new(
            "feather",
            "Feather",
            ParamKind::scalar(0.0, 0.0, 512.0),
            "How far the edge fades, in pixels",
        ))
        .with_param(ParamDescriptor::new(
            "opacity",
            "Opacity",
            ParamKind::scalar(1.0, 0.0, 1.0),
            "How much of the masked-out area is hidden. 1 hides it entirely",
        ))
        .with_param(ParamDescriptor::new(
            "invert",
            "Invert",
            ParamKind::Bool { default: false },
            "Hide what is inside the shape instead of what is outside",
        )),
        EffectDescriptor::new(
            kinds::LUMA_KEY,
            "Luma Key",
            EffectCategory::Matte,
            "Turns brightness into transparency",
        )
        .with_param(ParamDescriptor::new(
            "threshold",
            "Threshold",
            ParamKind::scalar(0.2, 0.0, 1.0),
            "Brightness below which the picture is fully transparent",
        ))
        .with_param(ParamDescriptor::new(
            "softness",
            "Softness",
            ParamKind::scalar(0.1, 0.0, 1.0),
            "How wide the ramp from transparent to opaque is",
        ))
        .with_param(ParamDescriptor::new(
            "invert",
            "Invert",
            ParamKind::Bool { default: false },
            "Key out the bright parts instead of the dark ones",
        )),
    ]
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::animation::Interpolation;
    use crate::effect::ParamValue;
    use ve_time::Ticks;

    fn id(raw: u64) -> EffectId {
        EffectId::from_raw(raw)
    }

    #[test]
    fn every_builtin_has_a_unique_kind_and_registers() {
        let registry = EffectRegistry::builtin();
        assert_eq!(registry.len(), builtin_descriptors().len());
        for descriptor in builtin_descriptors() {
            assert!(registry.contains(&descriptor.kind), "{} missing", descriptor.kind);
        }
    }

    #[test]
    fn every_builtin_parameter_has_a_unique_key_and_a_hint() {
        for descriptor in EffectRegistry::builtin().iter() {
            let mut keys: Vec<&str> =
                descriptor.params.iter().map(|p| p.key.as_str()).collect();
            let count = keys.len();
            keys.sort_unstable();
            keys.dedup();
            assert_eq!(
                keys.len(),
                count,
                "{} has two parameters with one key",
                descriptor.kind
            );
            for param in &descriptor.params {
                assert!(
                    !param.hint.is_empty(),
                    "{}.{} has no hint",
                    descriptor.kind,
                    param.key
                );
                assert!(!param.label.is_empty());
            }
        }
    }

    #[test]
    fn registering_a_kind_twice_is_refused_rather_than_replacing() {
        let mut registry = EffectRegistry::new();
        let first = EffectDescriptor::new("x.y", "First", EffectCategory::Blur, "one");
        let second = EffectDescriptor::new("x.y", "Second", EffectCategory::Blur, "two");
        assert!(registry.register(first));
        assert!(!registry.register(second));
        assert_eq!(registry.get("x.y").unwrap().name, "First");
        assert_eq!(registry.len(), 1);
    }

    #[test]
    fn an_instantiated_effect_carries_every_declared_parameter_at_its_default() {
        let registry = EffectRegistry::builtin();
        let effect = registry.instantiate(kinds::GAUSSIAN_BLUR, id(1)).unwrap();
        assert_eq!(effect.kind, kinds::GAUSSIAN_BLUR);
        assert_eq!(effect.name, "Gaussian Blur");
        assert!(effect.enabled);
        assert_eq!(effect.param("radius").unwrap().as_scalar_at(Ticks::ZERO), Some(8.0));
        assert_eq!(effect.params.len(), 2);
        assert!(!effect.is_animated());
    }

    #[test]
    fn an_unknown_kind_instantiates_nothing() {
        assert!(EffectRegistry::builtin().instantiate("nobody.at.all", id(1)).is_none());
    }

    #[test]
    fn conforming_adds_missing_parameters_and_replaces_mistyped_ones() {
        let registry = EffectRegistry::builtin();
        let mut effect = Effect::new(id(1), kinds::COLOR_ADJUST, "Colour Adjust")
            // The right key with the wrong type, as a hand-edited file might hold.
            .with_param("saturation", ParamValue::Bool(true))
            // A key this build does not declare, as a newer one might write.
            .with_param("highlights", ParamValue::scalar(0.25));
        registry.conform(std::slice::from_mut(&mut effect));

        let descriptor = registry.get(kinds::COLOR_ADJUST).unwrap();
        for (i, param) in descriptor.params.iter().enumerate() {
            assert_eq!(effect.params[i].0, param.key, "declared order is not kept");
        }
        // Replaced with the default rather than reinterpreted.
        assert_eq!(effect.param("saturation").unwrap().as_scalar_at(Ticks::ZERO), Some(1.0));
        // Kept, so saving the project back does not lose it.
        assert_eq!(effect.param("highlights").unwrap().as_scalar_at(Ticks::ZERO), Some(0.25));
    }

    #[test]
    fn conforming_keeps_a_value_the_user_set() {
        let registry = EffectRegistry::builtin();
        let mut effect = registry.instantiate(kinds::GAUSSIAN_BLUR, id(1)).unwrap();
        if let Some(ParamValue::Scalar(radius)) = effect.param_mut("radius") {
            radius.set_keyframe(Ticks::ZERO, 40.0, Interpolation::Linear);
        }
        registry.conform(std::slice::from_mut(&mut effect));
        assert_eq!(effect.param("radius").unwrap().as_scalar_at(Ticks::ZERO), Some(40.0));
        assert!(effect.is_animated());
    }

    #[test]
    fn conforming_leaves_an_unregistered_effect_entirely_alone() {
        let registry = EffectRegistry::builtin();
        let mut effect = Effect::new(id(1), "someone.elses.glow", "Glow")
            .with_param("intensity", ParamValue::scalar(3.0));
        let before = effect.clone();
        registry.conform(std::slice::from_mut(&mut effect));
        assert_eq!(effect, before);
        assert!(registry.describe(&effect).is_none());
    }

    #[test]
    fn a_scalar_is_clamped_to_the_range_the_control_offers() {
        let kind = ParamKind::scalar(8.0, 0.0, 200.0);
        assert_eq!(kind.clamp_scalar(-3.0), 0.0);
        assert_eq!(kind.clamp_scalar(1000.0), 200.0);
        assert_eq!(kind.clamp_scalar(12.0), 12.0);
    }

    #[test]
    fn switches_and_choices_are_not_animatable() {
        assert!(!ParamKind::Bool { default: false }.is_animatable());
        assert!(!ParamKind::choice(0, &["a", "b"]).is_animatable());
        assert!(ParamKind::scalar(0.0, 0.0, 1.0).is_animatable());
        assert!(ParamKind::Color { default: Rgba::WHITE }.is_animatable());
    }

    #[test]
    fn every_category_holds_at_least_one_builtin() {
        let registry = EffectRegistry::builtin();
        for category in EffectCategory::ALL {
            assert!(
                registry.in_category(category).next().is_some(),
                "{} is an empty menu",
                category.label()
            );
        }
    }

    #[test]
    fn a_chain_resolves_in_order_and_skips_what_it_cannot_run() {
        let registry = EffectRegistry::builtin();
        let blur = registry.instantiate(kinds::GAUSSIAN_BLUR, id(1)).unwrap();
        let mut off = registry.instantiate(kinds::SHARPEN, id(2)).unwrap();
        off.enabled = false;
        let foreign = Effect::new(id(3), "someone.elses.glow", "Glow");
        let colour = registry.instantiate(kinds::COLOR_ADJUST, id(4)).unwrap();

        let chain = registry.evaluate_chain(&[blur, off, foreign, colour], Ticks::ZERO);
        let kinds: Vec<&str> = chain.iter().map(|e| e.kind.as_str()).collect();
        assert_eq!(kinds, vec![kinds::GAUSSIAN_BLUR, kinds::COLOR_ADJUST]);
        assert_eq!(chain[0].scalar("radius", 0.0), 8.0);
        assert_eq!(chain[0].choice("direction", 9), 0);
    }

    #[test]
    fn an_animated_parameter_resolves_to_where_it_is_at_that_instant() {
        let registry = EffectRegistry::builtin();
        let mut blur = registry.instantiate(kinds::GAUSSIAN_BLUR, id(1)).unwrap();
        if let Some(ParamValue::Scalar(radius)) = blur.param_mut("radius") {
            radius.set_keyframe(Ticks::ZERO, 0.0, Interpolation::Linear);
            radius.set_keyframe(Ticks::new(1000), 100.0, Interpolation::Linear);
        }
        let chain = registry.evaluate_chain(std::slice::from_ref(&blur), Ticks::new(500));
        assert!((chain[0].scalar("radius", 0.0) - 50.0).abs() < 1e-9);
    }

    #[test]
    fn a_curve_that_overshoots_its_range_is_clamped_when_it_is_read() {
        // A bezier handle can carry a value past the end of the slider that set
        // it; the shader must still get a number it can use.
        let registry = EffectRegistry::builtin();
        let mut blur = registry.instantiate(kinds::GAUSSIAN_BLUR, id(1)).unwrap();
        if let Some(ParamValue::Scalar(radius)) = blur.param_mut("radius") {
            radius.set_keyframe(Ticks::ZERO, -500.0, Interpolation::Linear);
        }
        let chain = registry.evaluate_chain(std::slice::from_ref(&blur), Ticks::ZERO);
        assert_eq!(chain[0].scalar("radius", 1.0), 0.0);
    }

    #[test]
    fn an_unregistered_effect_still_evaluates_when_asked_directly() {
        let effect = Effect::new(id(1), "someone.elses.glow", "Glow")
            .with_param("intensity", ParamValue::scalar(3.0));
        let state = effect.evaluate(Ticks::ZERO, None);
        assert_eq!(state.kind, "someone.elses.glow");
        assert_eq!(state.scalar("intensity", 0.0), 3.0);
        // A key nobody declared reads as the fallback rather than panicking.
        assert_eq!(state.scalar("radius", 7.0), 7.0);
    }

    #[test]
    fn the_process_wide_registry_is_built_once() {
        let a = builtin_registry();
        let b = builtin_registry();
        assert!(std::ptr::eq(a, b));
    }
}
