//! Playback: the clock, composition evaluation, decode scheduling and audio.
//!
//! # Independent of the interface
//!
//! Nothing here draws, owns a window, or blocks on one. The UI calls
//! [`PlaybackEngine::update`] once per repaint and composites whatever came
//! back. That separation is what makes playback timing testable — see
//! [`clock::ManualTime`], which drives the transport from a fake clock so
//! timing behaviour is asserted rather than slept on.
//!
//! # Position comes from the wall clock
//!
//! Playback position is derived from elapsed real time, never accumulated one
//! frame at a time. Accumulating would tie playback speed to renderer speed, so
//! a slow frame would slow the audio down. Deriving means a slow frame drops a
//! picture and the sound keeps its pace.
//!
//! # Scrubbing beats read-ahead
//!
//! The frame under the playhead is always requested as interactive work, which
//! supersedes any decode still running for a position the user has left. Only
//! read-ahead is queued.

pub mod audio;
pub mod clock;
pub mod composition;
mod cpal_sink;
mod engine;
pub mod mixer;
pub mod ring;

pub use audio::{AudioError, AudioRenderer, AudioSink};
pub use clock::{ManualTime, PlaybackClock, SystemTime, TimeSource, TransportState};
pub use composition::{evaluate, evaluate_frames, AudibleClip, Composition, VisibleClip};
pub use cpal_sink::CpalSink;
pub use engine::{peek, EngineUpdate, PlaybackEngine, ResolvedLayer};
pub use mixer::{AudioMixer, MixSource, MixStats};
pub use ring::{AudioRing, Consumer, Producer};
