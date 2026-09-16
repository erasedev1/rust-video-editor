//! Commands that set and animate clip properties.
//!
//! One command covers every animatable property rather than there being a
//! `SetOpacity`, a `SetScale` and so on: [`ClipProperty`] names the target and
//! [`PropertyValue`] carries the value. Adding an animatable parameter to the
//! inspector therefore needs no new command type.

use std::any::Any;

use ve_core::{ClipId, EffectId, Interpolation, ParamValue, Project, Rgba, SequenceId, Vec2};
use ve_time::Ticks;

use crate::{Command, CommandError};

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

fn clip_mut(
    project: &mut Project,
    sequence: SequenceId,
    clip: ClipId,
) -> Result<&mut ve_core::Clip, CommandError> {
    project
        .sequence_mut(sequence)
        .ok_or(CommandError::SequenceNotFound(sequence))?
        .find_clip_mut(clip)
        .map(|(_, c)| c)
        .ok_or(CommandError::ClipNotFound(clip))
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
