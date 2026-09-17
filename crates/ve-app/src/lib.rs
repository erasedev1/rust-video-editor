//! Verge's editor shell.
//!
//! Exposed as a library as well as a binary so the whole editor can be driven
//! without a window: [`actions::dispatch`] is the single path every menu item,
//! shortcut and direct manipulation goes through, and the integration tests
//! drive exactly that.

pub mod actions;
pub mod app;
pub mod dialogs;
pub mod meter;
pub mod panels;
pub mod preview;
pub mod shortcuts;
pub mod state;
pub mod theme;
