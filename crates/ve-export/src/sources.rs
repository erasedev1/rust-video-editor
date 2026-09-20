//! Decoding for an export: every frame, however long it takes.
//!
//! The opposite of what playback wants, and so deliberately not the decode
//! service. Playback's rule is that a frame the user has already scrolled past
//! is worthless, so a new request cancels the old one and a frame that is not
//! ready is a dropped frame. An export has nowhere to be: the frame it is
//! asking for is the frame it is going to write, and waiting for it is the
//! whole job. So each asset gets an ordinary blocking decoder, driven forward
//! one frame at a time in the order the timeline asks for them — which is also
//! the order the decoder is fastest at.

use std::collections::HashMap;
use std::path::PathBuf;

use ve_core::{AssetId, Project};
use ve_engine::{Draw, LayerContent, PlanItem, RenderPlan, ResolvedLayer, ResolvedNode};
use ve_media::{CacheKey, VideoDecoder, VideoFrame};
use ve_time::Ticks;

/// Blocking decoders for the assets an export needs, opened on first use.
pub struct SourceFrames {
    paths: HashMap<AssetId, PathBuf>,
    /// One decoder per asset. An entry holding `None` is one that could not be
    /// opened, kept so the failure is not retried on every frame of a
    /// ten-minute render.
    decoders: HashMap<AssetId, Option<VideoDecoder>>,
    /// The frame each asset last delivered, so that two clips showing the same
    /// instant of the same file — a dissolve, a split screen — decode once.
    current: HashMap<AssetId, (CacheKey, VideoFrame)>,
    /// Items whose picture could not be decoded, which the report carries: a
    /// frame written without a layer that should have been in it is exactly the
    /// kind of thing that must not pass silently.
    missing: u64,
    problems: Vec<String>,
}

impl SourceFrames {
    /// Registers every asset in the project that has a video stream.
    ///
    /// **Always the original file, never a proxy.** A proxy exists so the
    /// editor can keep up with a hand on a mouse; a delivery has nowhere to be
    /// and every reason to be right. Rendering one from the quarter-size
    /// stand-in would hand back a soft file that the editor had never shown,
    /// and the discovery would be made by whoever was given it. So this reads
    /// `path` directly rather than going through `picture_source`, and
    /// `ve_core::ProjectSettings::use_proxies` is not consulted here at all.
    pub fn new(project: &Project) -> Self {
        let paths = project
            .assets
            .iter()
            .filter(|a| !a.offline && a.info.has_video())
            .map(|a| (a.id, a.path.clone()))
            .collect();
        SourceFrames {
            paths,
            decoders: HashMap::new(),
            current: HashMap::new(),
            missing: 0,
            problems: Vec::new(),
        }
    }

    /// How a decoded frame is identified, matching the key it was stored under.
    ///
    /// `None` for anything not decoded from media — a nested composition, or an
    /// asset this export never managed to open.
    pub fn key_for(&self, item: &PlanItem) -> Option<CacheKey> {
        let Draw::Media { asset, source_time } = item.draw else { return None };
        self.key(asset, source_time)
    }

    fn key(&self, asset: AssetId, source_time: Ticks) -> Option<CacheKey> {
        let decoder = self.decoders.get(&asset)?.as_ref()?;
        Some(CacheKey::new(
            asset,
            decoder.rate().ticks_to_frame(source_time),
            decoder.output_size().width,
        ))
    }

    /// Resolves every node of a plan, decoding whatever it needs.
    ///
    /// Parallel to [`RenderPlan::nodes`] — same length, same order — which is
    /// what the compositor expects. Blocks until each frame is in hand, unlike
    /// the playback engine's version of this, which reports what is not ready
    /// and carries on.
    pub fn resolve(&mut self, plan: &RenderPlan) -> Vec<ResolvedNode> {
        plan.nodes
            .iter()
            .map(|node| {
                let mut layers = Vec::with_capacity(node.items.len());
                for item in &node.items {
                    match item.draw {
                        Draw::Media { asset, source_time } => {
                            match self.frame(asset, source_time) {
                                Some(frame) => layers.push(ResolvedLayer {
                                    item: item.clone(),
                                    content: LayerContent::Frame(frame),
                                }),
                                None => self.missing += 1,
                            }
                        }
                        Draw::Nested { node: index, .. } => layers.push(ResolvedLayer {
                            item: item.clone(),
                            content: LayerContent::Nested(index),
                        }),
                    }
                }
                ResolvedNode { layers, pending: 0 }
            })
            .collect()
    }

    /// The frame of `asset` covering `source_time`, decoding it if need be.
    pub fn frame(&mut self, asset: AssetId, source_time: Ticks) -> Option<VideoFrame> {
        self.open(asset);
        let key = self.key(asset, source_time)?;
        if let Some((held, frame)) = self.current.get(&asset) {
            if *held == key {
                return Some(frame.clone());
            }
        }

        let decoder = self.decoders.get_mut(&asset)?.as_mut()?;
        match decoder.frame_at(source_time) {
            Ok(Some(frame)) => {
                self.current.insert(asset, (key, frame.clone()));
                Some(frame)
            }
            Ok(None) => None,
            Err(e) => {
                // One report per asset, not per frame: a file that has gone
                // missing would otherwise fill the report with the same line a
                // thousand times.
                let note = format!("asset {asset}: {e}");
                if !self.problems.contains(&note) {
                    self.problems.push(note);
                }
                None
            }
        }
    }

    /// Opens an asset's decoder if this is the first time it is asked for.
    fn open(&mut self, asset: AssetId) {
        if self.decoders.contains_key(&asset) {
            return;
        }
        let decoder = match self.paths.get(&asset) {
            Some(path) => match VideoDecoder::open(path) {
                Ok(decoder) => Some(decoder),
                Err(e) => {
                    self.problems.push(format!("could not open asset {asset}: {e}"));
                    None
                }
            },
            None => None,
        };
        self.decoders.insert(asset, decoder);
    }

    /// How many layers were left out of a picture because their frame never
    /// arrived.
    pub fn missing_frames(&self) -> u64 {
        self.missing
    }

    pub fn problems(&self) -> &[String] {
        &self.problems
    }
}
