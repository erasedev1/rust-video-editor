//! Commands that build and rearrange an effect chain.
//!
//! # One host, not two commands
//!
//! A clip and a composition layer both carry a `Vec<Effect>`, and everything
//! here does the same thing to either: add, remove, reorder, switch off,
//! rename. So rather than a `…Clip…` and a `…Layer…` twin of each command —
//! which is what [`SetClipProperty`] and [`SetLayerProperty`] had to be,
//! because the properties they reach differ in type — these take an
//! [`EffectHost`] saying which chain to edit and are otherwise one
//! implementation.
//!
//! # Order is the chain
//!
//! An effect chain is a pipeline: the first effect sees the clip's picture, the
//! second sees what the first produced. Blurring and then brightening is not
//! the same picture as brightening and then blurring, so [`MoveEffect`] is a
//! real edit rather than a cosmetic reordering of a list, and every command
//! here restores *position* on undo, not just presence.
//!
//! [`SetClipProperty`]: crate::SetClipProperty
//! [`SetLayerProperty`]: crate::SetLayerProperty

use std::any::Any;

use ve_core::registry::EffectRegistry;
use ve_core::{
    builtin_registry, ClipId, CompositionId, Effect, EffectId, LayerId, Project, SequenceId,
};

use crate::{Command, CommandError};

/// Which effect chain a command edits.
///
/// Copy and comparable, so a command can hold one, a drag can check two are the
/// same, and the interface can pass the current selection around without
/// cloning anything.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum EffectHost {
    Clip { sequence: SequenceId, clip: ClipId },
    Layer { composition: CompositionId, layer: LayerId },
}

impl EffectHost {
    pub fn clip(sequence: SequenceId, clip: ClipId) -> Self {
        EffectHost::Clip { sequence, clip }
    }

    pub fn layer(composition: CompositionId, layer: LayerId) -> Self {
        EffectHost::Layer { composition, layer }
    }

    /// The chain, for reading.
    pub fn effects<'a>(&self, project: &'a Project) -> Result<&'a [Effect], CommandError> {
        match *self {
            EffectHost::Clip { sequence, clip } => Ok(&project
                .sequence(sequence)
                .ok_or(CommandError::SequenceNotFound(sequence))?
                .find_clip(clip)
                .ok_or(CommandError::ClipNotFound(clip))?
                .1
                .effects),
            EffectHost::Layer { composition, layer } => Ok(&project
                .composition(composition)
                .ok_or(CommandError::CompositionNotFound(composition))?
                .layer(layer)
                .ok_or(CommandError::LayerNotFound(layer))?
                .effects),
        }
    }

    /// The chain, for editing.
    fn effects_mut<'a>(
        &self,
        project: &'a mut Project,
    ) -> Result<&'a mut Vec<Effect>, CommandError> {
        match *self {
            EffectHost::Clip { sequence, clip } => {
                Ok(&mut crate::clip_mut(project, sequence, clip)?.effects)
            }
            EffectHost::Layer { composition, layer } => Ok(&mut project
                .composition_mut(composition)
                .ok_or(CommandError::CompositionNotFound(composition))?
                .layer_mut(layer)
                .ok_or(CommandError::LayerNotFound(layer))?
                .effects),
        }
    }

    /// Where an effect sits in the chain, or a rejection naming it.
    fn index_of(&self, project: &Project, effect: EffectId) -> Result<usize, CommandError> {
        self.effects(project)?
            .iter()
            .position(|e| e.id == effect)
            .ok_or_else(|| CommandError::Rejected(format!("no effect {effect} here")))
    }
}

/// Adds an effect to a chain.
///
/// The effect is built from a registry descriptor, so it arrives with every
/// parameter the interface will draw a control for, each at its declared
/// default. A kind nothing answers to is refused rather than added as an empty
/// shell: an effect with no parameters looks like a bug in the effect, and the
/// registry is the one place that knows whether it should exist.
#[derive(Debug)]
pub struct AddEffect {
    host: EffectHost,
    kind: String,
    /// Where in the chain to insert, clamped to its length. `None` appends,
    /// which is what an effect added from the menu does.
    at: Option<usize>,
    /// Minted on first apply and kept, so redo re-adds the effect under the ID
    /// that anything captured in the meantime — a keyframe command, a selection
    /// — is still holding.
    id: Option<EffectId>,
    registry: &'static EffectRegistry,
    label: String,
}

impl AddEffect {
    pub fn new(host: EffectHost, kind: impl Into<String>) -> Self {
        AddEffect::from_registry(host, kind, builtin_registry())
    }

    /// Appends at a given position rather than at the end.
    pub fn at(mut self, index: usize) -> Self {
        self.at = Some(index);
        self
    }

    /// Builds against a registry other than the built-in one, which is how a
    /// host that has loaded plugin effects adds one of them.
    pub fn from_registry(
        host: EffectHost,
        kind: impl Into<String>,
        registry: &'static EffectRegistry,
    ) -> Self {
        let kind = kind.into();
        let label = match registry.get(&kind) {
            Some(descriptor) => format!("Add {}", descriptor.name),
            None => "Add Effect".to_string(),
        };
        AddEffect { host, kind, at: None, id: None, registry, label }
    }

    /// The effect this command added, once it has been applied.
    pub fn effect_id(&self) -> Option<EffectId> {
        self.id
    }
}

impl Command for AddEffect {
    fn name(&self) -> &str {
        &self.label
    }

    fn apply(&mut self, project: &mut Project) -> Result<(), CommandError> {
        let descriptor = self
            .registry
            .get(&self.kind)
            .ok_or_else(|| CommandError::Rejected(format!("no effect '{}'", self.kind)))?;
        // Checked before an ID is minted, so a refused add does not burn one.
        let id = match self.id {
            Some(id) => id,
            None => project.new_effect_id(),
        };
        let effect = descriptor.instantiate(id);
        let effects = self.host.effects_mut(project)?;
        let index = self.at.unwrap_or(effects.len()).min(effects.len());
        effects.insert(index, effect);
        self.id = Some(id);
        Ok(())
    }

    fn undo(&mut self, project: &mut Project) -> Result<(), CommandError> {
        let Some(id) = self.id else {
            return Err(CommandError::Rejected("effect was never added".into()));
        };
        let effects = self.host.effects_mut(project)?;
        effects.retain(|e| e.id != id);
        Ok(())
    }

    fn as_any(&self) -> &dyn Any {
        self
    }
}

/// Removes an effect, keeping it and its position for undo.
///
/// The whole effect is captured rather than its kind and parameters being
/// rebuilt on undo, because the parameters may be keyframed and a rebuild from
/// the registry would hand back the defaults.
#[derive(Debug)]
pub struct RemoveEffect {
    host: EffectHost,
    effect: EffectId,
    removed: Option<(usize, Effect)>,
    label: String,
}

impl RemoveEffect {
    pub fn new(host: EffectHost, effect: EffectId) -> Self {
        RemoveEffect { host, effect, removed: None, label: "Remove Effect".into() }
    }
}

impl Command for RemoveEffect {
    fn name(&self) -> &str {
        &self.label
    }

    fn apply(&mut self, project: &mut Project) -> Result<(), CommandError> {
        let index = self.host.index_of(project, self.effect)?;
        let effects = self.host.effects_mut(project)?;
        let effect = effects.remove(index);
        self.label = format!("Remove {}", effect.name);
        self.removed = Some((index, effect));
        Ok(())
    }

    fn undo(&mut self, project: &mut Project) -> Result<(), CommandError> {
        let Some((index, effect)) = self.removed.clone() else {
            return Err(CommandError::Rejected("effect was never removed".into()));
        };
        let effects = self.host.effects_mut(project)?;
        let index = index.min(effects.len());
        effects.insert(index, effect);
        Ok(())
    }

    fn as_any(&self) -> &dyn Any {
        self
    }
}

/// Moves an effect to another position in its chain.
///
/// The destination is **absolute** — "this effect is now at index 2" — rather
/// than a delta, which is what lets a drag restate itself on every pointer move
/// and still merge into one undo step. Undo restores the index the effect was
/// captured at.
#[derive(Debug)]
pub struct MoveEffect {
    host: EffectHost,
    effect: EffectId,
    to: usize,
    from: Option<usize>,
}

impl MoveEffect {
    pub fn new(host: EffectHost, effect: EffectId, to: usize) -> Self {
        MoveEffect { host, effect, to, from: None }
    }
}

impl Command for MoveEffect {
    fn name(&self) -> &str {
        "Reorder Effect"
    }

    fn apply(&mut self, project: &mut Project) -> Result<(), CommandError> {
        let index = self.host.index_of(project, self.effect)?;
        let effects = self.host.effects_mut(project)?;
        let to = self.to.min(effects.len().saturating_sub(1));
        // Captured before the first move only: a merged drag has to undo to
        // where the effect started, not to where the previous pointer move
        // left it.
        self.from.get_or_insert(index);
        if to == index {
            return Ok(());
        }
        let effect = effects.remove(index);
        effects.insert(to, effect);
        Ok(())
    }

    fn undo(&mut self, project: &mut Project) -> Result<(), CommandError> {
        let Some(from) = self.from else {
            return Err(CommandError::Rejected("effect was never moved".into()));
        };
        let index = self.host.index_of(project, self.effect)?;
        let effects = self.host.effects_mut(project)?;
        let effect = effects.remove(index);
        let from = from.min(effects.len());
        effects.insert(from, effect);
        Ok(())
    }

    fn merge(&mut self, next: &dyn Command) -> bool {
        match next.as_any().downcast_ref::<MoveEffect>() {
            Some(other) if other.host == self.host && other.effect == self.effect => {
                self.to = other.to;
                true
            }
            _ => false,
        }
    }

    fn as_any(&self) -> &dyn Any {
        self
    }
}

/// Switches an effect off, or back on.
///
/// Off is not removal: the effect keeps its place, its parameters and its
/// keyframes, and costs nothing while it is off, which is what makes it the
/// thing to reach for when comparing a look against the plate.
#[derive(Debug)]
pub struct SetEffectEnabled {
    host: EffectHost,
    effect: EffectId,
    enabled: bool,
    previous: Option<bool>,
}

impl SetEffectEnabled {
    pub fn new(host: EffectHost, effect: EffectId, enabled: bool) -> Self {
        SetEffectEnabled { host, effect, enabled, previous: None }
    }
}

impl Command for SetEffectEnabled {
    fn name(&self) -> &str {
        if self.enabled {
            "Enable Effect"
        } else {
            "Disable Effect"
        }
    }

    fn apply(&mut self, project: &mut Project) -> Result<(), CommandError> {
        let index = self.host.index_of(project, self.effect)?;
        let effect = &mut self.host.effects_mut(project)?[index];
        self.previous.get_or_insert(effect.enabled);
        effect.enabled = self.enabled;
        Ok(())
    }

    fn undo(&mut self, project: &mut Project) -> Result<(), CommandError> {
        let Some(previous) = self.previous else {
            return Err(CommandError::Rejected("effect was never switched".into()));
        };
        let index = self.host.index_of(project, self.effect)?;
        self.host.effects_mut(project)?[index].enabled = previous;
        Ok(())
    }

    fn as_any(&self) -> &dyn Any {
        self
    }
}

/// Renames an effect within its chain.
///
/// Two blurs doing two different jobs are otherwise both called "Gaussian
/// Blur", which is exactly when a chain stops being readable. Merges while the
/// user types, so a rename is one undo step rather than one per keystroke.
#[derive(Debug)]
pub struct RenameEffect {
    host: EffectHost,
    effect: EffectId,
    name: String,
    previous: Option<String>,
}

impl RenameEffect {
    pub fn new(host: EffectHost, effect: EffectId, name: impl Into<String>) -> Self {
        RenameEffect { host, effect, name: name.into(), previous: None }
    }
}

impl Command for RenameEffect {
    fn name(&self) -> &str {
        "Rename Effect"
    }

    fn apply(&mut self, project: &mut Project) -> Result<(), CommandError> {
        let name = self.name.trim();
        if name.is_empty() {
            return Err(CommandError::Rejected("an effect needs a name".into()));
        }
        let name = name.to_string();
        let index = self.host.index_of(project, self.effect)?;
        let effect = &mut self.host.effects_mut(project)?[index];
        if self.previous.is_none() {
            self.previous = Some(effect.name.clone());
        }
        effect.name = name;
        Ok(())
    }

    fn undo(&mut self, project: &mut Project) -> Result<(), CommandError> {
        let Some(previous) = self.previous.clone() else {
            return Err(CommandError::Rejected("effect was never renamed".into()));
        };
        let index = self.host.index_of(project, self.effect)?;
        self.host.effects_mut(project)?[index].name = previous;
        Ok(())
    }

    fn merge(&mut self, next: &dyn Command) -> bool {
        match next.as_any().downcast_ref::<RenameEffect>() {
            Some(other) if other.host == self.host && other.effect == self.effect => {
                self.name = other.name.clone();
                true
            }
            _ => false,
        }
    }

    fn as_any(&self) -> &dyn Any {
        self
    }
}
