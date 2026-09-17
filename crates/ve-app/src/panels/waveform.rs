//! Drawing an audio clip's waveform into the timeline.
//!
//! # One mesh, not one shape per column
//!
//! A waveform is a column per pixel, and a clip can be two thousand pixels
//! wide. Two thousand rounded rectangles would cost two thousand shapes to
//! tessellate and sort on every repaint, for something that is a single flat
//! colour. Both traces are therefore built as one [`egui::Mesh`] each — four
//! vertices a column, one draw — which is what keeps a timeline full of audio
//! as cheap to paint as a timeline full of video.
//!
//! # What is drawn
//!
//! The peak envelope, and the RMS level inside it. Two traces rather than one
//! because they answer different questions: the envelope says where the
//! transients are, the body says where the audio is actually loud. Level is
//! scaled by the clip's evaluated volume, so an animated fade is visible as a
//! fade in the picture of the sound rather than only in the inspector.
//!
//! Nothing is drawn where analysis has not reached yet. That is deliberate: a
//! flat line would say "silence", and the honest answer while a long file is
//! still being read is "not known yet".

use egui::{Color32, Mesh, Pos2, Rect, Stroke};
use ve_core::{Clip, MediaAsset};
use ve_media::{Peak, WaveformService};
use ve_time::{Ticks, TimeRange};

use crate::theme;

/// Below this height there is no room for a waveform that means anything, and
/// a two-pixel smear reads as noise rather than as audio.
const MIN_HEIGHT: f32 = 14.0;

/// Fraction of the half-height the loudest possible sample reaches, so a
/// full-scale peak does not touch the clip's edge.
const HEADROOM: f32 = 0.92;

/// Where an audio clip's waveform goes, and what part of the file it shows.
pub struct Area {
    /// The part of the clip body to draw into — already clamped to the lane, so
    /// a clip stretching off screen costs only what is on screen.
    pub rect: Rect,
    /// The source-media range `rect` spans.
    pub source: TimeRange,
    /// The same span in timeline time, for evaluating animated properties.
    pub timeline: TimeRange,
}

/// Draws a clip's waveform, asking for one if it has not been analysed.
///
/// Requesting from the paint loop is the same bargain the preview makes with
/// decoded frames: a hit draws immediately, a miss asks and draws what exists.
/// It costs one hash lookup per visible audio clip per repaint.
pub fn draw(
    painter: &egui::Painter,
    area: &Area,
    clip: &Clip,
    asset: &MediaAsset,
    waveforms: &WaveformService,
) {
    if area.rect.height() < MIN_HEIGHT || area.rect.width() < 2.0 {
        return;
    }
    let Some(audio) = asset.info.audio.as_ref() else { return };

    let state = waveforms.state(asset.id);
    if state.is_missing() {
        if !asset.offline {
            waveforms.request(asset.id, &asset.path, audio.sample_rate);
        }
        return;
    }
    // Queued, or failed. A failure leaves the clip drawn and the waveform
    // absent, with the reason in the log: a message painted across the timeline
    // is not somewhere the user can act on it, and a clip that cannot be
    // analysed can still be cut.
    if !state.has_peaks() {
        return;
    }

    let columns = area.rect.width().round().max(1.0) as usize;
    let Some(envelope) = waveforms.envelope(asset.id, area.source, columns) else { return };

    let axis = Axis { centre: area.rect.center().y, half: area.rect.height() * 0.5 * HEADROOM };
    let step = area.rect.width() / columns as f32;

    let mut peaks = Mesh::default();
    let mut bodies = Mesh::default();
    // Left and right edge of what was actually drawn, which is only the
    // analysed part of the clip.
    let mut drawn: Option<(f32, f32)> = None;

    for (index, column) in envelope.iter().enumerate() {
        let Some(peak) = column else { continue };
        let x = area.rect.left() + index as f32 * step;
        // A hairline of overlap: adjacent columns must not leave seams when
        // `step` is not a whole pixel.
        let right = (x + step).max(x + 1.0);

        let gain = gain_at(clip, area, index, columns);
        let peak = scaled(*peak, gain);

        add_column(&mut peaks, &axis, x, right, peak.max, peak.min, theme::WAVEFORM_PEAK);
        add_column(&mut bodies, &axis, x, right, peak.rms, -peak.rms, theme::WAVEFORM_BODY);

        drawn = Some(match drawn {
            Some((from, _)) => (from, right),
            None => (x, right),
        });
    }

    // The axis first, so the traces sit on top of it rather than being cut by
    // it, and only across what has actually been analysed.
    if let Some((from, to)) = drawn {
        painter.line_segment(
            [Pos2::new(from, axis.centre), Pos2::new(to, axis.centre)],
            Stroke::new(1.0, theme::WAVEFORM_AXIS),
        );
    }
    if !peaks.is_empty() {
        painter.add(egui::Shape::mesh(peaks));
    }
    if !bodies.is_empty() {
        painter.add(egui::Shape::mesh(bodies));
    }
}

/// Where the zero line sits and how far full scale reaches from it.
struct Axis {
    centre: f32,
    half: f32,
}

/// Adds one column, `top` and `bottom` being sample values in `[-1, 1]`.
fn add_column(
    mesh: &mut Mesh,
    axis: &Axis,
    left: f32,
    right: f32,
    top: f32,
    bottom: f32,
    colour: Color32,
) {
    let y0 = axis.centre - top.clamp(-1.0, 1.0) * axis.half;
    let y1 = axis.centre - bottom.clamp(-1.0, 1.0) * axis.half;
    // Always at least a pixel: audio that is quiet but present must not vanish
    // into the axis, or a clip of room tone looks like a clip of nothing.
    let (y0, y1) = if (y1 - y0).abs() < 1.0 {
        (axis.centre - 0.5, axis.centre + 0.5)
    } else {
        (y0.min(y1), y0.max(y1))
    };
    mesh.add_colored_rect(
        Rect::from_min_max(Pos2::new(left, y0), Pos2::new(right, y1)),
        colour,
    );
}

fn scaled(peak: Peak, gain: f32) -> Peak {
    Peak { min: peak.min * gain, max: peak.max * gain, rms: peak.rms * gain }
}

/// The clip's audible level at the time a column covers: its volume, its
/// automation and its fades, which is exactly what the mixer will apply.
///
/// Constant volume and no fade — the overwhelming case — is evaluated once per
/// column and costs a property lookup; it is not worth a special case, because
/// the same call is what makes a fade draw as one.
fn gain_at(clip: &Clip, area: &Area, index: usize, columns: usize) -> f32 {
    let fraction = (index as f64 + 0.5) / columns as f64;
    let at = area.timeline.start
        + Ticks::from_secs_f64(area.timeline.duration.as_secs_f64() * fraction);
    let local = clip.local_time_at(at);
    clip.audio.evaluate(local, clip.duration).0.clamp(0.0, 4.0) as f32
}
