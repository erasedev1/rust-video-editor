//! Keyboard shortcuts.
//!
//! Every binding builds the same [`Action`] the menu does, so the two can never
//! diverge, and the whole table can be read in one place rather than being
//! scattered through the interface.

use egui::{Key, Modifiers};

use crate::actions::Action;
use crate::state::TimelineTool;

/// A shortcut, its action, and how it is described in the menu.
pub struct Binding {
    pub label: &'static str,
    pub keys: &'static str,
}

/// Collects the actions the current frame's key presses call for.
pub fn collect(ctx: &egui::Context, timeline_width: f32) -> Vec<Action> {
    let mut actions = Vec::new();

    ctx.input_mut(|i| {
        // A text field has the keyboard while it is focused; transport keys
        // must not steal a space the user is typing.
        if i.focused {
            // `focused` is window focus, not widget focus; widget focus is
            // checked by the caller via `wants_keyboard_input`.
        }

        let plain = Modifiers::NONE;
        let ctrl = Modifiers::COMMAND;
        let shift = Modifiers::SHIFT;

        if i.consume_key(plain, Key::Space) {
            actions.push(Action::TogglePlayback);
        }
        if i.consume_key(plain, Key::ArrowLeft) {
            actions.push(Action::StepFrames(-1));
        }
        if i.consume_key(plain, Key::ArrowRight) {
            actions.push(Action::StepFrames(1));
        }
        if i.consume_key(shift, Key::ArrowLeft) {
            actions.push(Action::StepFrames(-10));
        }
        if i.consume_key(shift, Key::ArrowRight) {
            actions.push(Action::StepFrames(10));
        }
        if i.consume_key(plain, Key::Home) {
            actions.push(Action::GoToStart);
        }
        if i.consume_key(plain, Key::End) {
            actions.push(Action::GoToEnd);
        }

        if i.consume_key(ctrl, Key::Z) {
            actions.push(Action::Undo);
        }
        // Both conventions, because muscle memory differs by platform.
        if i.consume_key(ctrl | shift, Key::Z) || i.consume_key(ctrl, Key::Y) {
            actions.push(Action::Redo);
        }

        // Delete lifts, leaving the gap; shift-delete ripples it closed. The
        // same pairing as every other NLE.
        if i.consume_key(shift, Key::Delete) || i.consume_key(shift, Key::Backspace) {
            actions.push(Action::RippleDeleteSelected);
        } else if i.consume_key(plain, Key::Delete) || i.consume_key(plain, Key::Backspace) {
            actions.push(Action::DeleteSelected);
        }
        if i.consume_key(ctrl, Key::Backspace) {
            actions.push(Action::CloseGapAtPlayhead);
        }
        if i.consume_key(ctrl | shift, Key::F) {
            actions.push(Action::CrossfadeSelection(ve_core::FadeCurve::EqualPower));
        }
        if i.consume_key(ctrl, Key::C) {
            actions.push(Action::Copy);
        }
        if i.consume_key(ctrl, Key::X) {
            actions.push(Action::Cut);
        }
        if i.consume_key(ctrl, Key::V) {
            actions.push(Action::Paste);
        }
        if i.consume_key(ctrl, Key::A) {
            actions.push(Action::SelectAll);
        }
        if i.consume_key(ctrl, Key::K) {
            actions.push(Action::SplitAtPlayhead);
        }
        if i.consume_key(plain, Key::Escape) {
            actions.push(Action::ClearSelection);
        }
        if i.consume_key(plain, Key::S) {
            actions.push(Action::ToggleSnapping);
        }
        if i.consume_key(plain, Key::E) {
            actions.push(Action::ToggleSelectedEnabled);
        }
        if i.consume_key(plain, Key::M) {
            actions.push(Action::AddMarkerAtPlayhead);
        }
        if i.consume_key(plain, Key::A) {
            actions.push(Action::ToggleAnimationEditor);
        }
        // The timeline tools, on the keys every other editor puts them on.
        for (key, tool) in [
            (Key::V, TimelineTool::Select),
            (Key::N, TimelineTool::Roll),
            (Key::Y, TimelineTool::Slip),
            (Key::U, TimelineTool::Slide),
        ] {
            if i.consume_key(plain, key) {
                actions.push(Action::SetTool(tool));
            }
        }
        if i.consume_key(ctrl, Key::ArrowLeft) {
            actions.push(Action::GoToMarker(-1));
        }
        if i.consume_key(ctrl, Key::ArrowRight) {
            actions.push(Action::GoToMarker(1));
        }

        if i.consume_key(ctrl, Key::S) {
            actions.push(Action::SaveProject);
        }
        if i.consume_key(ctrl, Key::N) {
            actions.push(Action::NewProject);
        }

        if i.consume_key(plain, Key::Equals) || i.consume_key(plain, Key::Plus) {
            actions.push(Action::ZoomIn);
        }
        if i.consume_key(plain, Key::Minus) {
            actions.push(Action::ZoomOut);
        }
        if i.consume_key(shift, Key::Z) {
            actions.push(Action::ZoomToFit(timeline_width));
        }
        if i.consume_key(ctrl | shift, Key::P) {
            actions.push(Action::TogglePerformanceOverlay);
        }
    });

    actions
}

/// The table shown in Help, and used to label menu items.
pub const BINDINGS: &[Binding] = &[
    Binding { label: "Play / pause", keys: "Space" },
    Binding { label: "Step one frame", keys: "← / →" },
    Binding { label: "Step ten frames", keys: "Shift + ← / →" },
    Binding { label: "Go to start / end", keys: "Home / End" },
    Binding { label: "Undo", keys: "Ctrl + Z" },
    Binding { label: "Redo", keys: "Ctrl + Shift + Z" },
    Binding { label: "Split at playhead", keys: "Ctrl + K" },
    Binding { label: "Delete selected", keys: "Delete" },
    Binding { label: "Ripple delete", keys: "Shift + Delete" },
    Binding { label: "Close gap at playhead", keys: "Ctrl + Backspace" },
    Binding { label: "Crossfade two overlapping clips", keys: "Ctrl + Shift + F" },
    Binding { label: "Copy / cut / paste", keys: "Ctrl + C / X / V" },
    Binding { label: "Select all", keys: "Ctrl + A" },
    Binding { label: "Toggle clip enabled", keys: "E" },
    Binding { label: "Clear selection", keys: "Esc" },
    Binding { label: "Toggle snapping", keys: "S" },
    Binding { label: "Select / roll / slip / slide tool", keys: "V / N / Y / U" },
    Binding { label: "Add marker", keys: "M" },
    Binding { label: "Animation editor", keys: "A" },
    Binding { label: "Keyframe / delete keyframes", keys: "◆ button / Del" },
    Binding { label: "Copy / paste keyframes", keys: "Ctrl + C / V" },
    Binding { label: "Previous / next marker", keys: "Ctrl + ← / →" },
    Binding { label: "Zoom in / out", keys: "+ / −" },
    Binding { label: "Zoom to fit", keys: "Shift + Z" },
    Binding { label: "New project", keys: "Ctrl + N" },
    Binding { label: "Save project", keys: "Ctrl + S" },
    Binding { label: "Import media", keys: "Ctrl + I" },
    Binding { label: "Performance overlay", keys: "Ctrl + Shift + P" },
];
