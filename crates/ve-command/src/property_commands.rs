//! Commands that set and animate the properties of clips and layers.
//!
//! One command covers every animatable property rather than there being a
//! `SetOpacity`, a `SetScale` and so on: [`ClipProperty`] names the target and
//! [`PropertyValue`] carries the value. Adding an animatable parameter to the
//! inspector therefore needs no new command type.
//!
//! A composition layer carries the same transform, audio and effect parameters a
//! clip does, so [`SetLayerProperty`] reuses the same target enum and the same
//! macro rather than duplicating the property table. Where they differ is what
//! has to be looked up to reach the property, which is all the two commands do
//! separately.

use std::any::Any;

use ve_core::{
    ClipId, CompositionId, EffectId, Interpolation, LayerId, ParamValue, Project, Rgba,
    SequenceId, Vec2,
};
use ve_time::Ticks;

use crate::{clip_mut, Command, CommandError};

/// Which animatable value on a clip a command targets.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ClipProperty {
    Position,
    Scale,
    Rotation,
    Anchor,
    Opacity,
    Volume,
    Pan,
    /// A parameter on one of the clip's effects.
    EffectParam {
        effect: EffectId,
        key: String,
    },
}

impl ClipProperty {
    pub fn label(&self) -> &str {
        match self {
            ClipProperty::Position => "Position",
            ClipProperty::Scale => "Scale",
            ClipProperty::Rotation => "Rotation",
            ClipProperty::Anchor => "Anchor",
            ClipProperty::Opacity => "Opacity",
            ClipProperty::Volume => "Volume",
            ClipProperty::Pan => "Pan",
            ClipProperty::EffectParam { key, .. } => key,
        }
    }
}

/// A value for any of the property kinds in [`ClipProperty`].
#[derive(Debug, Clone, Copy, PartialEq)]
pub enum PropertyValue {
    Scalar(f64),
    Point(Vec2),
    Color(Rgba),
}

impl PropertyValue {
    /// How many numbers the value is made of: one for a scalar, two for a
    /// point, four for a colour.
    ///
    /// A graph editor draws a *curve per number*, not per property — the x and
    /// y of a position move independently and have to be editable that way —
    /// so this and [`PropertyValue::channel`] are how it takes a value apart
    /// without knowing which kind it has.
    pub fn channel_count(&self) -> usize {
        match self {
            PropertyValue::Scalar(_) => 1,
            PropertyValue::Point(_) => 2,
            PropertyValue::Color(_) => 4,
        }
    }

    /// One of those numbers, or `0.0` past the end.
    pub fn channel(&self, index: usize) -> f64 {
        match (self, index) {
            (PropertyValue::Scalar(v), 0) => *v,
            (PropertyValue::Point(v), 0) => v.x,
            (PropertyValue::Point(v), 1) => v.y,
            (PropertyValue::Color(c), 0) => c.r,
            (PropertyValue::Color(c), 1) => c.g,
            (PropertyValue::Color(c), 2) => c.b,
            (PropertyValue::Color(c), 3) => c.a,
            _ => 0.0,
        }
    }

    /// The same value with one channel replaced, which is what dragging a
    /// single curve in the graph editor produces.
    pub fn with_channel(self, index: usize, to: f64) -> PropertyValue {
        match (self, index) {
            (PropertyValue::Scalar(_), 0) => PropertyValue::Scalar(to),
            (PropertyValue::Point(v), 0) => PropertyValue::Point(Vec2::new(to, v.y)),
            (PropertyValue::Point(v), 1) => PropertyValue::Point(Vec2::new(v.x, to)),
            (PropertyValue::Color(c), 0) => PropertyValue::Color(Rgba { r: to, ..c }),
            (PropertyValue::Color(c), 1) => PropertyValue::Color(Rgba { g: to, ..c }),
            (PropertyValue::Color(c), 2) => PropertyValue::Color(Rgba { b: to, ..c }),
            (PropertyValue::Color(c), 3) => PropertyValue::Color(Rgba { a: to, ..c }),
            _ => self,
        }
    }

    /// What to call a channel of this value in a legend.
    pub fn channel_label(&self, index: usize) -> &'static str {
        match (self, index) {
            (PropertyValue::Point(_), 0) => "x",
            (PropertyValue::Point(_), 1) => "y",
            (PropertyValue::Color(_), 0) => "r",
            (PropertyValue::Color(_), 1) => "g",
            (PropertyValue::Color(_), 2) => "b",
            (PropertyValue::Color(_), 3) => "a",
            _ => "",
        }
    }
}

/// Applies `op` to the property named by `target` on `clip`.
///
/// The macro exists because the properties have different `T`, so a single
/// function cannot name them; each arm is the same three lines against a
/// different concrete type.
macro_rules! with_property {
    ($clip:expr, $target:expr, |$p:ident, $kind:ident| $body:expr) => {{
        let clip = $clip;
        match $target {
            ClipProperty::Position => {
                let $p = &mut clip.transform.position;
                #[allow(unused)]
                use PropertyValue::Point as $kind;
                $body
            }
            ClipProperty::Scale => {
                let $p = &mut clip.transform.scale;
                #[allow(unused)]
                use PropertyValue::Point as $kind;
                $body
            }
            ClipProperty::Anchor => {
                let $p = &mut clip.transform.anchor;
                #[allow(unused)]
                use PropertyValue::Point as $kind;
                $body
            }
            ClipProperty::Rotation => {
                let $p = &mut clip.transform.rotation;
                #[allow(unused)]
                use PropertyValue::Scalar as $kind;
                $body
            }
            ClipProperty::Opacity => {
                let $p = &mut clip.transform.opacity;
                #[allow(unused)]
                use PropertyValue::Scalar as $kind;
                $body
            }
            ClipProperty::Volume => {
                let $p = &mut clip.audio.volume;
                #[allow(unused)]
                use PropertyValue::Scalar as $kind;
                $body
            }
            ClipProperty::Pan => {
                let $p = &mut clip.audio.pan;
                #[allow(unused)]
                use PropertyValue::Scalar as $kind;
                $body
            }
            ClipProperty::EffectParam { effect, key } => {
                let param = clip
                    .effects
                    .iter_mut()
                    .find(|e| e.id == *effect)
                    .and_then(|e| e.param_mut(key))
                    .ok_or_else(|| {
                        CommandError::Rejected(format!("no effect parameter '{key}'"))
                    })?;
                match param {
                    ParamValue::Scalar($p) => {
                        #[allow(unused)]
                        use PropertyValue::Scalar as $kind;
                        $body
                    }
                    ParamValue::Point($p) => {
                        #[allow(unused)]
                        use PropertyValue::Point as $kind;
                        $body
                    }
                    ParamValue::Color($p) => {
                        #[allow(unused)]
                        use PropertyValue::Color as $kind;
                        $body
                    }
                    _ => {
                        return Err(CommandError::Rejected(
                            "that effect parameter is not animatable".into(),
                        ))
                    }
                }
            }
        }
    }};
}

pub(crate) use with_property;

fn layer_mut(
    project: &mut Project,
    composition: CompositionId,
    layer: LayerId,
) -> Result<&mut ve_core::CompositionLayer, CommandError> {
    project
        .composition_mut(composition)
        .ok_or(CommandError::CompositionNotFound(composition))?
        .layer_mut(layer)
        .ok_or(CommandError::LayerNotFound(layer))
}

/// Sets a property's static value, leaving any keyframes alone.
///
/// Merges with later sets of the same property, so dragging an inspector slider
/// is one undo step.
#[derive(Debug)]
pub struct SetClipProperty {
    sequence: SequenceId,
    clip: ClipId,
    target: ClipProperty,
    value: PropertyValue,
    previous: Option<PropertyValue>,
    label: String,
}

impl SetClipProperty {
    pub fn new(
        sequence: SequenceId,
        clip: ClipId,
        target: ClipProperty,
        value: PropertyValue,
    ) -> Self {
        let label = format!("Set {}", target.label());
        SetClipProperty { sequence, clip, target, value, previous: None, label }
    }
}

impl Command for SetClipProperty {
    fn name(&self) -> &str {
        &self.label
    }

    fn apply(&mut self, project: &mut Project) -> Result<(), CommandError> {
        let target = self.target.clone();
        let value = self.value;
        let clip = clip_mut(project, self.sequence, self.clip)?;
        let previous = with_property!(clip, &target, |p, Variant| {
            let old = Variant(p.value);
            match value {
                Variant(v) => p.value = v,
                other => {
                    return Err(CommandError::Rejected(format!(
                        "{other:?} is the wrong type for {}",
                        target.label()
                    )))
                }
            }
            old
        });
        self.previous.get_or_insert(previous);
        Ok(())
    }

    fn undo(&mut self, project: &mut Project) -> Result<(), CommandError> {
        let previous = self
            .previous
            .ok_or_else(|| CommandError::Rejected("property was never set".into()))?;
        let target = self.target.clone();
        let clip = clip_mut(project, self.sequence, self.clip)?;
        with_property!(clip, &target, |p, Variant| {
            if let Variant(v) = previous {
                p.value = v;
            }
        });
        Ok(())
    }

    fn merge(&mut self, next: &dyn Command) -> bool {
        match next.as_any().downcast_ref::<SetClipProperty>() {
            Some(other) if other.clip == self.clip && other.target == self.target => {
                self.value = other.value;
                true
            }
            _ => false,
        }
    }

    fn as_any(&self) -> &dyn Any {
        self
    }
}

/// Sets a property on a composition layer.
///
/// The layer-side twin of [`SetClipProperty`]: same targets, same values, same
/// merging behaviour, reached through a composition and a layer instead of a
/// sequence and a clip.
#[derive(Debug)]
pub struct SetLayerProperty {
    composition: CompositionId,
    layer: LayerId,
    target: ClipProperty,
    value: PropertyValue,
    previous: Option<PropertyValue>,
    label: String,
}

impl SetLayerProperty {
    pub fn new(
        composition: CompositionId,
        layer: LayerId,
        target: ClipProperty,
        value: PropertyValue,
    ) -> Self {
        let label = format!("Set {}", target.label());
        SetLayerProperty { composition, layer, target, value, previous: None, label }
    }
}

impl Command for SetLayerProperty {
    fn name(&self) -> &str {
        &self.label
    }

    fn apply(&mut self, project: &mut Project) -> Result<(), CommandError> {
        let target = self.target.clone();
        let value = self.value;
        let layer = layer_mut(project, self.composition, self.layer)?;
        let previous = with_property!(layer, &target, |p, Variant| {
            let old = Variant(p.value);
            match value {
                Variant(v) => p.value = v,
                other => {
                    return Err(CommandError::Rejected(format!(
                        "{other:?} is the wrong type for {}",
                        target.label()
                    )))
                }
            }
            old
        });
        self.previous.get_or_insert(previous);
        Ok(())
    }

    fn undo(&mut self, project: &mut Project) -> Result<(), CommandError> {
        let previous = self
            .previous
            .ok_or_else(|| CommandError::Rejected("property was never set".into()))?;
        let target = self.target.clone();
        let layer = layer_mut(project, self.composition, self.layer)?;
        with_property!(layer, &target, |p, Variant| {
            if let Variant(v) = previous {
                p.value = v;
            }
        });
        Ok(())
    }

    fn merge(&mut self, next: &dyn Command) -> bool {
        match next.as_any().downcast_ref::<SetLayerProperty>() {
            Some(other)
                if other.layer == self.layer
                    && other.composition == self.composition
                    && other.target == self.target =>
            {
                self.value = other.value;
                true
            }
            _ => false,
        }
    }

    fn as_any(&self) -> &dyn Any {
        self
    }
}

/// Adds or replaces a keyframe at a clip-local time.
#[derive(Debug)]
pub struct SetClipKeyframe {
    sequence: SequenceId,
    clip: ClipId,
    target: ClipProperty,
    time: Ticks,
    value: PropertyValue,
    interpolation: Interpolation,
    /// The keyframe this one displaced, if any. `Some(None)` means the apply
    /// ran and displaced nothing, which undo must distinguish from "never ran".
    displaced: Option<Option<PropertyValue>>,
    displaced_interp: Option<Interpolation>,
    label: String,
}

impl SetClipKeyframe {
    pub fn new(
        sequence: SequenceId,
        clip: ClipId,
        target: ClipProperty,
        time: Ticks,
        value: PropertyValue,
        interpolation: Interpolation,
    ) -> Self {
        let label = format!("Keyframe {}", target.label());
        SetClipKeyframe {
            sequence,
            clip,
            target,
            time,
            value,
            interpolation,
            displaced: None,
            displaced_interp: None,
            label,
        }
    }
}

impl Command for SetClipKeyframe {
    fn name(&self) -> &str {
        &self.label
    }

    fn apply(&mut self, project: &mut Project) -> Result<(), CommandError> {
        let target = self.target.clone();
        let (value, time, interp) = (self.value, self.time, self.interpolation);
        let clip = clip_mut(project, self.sequence, self.clip)?;
        let displaced = with_property!(clip, &target, |p, Variant| {
            let v = match value {
                Variant(v) => v,
                other => {
                    return Err(CommandError::Rejected(format!(
                        "{other:?} is the wrong type for {}",
                        target.label()
                    )))
                }
            };
            p.set_keyframe(time, v, interp).map(|k| (Variant(k.value), k.interpolation))
        });
        self.displaced = Some(displaced.map(|(v, _)| v));
        self.displaced_interp = displaced.map(|(_, i)| i);
        Ok(())
    }

    fn undo(&mut self, project: &mut Project) -> Result<(), CommandError> {
        let displaced = self
            .displaced
            .ok_or_else(|| CommandError::Rejected("keyframe was never set".into()))?;
        let (target, time) = (self.target.clone(), self.time);
        let interp = self.displaced_interp.unwrap_or_default();
        let clip = clip_mut(project, self.sequence, self.clip)?;
        with_property!(clip, &target, |p, Variant| {
            match displaced {
                // There was a keyframe here before; put it back.
                Some(Variant(v)) => {
                    p.set_keyframe(time, v, interp);
                }
                // There was not; removing ours restores the original state.
                _ => {
                    p.remove_keyframe(time);
                }
            }
        });
        Ok(())
    }

    fn as_any(&self) -> &dyn Any {
        self
    }
}

/// Deletes a keyframe.
#[derive(Debug)]
pub struct RemoveClipKeyframe {
    sequence: SequenceId,
    clip: ClipId,
    target: ClipProperty,
    time: Ticks,
    removed: Option<(PropertyValue, Interpolation)>,
    label: String,
}

impl RemoveClipKeyframe {
    pub fn new(sequence: SequenceId, clip: ClipId, target: ClipProperty, time: Ticks) -> Self {
        let label = format!("Remove Keyframe {}", target.label());
        RemoveClipKeyframe { sequence, clip, target, time, removed: None, label }
    }
}

impl Command for RemoveClipKeyframe {
    fn name(&self) -> &str {
        &self.label
    }

    fn apply(&mut self, project: &mut Project) -> Result<(), CommandError> {
        let (target, time) = (self.target.clone(), self.time);
        let clip = clip_mut(project, self.sequence, self.clip)?;
        let removed = with_property!(clip, &target, |p, Variant| {
            p.remove_keyframe(time).map(|k| (Variant(k.value), k.interpolation))
        });
        match removed {
            Some(r) => {
                self.removed = Some(r);
                Ok(())
            }
            None => Err(CommandError::Rejected("no keyframe at that time".into())),
        }
    }

    fn undo(&mut self, project: &mut Project) -> Result<(), CommandError> {
        let (value, interp) = self
            .removed
            .ok_or_else(|| CommandError::Rejected("keyframe was never removed".into()))?;
        let (target, time) = (self.target.clone(), self.time);
        let clip = clip_mut(project, self.sequence, self.clip)?;
        with_property!(clip, &target, |p, Variant| {
            if let Variant(v) = value {
                p.set_keyframe(time, v, interp);
            }
        });
        Ok(())
    }

    fn as_any(&self) -> &dyn Any {
        self
    }
}
