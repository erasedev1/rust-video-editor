//! The editor's panels.
//!
//! Each takes the editor state and pushes [`crate::Action`]s rather than
//! mutating anything, so the drawing pass stays free of edit logic and every
//! operation goes through the one undoable path.

pub mod animation;
pub mod curves;
pub mod export;
pub mod inspector;
pub mod overlay;
pub mod preview_panel;
pub mod project_panel;
pub mod scopes;
pub mod timeline;
pub mod waveform;
