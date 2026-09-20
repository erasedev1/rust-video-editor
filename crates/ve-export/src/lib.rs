//! Rendering a finished file: composite, read back, encode, mux.
//!
//! # The editor, run with nobody watching
//!
//! Nothing here is a second implementation of anything. An exported frame is
//! resolved by the same `evaluate` the preview calls, composited by the same
//! [`compose::FrameComposer`] the preview draws with, and its sound comes from
//! the same [`ve_engine::AudioRenderer`] the device is fed by. What is different
//! is only what the editor is allowed to do about time:
//!
//! | | Preview | Export |
//! |---|---|---|
//! | A frame that is not decoded yet | is dropped | is waited for |
//! | A frame that was decoded before | may be a cache hit | is decoded in order |
//! | The clock | drives the picture | is derived from the frame index |
//!
//! That is the whole of it. Playback is a real-time system with a deadline it
//! must not miss; an export is a batch job with an answer it must not get
//! wrong.
//!
//! # Structure
//!
//! * [`settings`] — what to write, where, and how. Plain data, no codecs.
//! * [`compose`] — one instant to a picture on the GPU, shared with the preview.
//! * [`sources`] — blocking decoders, one per asset.
//! * [`writer`] — the FFmpeg half: encoders, a muxer, and nothing else.
//! * [`job`] — the loop, and the thread the editor runs it on.

use std::path::PathBuf;

pub mod compose;
pub mod job;
pub mod settings;
pub mod sources;
pub mod writer;

pub use compose::FrameComposer;
pub use job::{run, Cancel, ExportEvent, ExportJob, ExportReport, Progress};
pub use settings::{
    AudioCodec, AudioSettings, Container, ExportRange, ExportSettings, Quality, VideoCodec,
};
pub use sources::SourceFrames;
pub use writer::MediaWriter;

#[derive(Debug, thiserror::Error)]
pub enum ExportError {
    /// Something about the request itself cannot work, found before any frame
    /// was rendered.
    #[error("{0}")]
    Settings(String),
    #[error("this build of FFmpeg cannot encode {0}")]
    NoEncoder(String),
    #[error("could not create {path}: {source}")]
    Open {
        path: PathBuf,
        #[source]
        source: ffmpeg_next::Error,
    },
    #[error("{path}: {source}")]
    Io {
        path: PathBuf,
        #[source]
        source: std::io::Error,
    },
    #[error("encoding failed: {0}")]
    Encoder(String),
    #[error("rendering failed: {0}")]
    Render(String),
    #[error("this project has no sequence to export")]
    NoSequence,
    /// Not really a failure — the user asked for it — but it travels the same
    /// path as one, and the caller has to be able to tell them apart.
    #[error("the export was cancelled")]
    Cancelled,
    #[error(transparent)]
    Ffmpeg(#[from] ffmpeg_next::Error),
    #[error(transparent)]
    Gpu(#[from] ve_render::RenderError),
}

impl ExportError {
    /// Whether this is the user having stopped the export rather than a fault.
    pub fn is_cancellation(&self) -> bool {
        matches!(self, ExportError::Cancelled)
    }
}
