//! Turning a sequence and a time into a flat list of what to draw and hear.
//!
//! This is the boundary between the edit model and the engine, and it is
//! deliberately **pure**: no decoding, no GPU, no clock. Given a
//! [`Sequence`] and an instant it answers "what is on screen, where, and how
//! loud", which makes every question about track ordering, muting, soloing and
//! animation answerable in a unit test rather than by looking at a preview.
//!
//! # Why a plan and not a "composition"
//!
//! The result is a [`RenderPlan`]: the flattened, ready-to-draw description of
//! one instant. It is deliberately not called a composition, because a
//! composition is a thing the *user* authors and saves — see
//! [`ve_core::RenderPlan`] — whereas this is derived, thrown away every frame,
//! and never persisted.

use ve_core::{AssetId, BlendMode, ClipId, Sequence, Size, TrackId, TrackKind, TransformState};
use ve_time::Ticks;

/// One clip contributing picture at a given instant.
#[derive(Debug, Clone, PartialEq)]
pub struct VisibleClip {
    pub clip: ClipId,
    pub track: TrackId,
    pub asset: AssetId,
    /// Where to read in the source media.
    pub source_time: Ticks,
    /// Every animated property resolved at this instant.
    pub transform: TransformState,
    /// How this clip combines with the layers beneath it.
    pub blend: BlendMode,
    /// Compositing order: 0 is the bottom layer.
    pub layer: usize,
}

/// One clip contributing sound at a given instant.
#[derive(Debug, Clone, PartialEq)]
pub struct AudibleClip {
    pub clip: ClipId,
    pub track: TrackId,
    pub asset: AssetId,
    pub source_time: Ticks,
    /// Linear gain, already resolved from the clip's animated volume.
    pub gain: f64,
    /// -1 hard left to +1 hard right.
    pub pan: f64,
}

/// Everything a single instant of the timeline resolves to: what to draw, in
/// what order, over what background, and what to mix.
#[derive(Debug, Clone, PartialEq)]
pub struct RenderPlan {
    pub time: Ticks,
    pub size: Size,
    pub background: ve_core::Rgba,
    /// Bottom layer first, which is the order the compositor draws in.
    pub video: Vec<VisibleClip>,
    pub audio: Vec<AudibleClip>,
}

impl RenderPlan {
    pub fn is_empty(&self) -> bool {
        self.video.is_empty() && self.audio.is_empty()
    }

    /// Assets this instant needs decoded, without duplicates.
    pub fn required_assets(&self) -> Vec<AssetId> {
        let mut assets: Vec<AssetId> = self
            .video
            .iter()
            .map(|c| c.asset)
            .chain(self.audio.iter().map(|c| c.asset))
            .collect();
        assets.sort_unstable();
        assets.dedup();
        assets
    }
}

/// Resolves a sequence at `at` into a flat plan.
///
/// Track order is layer order: index 0 is the bottom video layer, drawn first.
/// Clips that are disabled, on a muted track, or excluded by a solo are left
/// out entirely rather than being included at zero opacity, so the compositor
/// never spends a draw call on something invisible.
pub fn evaluate(sequence: &Sequence, at: Ticks) -> RenderPlan {
    // Solo is exclusive: the moment anything is soloed, everything else on that
    // side goes quiet. Video and audio solo independently, because soloing a
    // video track to inspect it should not silence the mix.
    let video_solo =
        sequence.tracks.iter().any(|t| t.kind == TrackKind::Video && t.solo && !t.muted);
    let audio_solo =
        sequence.tracks.iter().any(|t| t.kind == TrackKind::Audio && t.solo && !t.muted);

    let mut video = Vec::new();
    let mut audio = Vec::new();

    for track in &sequence.tracks {
        if track.muted {
            continue;
        }
        let soloing = match track.kind {
            TrackKind::Video => video_solo,
            TrackKind::Audio => audio_solo,
        };
        if soloing && !track.solo {
            continue;
        }

        let Some(clip) = track.clip_at(at) else { continue };
        if !clip.enabled {
            continue;
        }
        let Some(source_time) = clip.source_time_at(at) else { continue };
        // Properties are keyed on clip-local time, so moving a clip along the
        // timeline carries its animation with it.
        let local = clip.local_time_at(at);

        match track.kind {
            TrackKind::Video => {
                video.push(VisibleClip {
                    clip: clip.id,
                    track: track.id,
                    asset: clip.asset,
                    source_time,
                    transform: clip.transform.evaluate(local),
                    blend: clip.blend,
                    layer: video.len(),
                });
            }
            TrackKind::Audio => {
                let (gain, pan) = clip.audio.evaluate(local);
                audio.push(AudibleClip {
                    clip: clip.id,
                    track: track.id,
                    asset: clip.asset,
                    source_time,
                    gain,
                    pan,
                });
            }
        }
    }

    RenderPlan {
        time: at,
        size: sequence.settings.resolution,
        background: sequence.settings.background,
        video,
        audio,
    }
}

/// Resolves the instants a range of frames lands on.
///
/// Used by export and by prefetch, both of which want to know what is coming
/// rather than only what is current.
pub fn evaluate_frames(sequence: &Sequence, first_frame: i64, count: usize) -> Vec<RenderPlan> {
    let rate = sequence.rate();
    (0..count)
        .map(|i| evaluate(sequence, rate.frame_to_ticks(first_frame + i as i64)))
        .collect()
}
