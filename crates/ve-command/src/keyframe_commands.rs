//! Editing the keyframes on a property, and reading them back.
//!
//! # One command, every keyframe edit
//!
//! Adding a keyframe, deleting a handful, dragging one along the timeline,
//! scaling a whole span, pasting a copied curve and throwing the animation away
//! are all the same shape: *the keyframes on these properties become those
//! keyframes*. [`EditKeyframes`] is that command, and [`KeyframeEdit`] says
//! which of the six it is.
//!
//! The alternative — a command type per operation — would mean six undo paths
//! to keep exact, six merge rules, and six places to extend when a seventh
//! operation arrives. Here there is one of each.
//!
//! # Undo restores the list, rather than replaying an inverse
//!
//! Every edit captures the affected properties whole — static value and
//! keyframes — before it changes anything, and undo puts that back. A property
//! carries a handful of keyframes rather than a timeline's worth of clips, so
//! the copy is cheap, and it makes undo exact for the edits where an inverse
//! would not be: retiming two keyframes onto the same tick collapses them, and
//! no sequence of moves brings the lost one back.
//!
//! # Every edit is absolute
//!
//! A drag restates its whole gesture on every pointer move rather than sending
//! a delta: "these keyframes are now at these times", not "move them 3 ticks
//! left". Applying the same edit twice therefore lands in the same place, which
//! is exactly what lets the history collapse a drag into one undo step —
//! merging is then "keep my snapshot, take your destination" and cannot drift.

use std::any::Any;

use ve_core::{
    AudioProperties, Clip, ClipId, CompositionLayer, Effect, Interpolation, Keyframe,
    ParamValue, Project, Property, Rgba, SequenceId, Transform, Vec2,
};
use ve_time::Ticks;

use crate::property_commands::with_property;
use crate::{clip_mut, ClipProperty, Command, CommandError, PropertyValue};

/// One keyframe, with its value carried as a [`PropertyValue`] so that
/// keyframes from properties of different types can be held in one list.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct KeyframePoint {
    /// Clip-relative, like every keyframe time in the model.
    pub time: Ticks,
    pub value: PropertyValue,
    pub interpolation: Interpolation,
}

/// A property as it was before an edit: enough to put it back exactly.
#[derive(Debug, Clone, PartialEq)]
pub struct PropertyState {
    /// The static value, which [`KeyframeEdit::Freeze`] changes.
    pub value: PropertyValue,
    pub keyframes: Vec<KeyframePoint>,
}

/// What [`EditKeyframes`] does to the properties it names.
#[derive(Debug, Clone, PartialEq)]
pub enum KeyframeEdit {
    /// Adds a keyframe, or replaces the one already at that time.
    Set { time: Ticks, value: PropertyValue, interpolation: Interpolation },
    /// Deletes the keyframes at these times. Times with no keyframe are
    /// ignored, so deleting a selection that undo has already emptied is not an
    /// error.
    Remove(Vec<Ticks>),
    /// Gives the property's keyframes these times, in their current order.
    ///
    /// This is every retiming gesture: one keyframe dragged, a selection slid,
    /// a span scaled. See [`Property::set_keyframe_times`].
    ///
    /// [`Property::set_keyframe_times`]: ve_core::Property::set_keyframe_times
    SetTimes(Vec<Ticks>),
    /// Changes the easing that leaves a keyframe — what the graph editor's
    /// handles set.
    SetInterpolation { time: Ticks, interpolation: Interpolation },
    /// Adds keyframes, each landing at `at` plus its own time.
    ///
    /// The times are therefore offsets from a shared origin rather than
    /// absolute, which is what keeps the shape of a copy intact when it spans
    /// several properties: two curves a second apart stay a second apart. A
    /// time that already has a keyframe is overwritten, as pasting over
    /// something always is.
    Insert { keyframes: Vec<KeyframePoint>, at: Ticks },
    /// Drops every keyframe, leaving the property at the value it had at `at`.
    Freeze { at: Ticks },
}

impl KeyframeEdit {
    fn label(&self) -> &'static str {
        match self {
            KeyframeEdit::Set { .. } => "Keyframe",
            KeyframeEdit::Remove(_) => "Delete Keyframes",
            KeyframeEdit::SetTimes(_) => "Retime Keyframes",
            KeyframeEdit::SetInterpolation { .. } => "Set Easing",
            KeyframeEdit::Insert { .. } => "Paste Keyframes",
            KeyframeEdit::Freeze { .. } => "Remove Animation",
        }
    }

    /// Whether two edits are the same gesture continuing, which is what the
    /// history needs to know to collapse a drag into one undo step.
    ///
    /// A retime or a handle drag restates itself on every pointer move and so
    /// merges; adding and deleting keyframes are discrete acts, and two of them
    /// in a row are two things the user did.
    fn continues(&self, next: &KeyframeEdit) -> bool {
        match (self, next) {
            (KeyframeEdit::SetTimes(_), KeyframeEdit::SetTimes(_)) => true,
            (
                KeyframeEdit::SetInterpolation { time: a, .. },
                KeyframeEdit::SetInterpolation { time: b, .. },
            ) => a == b,
            _ => false,
        }
    }
}

/// Adds, deletes, retimes, eases, pastes or clears keyframes.
///
/// Several properties at once, as one undo step: retiming a selection that
/// spans position and opacity is one gesture, and so is one entry in the
/// history.
#[derive(Debug)]
pub struct EditKeyframes {
    sequence: SequenceId,
    clip: ClipId,
    /// One edit per property. Properties are edited independently — a retime
    /// carries its own times per property — rather than sharing one edit.
    edits: Vec<(ClipProperty, KeyframeEdit)>,
    /// What the properties held before the first apply, in the same order.
    /// `None` until then, which is how undo tells "never ran" from "ran and
    /// changed nothing".
    before: Option<Vec<PropertyState>>,
    label: String,
}

impl EditKeyframes {
    pub fn new(
        sequence: SequenceId,
        clip: ClipId,
        edits: Vec<(ClipProperty, KeyframeEdit)>,
    ) -> Self {
        let label = edits
            .first()
            .map(|(_, e)| e.label().to_string())
            .unwrap_or_else(|| "Keyframe".to_string());
        EditKeyframes { sequence, clip, edits, before: None, label }
    }

    /// The common case: one property, one edit.
    pub fn one(
        sequence: SequenceId,
        clip: ClipId,
        target: ClipProperty,
        edit: KeyframeEdit,
    ) -> Self {
        EditKeyframes::new(sequence, clip, vec![(target, edit)])
    }

    fn targets_match(&self, other: &EditKeyframes) -> bool {
        self.edits.len() == other.edits.len()
            && self.edits.iter().zip(&other.edits).all(|((a, _), (b, _))| a == b)
    }
}

impl Command for EditKeyframes {
    fn name(&self) -> &str {
        &self.label
    }

    fn apply(&mut self, project: &mut Project) -> Result<(), CommandError> {
        let edits = self.edits.clone();
        let capture = self.before.is_none();
        let clip = clip_mut(project, self.sequence, self.clip)?;

        // Captured before anything is touched, so a failure part-way through
        // the list can put back what the earlier edits already changed.
        let mut captured = Vec::with_capacity(edits.len());
        for (target, _) in &edits {
            captured.push(read_property(clip, target)?);
        }

        for (index, (target, edit)) in edits.iter().enumerate() {
            if let Err(e) = apply_edit(clip, target, edit) {
                for ((undo_target, _), state) in edits.iter().zip(&captured).take(index) {
                    let _ = write_property(clip, undo_target, state);
                }
                return Err(e);
            }
        }

        if capture {
            self.before = Some(captured);
        }
        Ok(())
    }

    fn undo(&mut self, project: &mut Project) -> Result<(), CommandError> {
        let before = self
            .before
            .clone()
            .ok_or_else(|| CommandError::Rejected("keyframes were never edited".into()))?;
        let edits = self.edits.clone();
        let clip = clip_mut(project, self.sequence, self.clip)?;
        for ((target, _), state) in edits.iter().zip(&before) {
            write_property(clip, target, state)?;
        }
        Ok(())
    }

    fn merge(&mut self, next: &dyn Command) -> bool {
        let Some(other) = next.as_any().downcast_ref::<EditKeyframes>() else { return false };
        if other.clip != self.clip || !self.targets_match(other) {
            return false;
        }
        if !self
            .edits
            .iter()
            .zip(&other.edits)
            .all(|((_, mine), (_, theirs))| mine.continues(theirs))
        {
            return false;
        }
        // Keep this command's snapshot — it is where the gesture started — and
        // take the newer destination. Both edits are absolute, so the result is
        // the same as if the gesture had gone straight there.
        for (mine, theirs) in self.edits.iter_mut().zip(&other.edits) {
            mine.1 = theirs.1.clone();
        }
        true
    }

    fn as_any(&self) -> &dyn Any {
        self
    }
}

/// Applies one edit to one property of a clip.
fn apply_edit(
    clip: &mut Clip,
    target: &ClipProperty,
    edit: &KeyframeEdit,
) -> Result<(), CommandError> {
    let label = target.label().to_string();
    with_property!(clip, target, |p, Variant| {
        match edit {
            KeyframeEdit::Set { time, value, interpolation } => {
                let Variant(v) = *value else {
                    return Err(CommandError::Rejected(format!(
                        "{value:?} is the wrong type for {label}"
                    )));
                };
                p.set_keyframe(*time, v, *interpolation);
            }
            KeyframeEdit::Remove(times) => {
                for time in times {
                    p.remove_keyframe(*time);
                }
            }
            KeyframeEdit::SetTimes(times) => {
                if !p.set_keyframe_times(times) {
                    return Err(CommandError::Rejected(format!(
                        "{label} has {} keyframes, not {}",
                        p.keyframes().len(),
                        times.len()
                    )));
                }
            }
            KeyframeEdit::SetInterpolation { time, interpolation } => {
                if p.set_interpolation(*time, *interpolation).is_none() {
                    return Err(CommandError::Rejected(format!(
                        "{label} has no keyframe there"
                    )));
                }
            }
            KeyframeEdit::Insert { keyframes, at } => {
                for kf in keyframes {
                    let Variant(v) = kf.value else { continue };
                    p.set_keyframe(*at + kf.time, v, kf.interpolation);
                }
            }
            KeyframeEdit::Freeze { at } => p.freeze_at(*at),
        }
        Ok::<(), CommandError>(())
    })
}

/// Reads a property whole, for undo.
fn read_property(clip: &Clip, target: &ClipProperty) -> Result<PropertyState, CommandError> {
    property_ref(clip, target)
        .map(|p| PropertyState { value: p.value(), keyframes: p.keyframes() })
        .ok_or_else(|| {
            CommandError::Rejected(format!("{} is not animatable here", target.label()))
        })
}

/// Puts a property back exactly as [`read_property`] found it.
fn write_property(
    clip: &mut Clip,
    target: &ClipProperty,
    state: &PropertyState,
) -> Result<(), CommandError> {
    with_property!(clip, target, |p, Variant| {
        if let Variant(v) = state.value {
            p.value = v;
        }
        let kfs = state
            .keyframes
            .iter()
            .filter_map(|k| match k.value {
                Variant(v) => {
                    Some(Keyframe { time: k.time, value: v, interpolation: k.interpolation })
                }
                _ => None,
            })
            .collect();
        p.set_keyframes(kfs);
        Ok::<(), CommandError>(())
    })
}

/// Anything carrying the standard animatable properties: a clip, or a layer in
/// a composition.
///
/// The two are the same set of properties reached through different owners,
/// which is why [`ClipProperty`] names both and why reading one is written once
/// here rather than twice.
pub trait Animated {
    fn transform(&self) -> &Transform;
    fn audio(&self) -> &AudioProperties;
    fn effects(&self) -> &[Effect];
}

impl Animated for Clip {
    fn transform(&self) -> &Transform {
        &self.transform
    }
    fn audio(&self) -> &AudioProperties {
        &self.audio
    }
    fn effects(&self) -> &[Effect] {
        &self.effects
    }
}

impl Animated for CompositionLayer {
    fn transform(&self) -> &Transform {
        &self.transform
    }
    fn audio(&self) -> &AudioProperties {
        &self.audio
    }
    fn effects(&self) -> &[Effect] {
        &self.effects
    }
}

/// One of the model's properties, borrowed without caring which type it holds.
///
/// The read-side twin of the macro the commands mutate through: the interface
/// draws keyframes, curves and animated-or-not marks for properties of three
/// different types, and this is what lets it do that in one pass rather than
/// three.
#[derive(Debug, Clone, Copy)]
pub enum PropertyRef<'a> {
    Scalar(&'a Property<f64>),
    Point(&'a Property<Vec2>),
    Color(&'a Property<Rgba>),
}

/// Borrows the property a [`ClipProperty`] names, or `None` when the target
/// does not resolve — a deleted effect, or a parameter that is not animatable.
pub fn property_ref<'a>(
    item: &'a impl Animated,
    target: &ClipProperty,
) -> Option<PropertyRef<'a>> {
    let transform = item.transform();
    Some(match target {
        ClipProperty::Position => PropertyRef::Point(&transform.position),
        ClipProperty::Scale => PropertyRef::Point(&transform.scale),
        ClipProperty::Anchor => PropertyRef::Point(&transform.anchor),
        ClipProperty::Rotation => PropertyRef::Scalar(&transform.rotation),
        ClipProperty::Opacity => PropertyRef::Scalar(&transform.opacity),
        ClipProperty::Volume => PropertyRef::Scalar(&item.audio().volume),
        ClipProperty::Pan => PropertyRef::Scalar(&item.audio().pan),
        ClipProperty::EffectParam { effect, key } => {
            let param = item.effects().iter().find(|e| e.id == *effect)?.param(key)?;
            match param {
                ParamValue::Scalar(p) => PropertyRef::Scalar(p),
                ParamValue::Point(p) => PropertyRef::Point(p),
                ParamValue::Color(p) => PropertyRef::Color(p),
                ParamValue::Bool(_) | ParamValue::Choice(_) => return None,
            }
        }
    })
}

impl PropertyRef<'_> {
    /// The static value, which is what the property reads as with no keyframes.
    pub fn value(&self) -> PropertyValue {
        match self {
            PropertyRef::Scalar(p) => PropertyValue::Scalar(p.value),
            PropertyRef::Point(p) => PropertyValue::Point(p.value),
            PropertyRef::Color(p) => PropertyValue::Color(p.value),
        }
    }

    /// The value at clip-relative time `at`, animation included.
    pub fn evaluate(&self, at: Ticks) -> PropertyValue {
        match self {
            PropertyRef::Scalar(p) => PropertyValue::Scalar(p.evaluate(at)),
            PropertyRef::Point(p) => PropertyValue::Point(p.evaluate(at)),
            PropertyRef::Color(p) => PropertyValue::Color(p.evaluate(at)),
        }
    }

    pub fn is_animated(&self) -> bool {
        match self {
            PropertyRef::Scalar(p) => p.is_animated(),
            PropertyRef::Point(p) => p.is_animated(),
            PropertyRef::Color(p) => p.is_animated(),
        }
    }

    pub fn keyframe_count(&self) -> usize {
        match self {
            PropertyRef::Scalar(p) => p.keyframes().len(),
            PropertyRef::Point(p) => p.keyframes().len(),
            PropertyRef::Color(p) => p.keyframes().len(),
        }
    }

    /// Every keyframe, in time order, with its value type erased.
    pub fn keyframes(&self) -> Vec<KeyframePoint> {
        fn collect<T: Copy>(
            kfs: &[Keyframe<T>],
            wrap: impl Fn(T) -> PropertyValue,
        ) -> Vec<KeyframePoint> {
            kfs.iter()
                .map(|k| KeyframePoint {
                    time: k.time,
                    value: wrap(k.value),
                    interpolation: k.interpolation,
                })
                .collect()
        }
        match self {
            PropertyRef::Scalar(p) => collect(p.keyframes(), PropertyValue::Scalar),
            PropertyRef::Point(p) => collect(p.keyframes(), PropertyValue::Point),
            PropertyRef::Color(p) => collect(p.keyframes(), PropertyValue::Color),
        }
    }

    /// The times of every keyframe, which is what a retime restates.
    pub fn keyframe_times(&self) -> Vec<Ticks> {
        match self {
            PropertyRef::Scalar(p) => p.keyframes().iter().map(|k| k.time).collect(),
            PropertyRef::Point(p) => p.keyframes().iter().map(|k| k.time).collect(),
            PropertyRef::Color(p) => p.keyframes().iter().map(|k| k.time).collect(),
        }
    }

    /// The keyframe at exactly `time`, if there is one.
    pub fn keyframe_at(&self, time: Ticks) -> Option<KeyframePoint> {
        self.keyframes().into_iter().find(|k| k.time == time)
    }
}

/// Every property of a clip that can hold keyframes, in the order the inspector
/// and the animation editor list them.
///
/// Transform first, then sound, then whatever the clip's effects expose — which
/// is the order they are applied in, and the order the inspector already shows.
pub fn animatable_properties(item: &impl Animated) -> Vec<ClipProperty> {
    let mut properties = vec![
        ClipProperty::Position,
        ClipProperty::Scale,
        ClipProperty::Rotation,
        ClipProperty::Anchor,
        ClipProperty::Opacity,
        ClipProperty::Volume,
        ClipProperty::Pan,
    ];
    for effect in item.effects() {
        for (key, param) in &effect.params {
            if matches!(
                param,
                ParamValue::Scalar(_) | ParamValue::Point(_) | ParamValue::Color(_)
            ) {
                properties
                    .push(ClipProperty::EffectParam { effect: effect.id, key: key.clone() });
            }
        }
    }
    properties
}
