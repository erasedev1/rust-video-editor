//! Commands that create graphics and change what they draw.
//!
//! A graphic is project-level, like a composition, so these reach it by ID
//! rather than through a sequence and a track — but they are the same
//! [`Command`] contract as cutting, which is what puts "typed a letter into a
//! title" and "trimmed a shot" in one history.
//!
//! # Three kinds of change, three commands
//!
//! * [`SetGraphicProperty`] for anything animatable — a size, a colour, a
//!   radius. Merges, so dragging a slider is one undo step, and the property it
//!   writes is the same [`Property`] the transform uses, which is how
//!   [`EditGraphicKeyframes`] keyframes it with the machinery that already
//!   existed.
//! * [`SetGraphicText`] for the words. Merges too, so typing a sentence is one
//!   undo step rather than forty.
//! * [`SetGraphicOption`] for the choices that are not numbers: which shape,
//!   how many points, which font, how the lines align. None of those has a
//!   halfway point, so none of them is animatable, and putting them through the
//!   property commands would mean inventing an interpolation for "rectangle to
//!   ellipse".
//!
//! [`Property`]: ve_core::Property

use std::any::Any;

use ve_core::{
    FontSpec, Graphic, GraphicContent, GraphicId, Project, Shape, ShapeKind, Text, TextAlign,
};
use ve_time::Ticks;

use crate::keyframe_commands::{KeyframeEdit, PropertyRef, PropertyState};
use crate::{Command, CommandError, PropertyValue};

/// Resolves a graphic for mutation, turning a missing one into a typed error
/// rather than a panic.
fn graphic_mut(project: &mut Project, id: GraphicId) -> Result<&mut Graphic, CommandError> {
    project.graphic_mut(id).ok_or(CommandError::GraphicNotFound(id))
}

/// Which animatable value on a graphic a command targets.
///
/// Fill, stroke and stroke width are shared between a shape and a run of text,
/// because they mean the same thing on both and are drawn by the same code. The
/// rest belong to one kind; naming one that does not apply is a rejected edit
/// with a message, not a silent no-op.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum GraphicProperty {
    /// A shape's bounding box.
    Size,
    CornerRadius,
    InnerRadius,
    /// Text's em size. Separate from [`GraphicProperty::Size`] because it is
    /// one number rather than two, and a target whose type depends on what it
    /// points at would be a type error waiting to happen.
    FontSize,
    Tracking,
    LineHeight,
    Fill,
    Stroke,
    StrokeWidth,
}

impl GraphicProperty {
    pub fn label(self) -> &'static str {
        match self {
            GraphicProperty::Size => "Size",
            GraphicProperty::CornerRadius => "Corner Radius",
            GraphicProperty::InnerRadius => "Inner Radius",
            GraphicProperty::FontSize => "Font Size",
            GraphicProperty::Tracking => "Tracking",
            GraphicProperty::LineHeight => "Line Height",
            GraphicProperty::Fill => "Fill",
            GraphicProperty::Stroke => "Stroke",
            GraphicProperty::StrokeWidth => "Stroke Width",
        }
    }
}

/// Applies `op` to the property `target` names on a graphic.
///
/// The same shape as the clip-side macro and for the same reason: the
/// properties have different `T`, so one function cannot name them all.
macro_rules! with_graphic_property {
    ($graphic:expr, $target:expr, |$p:ident, $kind:ident| $body:expr) => {{
        let graphic = $graphic;
        let target = $target;
        match (&mut graphic.content, target) {
            (GraphicContent::Shape(shape), GraphicProperty::Size) => {
                let $p = &mut shape.size;
                #[allow(unused)]
                use PropertyValue::Point as $kind;
                $body
            }
            (GraphicContent::Shape(shape), GraphicProperty::CornerRadius) => {
                let $p = &mut shape.corner_radius;
                #[allow(unused)]
                use PropertyValue::Scalar as $kind;
                $body
            }
            (GraphicContent::Shape(shape), GraphicProperty::InnerRadius) => {
                let $p = &mut shape.inner_radius;
                #[allow(unused)]
                use PropertyValue::Scalar as $kind;
                $body
            }
            (GraphicContent::Shape(shape), GraphicProperty::Fill) => {
                let $p = &mut shape.fill;
                #[allow(unused)]
                use PropertyValue::Color as $kind;
                $body
            }
            (GraphicContent::Shape(shape), GraphicProperty::Stroke) => {
                let $p = &mut shape.stroke;
                #[allow(unused)]
                use PropertyValue::Color as $kind;
                $body
            }
            (GraphicContent::Shape(shape), GraphicProperty::StrokeWidth) => {
                let $p = &mut shape.stroke_width;
                #[allow(unused)]
                use PropertyValue::Scalar as $kind;
                $body
            }
            (GraphicContent::Text(text), GraphicProperty::FontSize) => {
                let $p = &mut text.size;
                #[allow(unused)]
                use PropertyValue::Scalar as $kind;
                $body
            }
            (GraphicContent::Text(text), GraphicProperty::Tracking) => {
                let $p = &mut text.tracking;
                #[allow(unused)]
                use PropertyValue::Scalar as $kind;
                $body
            }
            (GraphicContent::Text(text), GraphicProperty::LineHeight) => {
                let $p = &mut text.line_height;
                #[allow(unused)]
                use PropertyValue::Scalar as $kind;
                $body
            }
            (GraphicContent::Text(text), GraphicProperty::Fill) => {
                let $p = &mut text.fill;
                #[allow(unused)]
                use PropertyValue::Color as $kind;
                $body
            }
            (GraphicContent::Text(text), GraphicProperty::Stroke) => {
                let $p = &mut text.stroke;
                #[allow(unused)]
                use PropertyValue::Color as $kind;
                $body
            }
            (GraphicContent::Text(text), GraphicProperty::StrokeWidth) => {
                let $p = &mut text.stroke_width;
                #[allow(unused)]
                use PropertyValue::Scalar as $kind;
                $body
            }
            (content, target) => {
                return Err(CommandError::Rejected(format!(
                    "a {} has no {}",
                    match content {
                        GraphicContent::Shape(_) => "shape",
                        GraphicContent::Text(_) => "run of text",
                    },
                    target.label().to_lowercase()
                )))
            }
        }
    }};
}

/// Borrows the property a [`GraphicProperty`] names, or `None` when this
/// graphic does not have one.
///
/// The read side of the macro above, and the same [`PropertyRef`] the clip
/// properties are read through — so the animation sheet, the curve editor and
/// the inspector draw a graphic's keyframes with the code they already had.
pub fn graphic_property_ref<'a>(
    graphic: &'a Graphic,
    target: GraphicProperty,
) -> Option<PropertyRef<'a>> {
    Some(match (&graphic.content, target) {
        (GraphicContent::Shape(s), GraphicProperty::Size) => PropertyRef::Point(&s.size),
        (GraphicContent::Shape(s), GraphicProperty::CornerRadius) => {
            PropertyRef::Scalar(&s.corner_radius)
        }
        (GraphicContent::Shape(s), GraphicProperty::InnerRadius) => {
            PropertyRef::Scalar(&s.inner_radius)
        }
        (GraphicContent::Shape(s), GraphicProperty::Fill) => PropertyRef::Color(&s.fill),
        (GraphicContent::Shape(s), GraphicProperty::Stroke) => PropertyRef::Color(&s.stroke),
        (GraphicContent::Shape(s), GraphicProperty::StrokeWidth) => {
            PropertyRef::Scalar(&s.stroke_width)
        }
        (GraphicContent::Text(t), GraphicProperty::FontSize) => PropertyRef::Scalar(&t.size),
        (GraphicContent::Text(t), GraphicProperty::Tracking) => {
            PropertyRef::Scalar(&t.tracking)
        }
        (GraphicContent::Text(t), GraphicProperty::LineHeight) => {
            PropertyRef::Scalar(&t.line_height)
        }
        (GraphicContent::Text(t), GraphicProperty::Fill) => PropertyRef::Color(&t.fill),
        (GraphicContent::Text(t), GraphicProperty::Stroke) => PropertyRef::Color(&t.stroke),
        (GraphicContent::Text(t), GraphicProperty::StrokeWidth) => {
            PropertyRef::Scalar(&t.stroke_width)
        }
        _ => return None,
    })
}

/// Every property of a graphic that can be keyframed, in the order the
/// inspector lists them.
pub fn animatable_graphic_properties(graphic: &Graphic) -> Vec<GraphicProperty> {
    match &graphic.content {
        GraphicContent::Shape(_) => vec![
            GraphicProperty::Size,
            GraphicProperty::CornerRadius,
            GraphicProperty::InnerRadius,
            GraphicProperty::Fill,
            GraphicProperty::Stroke,
            GraphicProperty::StrokeWidth,
        ],
        GraphicContent::Text(_) => vec![
            GraphicProperty::FontSize,
            GraphicProperty::Tracking,
            GraphicProperty::LineHeight,
            GraphicProperty::Fill,
            GraphicProperty::Stroke,
            GraphicProperty::StrokeWidth,
        ],
    }
}

// ---- creating and removing ----------------------------------------------

/// Adds a graphic to the project.
#[derive(Debug)]
pub struct AddGraphic {
    name: String,
    content: GraphicContent,
    /// Allocated on the first apply and reused by redo, so a redone graphic is
    /// the same graphic and the clip that drew it still draws it.
    id: Option<GraphicId>,
}

impl AddGraphic {
    pub fn new(name: impl Into<String>, content: GraphicContent) -> Self {
        AddGraphic { name: name.into(), content, id: None }
    }

    /// A rectangle, which is what the shape tool makes before anything is
    /// dialled in.
    pub fn shape(name: impl Into<String>, shape: Shape) -> Self {
        AddGraphic::new(name, GraphicContent::Shape(shape))
    }

    pub fn text(name: impl Into<String>, text: Text) -> Self {
        AddGraphic::new(name, GraphicContent::Text(text))
    }

    /// The graphic's ID, once the command has been applied.
    pub fn graphic_id(&self) -> Option<GraphicId> {
        self.id
    }
}

impl Command for AddGraphic {
    fn name(&self) -> &str {
        "New Graphic"
    }

    fn apply(&mut self, project: &mut Project) -> Result<(), CommandError> {
        match self.id {
            Some(id) => project.graphics.push(Graphic {
                id,
                name: self.name.clone(),
                content: self.content.clone(),
            }),
            None => {
                self.id = Some(project.add_graphic(self.name.clone(), self.content.clone()))
            }
        }
        Ok(())
    }

    fn undo(&mut self, project: &mut Project) -> Result<(), CommandError> {
        let id = self.id.ok_or_else(|| CommandError::Rejected("never applied".into()))?;
        // The graphic as it now stands, not as it was created: an undo that
        // reached back past later edits would lose them on redo.
        if let Some(current) = project.graphic(id) {
            self.content = current.content.clone();
            self.name = current.name.clone();
        }
        project.remove_graphic(id).map(|_| ()).map_err(Into::into)
    }

    fn as_any(&self) -> &dyn Any {
        self
    }
}

/// Removes a graphic, refusing while anything still draws it.
#[derive(Debug)]
pub struct RemoveGraphic {
    id: GraphicId,
    /// Where it was and what it held, so undo puts it back in place rather than
    /// at the end of the list.
    removed: Option<(usize, Graphic)>,
}

impl RemoveGraphic {
    pub fn new(id: GraphicId) -> Self {
        RemoveGraphic { id, removed: None }
    }
}

impl Command for RemoveGraphic {
    fn name(&self) -> &str {
        "Delete Graphic"
    }

    fn apply(&mut self, project: &mut Project) -> Result<(), CommandError> {
        let index = project
            .graphics
            .iter()
            .position(|g| g.id == self.id)
            .ok_or(CommandError::GraphicNotFound(self.id))?;
        let graphic = project.remove_graphic(self.id)?;
        self.removed = Some((index, graphic));
        Ok(())
    }

    fn undo(&mut self, project: &mut Project) -> Result<(), CommandError> {
        let (index, graphic) = self
            .removed
            .take()
            .ok_or_else(|| CommandError::Rejected("nothing was removed".into()))?;
        let at = index.min(project.graphics.len());
        project.graphics.insert(at, graphic);
        Ok(())
    }

    fn as_any(&self) -> &dyn Any {
        self
    }
}

/// Copies a graphic into a new one that nothing yet draws.
#[derive(Debug)]
pub struct DuplicateGraphic {
    source: GraphicId,
    copy: Option<GraphicId>,
}

impl DuplicateGraphic {
    pub fn new(source: GraphicId) -> Self {
        DuplicateGraphic { source, copy: None }
    }

    pub fn graphic_id(&self) -> Option<GraphicId> {
        self.copy
    }
}

impl Command for DuplicateGraphic {
    fn name(&self) -> &str {
        "Duplicate Graphic"
    }

    fn apply(&mut self, project: &mut Project) -> Result<(), CommandError> {
        let source =
            project.graphic(self.source).ok_or(CommandError::GraphicNotFound(self.source))?;
        let (name, content) = (format!("{} copy", source.name), source.content.clone());
        match self.copy {
            Some(id) => project.graphics.push(Graphic { id, name, content }),
            None => self.copy = Some(project.add_graphic(name, content)),
        }
        Ok(())
    }

    fn undo(&mut self, project: &mut Project) -> Result<(), CommandError> {
        let id = self.copy.ok_or_else(|| CommandError::Rejected("never applied".into()))?;
        project.remove_graphic(id).map(|_| ()).map_err(Into::into)
    }

    fn as_any(&self) -> &dyn Any {
        self
    }
}

/// Renames a graphic.
#[derive(Debug)]
pub struct RenameGraphic {
    id: GraphicId,
    name: String,
    previous: Option<String>,
}

impl RenameGraphic {
    pub fn new(id: GraphicId, name: impl Into<String>) -> Self {
        RenameGraphic { id, name: name.into(), previous: None }
    }
}

impl Command for RenameGraphic {
    fn name(&self) -> &str {
        "Rename Graphic"
    }

    fn apply(&mut self, project: &mut Project) -> Result<(), CommandError> {
        let graphic = graphic_mut(project, self.id)?;
        let previous = std::mem::replace(&mut graphic.name, self.name.clone());
        self.previous.get_or_insert(previous);
        Ok(())
    }

    fn undo(&mut self, project: &mut Project) -> Result<(), CommandError> {
        let previous = self
            .previous
            .clone()
            .ok_or_else(|| CommandError::Rejected("never applied".into()))?;
        graphic_mut(project, self.id)?.name = previous;
        Ok(())
    }

    fn merge(&mut self, next: &dyn Command) -> bool {
        match next.as_any().downcast_ref::<RenameGraphic>() {
            Some(other) if other.id == self.id => {
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

// ---- what a graphic draws ------------------------------------------------

/// Sets an animatable parameter's static value, leaving any keyframes alone.
///
/// Merges with later sets of the same parameter, so dragging a slider is one
/// undo step.
#[derive(Debug)]
pub struct SetGraphicProperty {
    id: GraphicId,
    target: GraphicProperty,
    value: PropertyValue,
    previous: Option<PropertyValue>,
    label: String,
}

impl SetGraphicProperty {
    pub fn new(id: GraphicId, target: GraphicProperty, value: PropertyValue) -> Self {
        let label = format!("Set {}", target.label());
        SetGraphicProperty { id, target, value, previous: None, label }
    }
}

impl Command for SetGraphicProperty {
    fn name(&self) -> &str {
        &self.label
    }

    fn apply(&mut self, project: &mut Project) -> Result<(), CommandError> {
        let (target, value) = (self.target, self.value);
        let graphic = graphic_mut(project, self.id)?;
        let previous = with_graphic_property!(graphic, target, |p, Variant| {
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
        let target = self.target;
        let graphic = graphic_mut(project, self.id)?;
        with_graphic_property!(graphic, target, |p, Variant| {
            if let Variant(v) = previous {
                p.value = v;
            }
        });
        Ok(())
    }

    fn merge(&mut self, next: &dyn Command) -> bool {
        match next.as_any().downcast_ref::<SetGraphicProperty>() {
            Some(other) if other.id == self.id && other.target == self.target => {
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

/// Changes what a run of text says.
///
/// Merges, so typing a sentence is one undo step rather than one per keystroke
/// — the same rule a slider drag follows, for the same reason.
#[derive(Debug)]
pub struct SetGraphicText {
    id: GraphicId,
    text: String,
    previous: Option<String>,
}

impl SetGraphicText {
    pub fn new(id: GraphicId, text: impl Into<String>) -> Self {
        SetGraphicText { id, text: text.into(), previous: None }
    }
}

impl Command for SetGraphicText {
    fn name(&self) -> &str {
        "Edit Text"
    }

    fn apply(&mut self, project: &mut Project) -> Result<(), CommandError> {
        let graphic = graphic_mut(project, self.id)?;
        let text = graphic
            .as_text_mut()
            .ok_or_else(|| CommandError::Rejected("that graphic is not text".into()))?;
        let previous = std::mem::replace(&mut text.text, self.text.clone());
        self.previous.get_or_insert(previous);
        Ok(())
    }

    fn undo(&mut self, project: &mut Project) -> Result<(), CommandError> {
        let previous = self
            .previous
            .clone()
            .ok_or_else(|| CommandError::Rejected("never applied".into()))?;
        let graphic = graphic_mut(project, self.id)?;
        let text = graphic
            .as_text_mut()
            .ok_or_else(|| CommandError::Rejected("that graphic is not text".into()))?;
        text.text = previous;
        Ok(())
    }

    fn merge(&mut self, next: &dyn Command) -> bool {
        match next.as_any().downcast_ref::<SetGraphicText>() {
            Some(other) if other.id == self.id => {
                self.text = other.text.clone();
                true
            }
            _ => false,
        }
    }

    fn as_any(&self) -> &dyn Any {
        self
    }
}

/// One of a graphic's choices: the settings that are not numbers.
///
/// Each of these has no halfway point — a font is not 40% of the way to another
/// font — so none of them is animatable, and none of them belongs in
/// [`GraphicProperty`].
#[derive(Debug, Clone, PartialEq)]
pub enum GraphicOption {
    /// Which outline a shape draws. Carries its own point count, so switching
    /// from a hexagon to a star does not have to guess one.
    Kind(ShapeKind),
    Font(FontSpec),
    Align(TextAlign),
    /// The width lines wrap at, or `None` to break only where the text says to.
    WrapWidth(Option<f64>),
}

impl GraphicOption {
    fn label(&self) -> &'static str {
        match self {
            GraphicOption::Kind(_) => "Set Shape",
            GraphicOption::Font(_) => "Set Font",
            GraphicOption::Align(_) => "Set Alignment",
            GraphicOption::WrapWidth(_) => "Set Wrapping",
        }
    }

    /// Reads whichever of these a graphic currently holds.
    fn read(&self, graphic: &Graphic) -> Result<GraphicOption, CommandError> {
        match (self, &graphic.content) {
            (GraphicOption::Kind(_), GraphicContent::Shape(s)) => {
                Ok(GraphicOption::Kind(s.kind))
            }
            (GraphicOption::Font(_), GraphicContent::Text(t)) => {
                Ok(GraphicOption::Font(t.font.clone()))
            }
            (GraphicOption::Align(_), GraphicContent::Text(t)) => {
                Ok(GraphicOption::Align(t.align))
            }
            (GraphicOption::WrapWidth(_), GraphicContent::Text(t)) => {
                Ok(GraphicOption::WrapWidth(t.wrap_width))
            }
            _ => Err(CommandError::Rejected(format!(
                "{} does not apply to this graphic",
                self.label()
            ))),
        }
    }

    fn write(&self, graphic: &mut Graphic) -> Result<(), CommandError> {
        match (self, &mut graphic.content) {
            (GraphicOption::Kind(kind), GraphicContent::Shape(s)) => s.kind = *kind,
            (GraphicOption::Font(font), GraphicContent::Text(t)) => t.font = font.clone(),
            (GraphicOption::Align(align), GraphicContent::Text(t)) => t.align = *align,
            (GraphicOption::WrapWidth(width), GraphicContent::Text(t)) => {
                // A box narrower than nothing is not a box. Clamped rather than
                // refused: it arrives from a drag, and a drag that went too far
                // should stop rather than raise an error.
                t.wrap_width = width.map(|w| w.max(1.0));
            }
            _ => {
                return Err(CommandError::Rejected(format!(
                    "{} does not apply to this graphic",
                    self.label()
                )))
            }
        }
        Ok(())
    }

    /// Whether two of these are the same setting, whatever value they carry —
    /// which is what makes a drag through the wrap widths one undo step.
    fn same_setting(&self, other: &GraphicOption) -> bool {
        std::mem::discriminant(self) == std::mem::discriminant(other)
    }
}

/// Sets one of a graphic's non-animatable choices.
#[derive(Debug)]
pub struct SetGraphicOption {
    id: GraphicId,
    option: GraphicOption,
    previous: Option<GraphicOption>,
    label: String,
}

impl SetGraphicOption {
    pub fn new(id: GraphicId, option: GraphicOption) -> Self {
        let label = option.label().to_string();
        SetGraphicOption { id, option, previous: None, label }
    }
}

impl Command for SetGraphicOption {
    fn name(&self) -> &str {
        &self.label
    }

    fn apply(&mut self, project: &mut Project) -> Result<(), CommandError> {
        let option = self.option.clone();
        let graphic = graphic_mut(project, self.id)?;
        let previous = option.read(graphic)?;
        option.write(graphic)?;
        self.previous.get_or_insert(previous);
        Ok(())
    }

    fn undo(&mut self, project: &mut Project) -> Result<(), CommandError> {
        let previous = self
            .previous
            .clone()
            .ok_or_else(|| CommandError::Rejected("never applied".into()))?;
        let graphic = graphic_mut(project, self.id)?;
        previous.write(graphic)
    }

    fn merge(&mut self, next: &dyn Command) -> bool {
        match next.as_any().downcast_ref::<SetGraphicOption>() {
            Some(other) if other.id == self.id && other.option.same_setting(&self.option) => {
                self.option = other.option.clone();
                true
            }
            _ => false,
        }
    }

    fn as_any(&self) -> &dyn Any {
        self
    }
}

// ---- keyframes -----------------------------------------------------------

/// Adds, deletes, retimes, eases, pastes or clears keyframes on a graphic.
///
/// The graphic-side twin of [`crate::EditKeyframes`]: the same six edits, the
/// same absolute statement of where the keyframes end up, and the same undo
/// that restores the property whole rather than replaying an inverse.
///
/// **Times are in the graphic's own domain**, not the sequence's and not the
/// clip's. A graphic has a timeline of its own that a clip is a window onto, so
/// a keyframe at two seconds is two seconds into the *graphic*, wherever the
/// clip drawing it happens to sit — which is what keeps a build-in intact when
/// the clip is moved, and what makes it possible for two clips to show the same
/// build.
#[derive(Debug)]
pub struct EditGraphicKeyframes {
    id: GraphicId,
    edits: Vec<(GraphicProperty, KeyframeEdit)>,
    before: Option<Vec<PropertyState>>,
    label: String,
}

impl EditGraphicKeyframes {
    pub fn new(id: GraphicId, edits: Vec<(GraphicProperty, KeyframeEdit)>) -> Self {
        let label = edits
            .first()
            .map(|(_, e)| e.label().to_string())
            .unwrap_or_else(|| "Keyframe".to_string());
        EditGraphicKeyframes { id, edits, before: None, label }
    }

    /// The common case: one keyframe on one parameter, at the instant the
    /// graphic is being looked at.
    pub fn set(
        id: GraphicId,
        target: GraphicProperty,
        at: Ticks,
        value: PropertyValue,
        interpolation: ve_core::Interpolation,
    ) -> Self {
        EditGraphicKeyframes::new(
            id,
            vec![(target, KeyframeEdit::Set { time: at, value, interpolation })],
        )
    }
}

impl Command for EditGraphicKeyframes {
    fn name(&self) -> &str {
        &self.label
    }

    fn apply(&mut self, project: &mut Project) -> Result<(), CommandError> {
        let edits = self.edits.clone();
        let graphic = graphic_mut(project, self.id)?;

        // Captured before anything changes, so a failure part way through the
        // list leaves the whole command undoable — and so that undo restores
        // the list rather than replaying an inverse that a collapsed retime
        // would get wrong.
        let mut before = Vec::with_capacity(edits.len());
        for (target, _) in &edits {
            let state = crate::graphic_commands::graphic_property_ref(graphic, *target)
                .ok_or_else(|| {
                    CommandError::Rejected(format!(
                        "this graphic has no {}",
                        target.label().to_lowercase()
                    ))
                })?
                .capture();
            before.push(state);
        }

        for (target, edit) in &edits {
            let edit = edit.clone();
            let label = target.label();
            // Reborrowed: the macro binds its first argument by value, so
            // handing it the outer `&mut` would move it on the first pass.
            with_graphic_property!(&mut *graphic, *target, |p, Variant| {
                crate::keyframe_commands::edit_property(p, &edit, label, |value| match value {
                    Variant(v) => Some(v),
                    _ => None,
                })
            })?;
        }

        self.before.get_or_insert(before);
        Ok(())
    }

    fn undo(&mut self, project: &mut Project) -> Result<(), CommandError> {
        let before = self
            .before
            .clone()
            .ok_or_else(|| CommandError::Rejected("never applied".into()))?;
        let targets: Vec<GraphicProperty> = self.edits.iter().map(|(t, _)| *t).collect();
        let graphic = graphic_mut(project, self.id)?;
        for (target, state) in targets.iter().zip(before) {
            with_graphic_property!(&mut *graphic, *target, |p, Variant| {
                crate::keyframe_commands::restore_property(p, &state, |value| match value {
                    Variant(v) => Some(v),
                    _ => None,
                });
            });
        }
        Ok(())
    }

    fn merge(&mut self, next: &dyn Command) -> bool {
        match next.as_any().downcast_ref::<EditGraphicKeyframes>() {
            Some(other)
                if other.id == self.id
                    && other.edits.len() == self.edits.len()
                    && other
                        .edits
                        .iter()
                        .zip(&self.edits)
                        .all(|((a, edit), (b, mine))| a == b && mine.continues(edit)) =>
            {
                self.edits = other.edits.clone();
                true
            }
            _ => false,
        }
    }

    fn as_any(&self) -> &dyn Any {
        self
    }
}

/// Places an existing graphic on a track as a clip.
///
/// The graphic-side twin of [`crate::composition_clip`]. A graphic has no
/// length of its own — a rectangle is as long as you want it to be — so the
/// clip is given the default a still gets, and trimming it longer is an
/// ordinary trim rather than a special case.
pub fn graphic_clip(
    project: &mut Project,
    graphic: GraphicId,
    at: Ticks,
) -> Result<ve_core::Clip, CommandError> {
    let name =
        project.graphic(graphic).ok_or(CommandError::GraphicNotFound(graphic))?.name.clone();
    let id = project.new_clip_id();
    Ok(ve_core::Clip::new(
        id,
        ve_core::Source::Graphic(graphic),
        name,
        Ticks::ZERO,
        at,
        Ticks::from_seconds(Graphic::DEFAULT_DURATION_SECONDS),
    ))
}
