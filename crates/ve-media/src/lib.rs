//! Demuxing, decoding and frame caching, built on FFmpeg.
//!
//! # Why FFmpeg
//!
//! Container parsing and codec support are solved problems with a decade of
//! security hardening behind them. Reimplementing them would consume the whole
//! project and produce something worse. What the editor *does* own is
//! everything above that line: how decoding is scheduled, what is cached, how
//! obsolete work is cancelled, and how frames reach the GPU without being
//! copied more than once.
//!
//! # Structure
//!
//! * [`probe`] reads metadata without decoding.
//! * [`VideoDecoder`] and [`AudioDecoder`] wrap one open file each, and are
//!   single-threaded and stateful — they own a seek position.
//! * [`FrameCache`] holds decoded frames under a memory budget.
//! * [`DecodeService`] owns the decoder threads and turns the whole thing into
//!   a non-blocking request/response interface, which is what keeps decoding
//!   off the UI thread.

use std::path::{Path, PathBuf};
use std::sync::Once;

use ffmpeg_next as ffmpeg;

mod cache;
mod decoder;
mod frame;
mod probe;
mod service;

pub use cache::{CacheKey, CacheStats, FrameCache};
pub use decoder::{AudioDecoder, VideoDecoder};
pub use frame::{AudioBuffer, PixelFormat, VideoFrame};
pub use probe::probe;
pub use service::{
    budget, DecodeEvent, DecodeFailure, DecodeService, FrameReady, FrameRequest,
    RequestPriority,
};

#[derive(Debug, thiserror::Error)]
pub enum MediaError {
    #[error("could not open {path}: {source}")]
    Open {
        path: PathBuf,
        #[source]
        source: ffmpeg::Error,
    },
    #[error("{0} contains no video or audio this build can decode")]
    NoDecodableStreams(PathBuf),
    #[error("this file has no video stream")]
    NoVideoStream,
    #[error("this file has no audio stream")]
    NoAudioStream,
    #[error("the decoder worker has stopped")]
    WorkerGone,
    #[error(transparent)]
    Ffmpeg(#[from] ffmpeg::Error),
}

impl MediaError {
    pub(crate) fn open(path: impl AsRef<Path>, source: ffmpeg::Error) -> Self {
        MediaError::Open { path: path.as_ref().to_path_buf(), source }
    }
}

static FFMPEG_INIT: Once = Once::new();
static mut FFMPEG_INIT_RESULT: Result<(), ffmpeg::Error> = Ok(());

/// Initialises FFmpeg exactly once per process.
///
/// `ffmpeg::init` registers global state and is not safe to call concurrently,
/// so every entry point into this crate funnels through here.
pub(crate) fn ffmpeg_init() -> Result<(), MediaError> {
    FFMPEG_INIT.call_once(|| {
        let result = ffmpeg::init();
        if let Err(ref e) = result {
            log::error!("FFmpeg initialisation failed: {e}");
        } else {
            // FFmpeg's own logging is noisy at the default level and duplicates
            // errors this crate already reports.
            ffmpeg::util::log::set_level(ffmpeg::util::log::Level::Error);
        }
        // SAFETY: written exactly once, inside `call_once`, before any reader
        // can observe it; `call_once` provides the happens-before edge.
        unsafe {
            FFMPEG_INIT_RESULT = result;
        }
    });
    // SAFETY: `call_once` has returned, so the write above is complete and
    // visible, and nothing ever writes again.
    unsafe {
        #[allow(static_mut_refs)]
        match &FFMPEG_INIT_RESULT {
            Ok(()) => Ok(()),
            Err(e) => Err(MediaError::Ffmpeg(*e)),
        }
    }
}
