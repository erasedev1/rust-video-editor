//! OpenTimelineIO (`.otio`) timelines.
//!
//! OTIO is JSON, so unlike an EDL it carries its own rates, names its media by
//! URL, and says where gaps are. Verge writes the part of it every reader
//! understands — one `Track` per track, `Clip`s with an `ExternalReference`,
//! `Gap`s between them, and a `LinearTimeWarp` for constant speed — and reads
//! the same.
//!
//! Like the EDL reader, reading never touches a project: [`read`] produces
//! [`Placement`]s for the caller to number and insert. Transitions, nested
//! stacks and other effects are reported and skipped; nothing is invented.

use serde_json::{json, Value};
use ve_core::{AssetId, Project, Sequence, Source, Speed, TrackKind};
use ve_time::Ticks;

use crate::edl::{asset_origin, PlacedClip, Placement};

#[derive(Debug, thiserror::Error)]
pub enum OtioError {
    #[error("not valid JSON: {0}")]
    Json(#[from] serde_json::Error),
    #[error("not an OpenTimelineIO timeline")]
    NotATimeline,
}

/// A clip as the file states it, before any footage is found for it.
#[derive(Debug, Clone, PartialEq)]
pub struct OtioClip {
    /// Position among the file's clips, in file order; becomes `PlacedClip::event`.
    pub index: u32,
    pub name: String,
    pub track_kind: TrackKind,
    /// The media's URL or path, when the file gave one.
    pub target: Option<String>,
}

// ---- writing ----------------------------------------------------------------

fn time(frames: i64, rate: f64) -> Value {
    json!({ "OTIO_SCHEMA": "RationalTime.1", "rate": rate, "value": frames as f64 })
}

fn range(start: i64, duration: i64, rate: f64) -> Value {
    json!({
        "OTIO_SCHEMA": "TimeRange.1",
        "start_time": time(start, rate),
        "duration": time(duration, rate),
    })
}

/// Renders `sequence` as an OTIO timeline, with the reasons anything was left out.
pub fn write(project: &Project, sequence: &Sequence) -> (String, Vec<String>) {
    let rate = sequence.rate();
    let fps = rate.as_f64();
    let mut warnings = Vec::new();
    let mut tracks = Vec::new();

    for track in &sequence.tracks {
        let mut children = Vec::new();
        let mut cursor = 0i64;
        for clip in track.clips() {
            let Source::Asset(id) = clip.source else {
                warnings.push(format!("{}: not a file, left out", clip.name));
                continue;
            };
            let Some(asset) = project.asset(id).filter(|_| clip.enabled) else {
                if !clip.enabled {
                    warnings.push(format!("{}: disabled, left out", clip.name));
                }
                continue;
            };

            let start = rate.ticks_to_frame_round(clip.timeline_start);
            if start > cursor {
                children.push(json!({
                    "OTIO_SCHEMA": "Gap.1",
                    "name": "",
                    "source_range": range(0, start - cursor, fps),
                }));
            }
            let len = rate.ticks_to_frame_round(clip.duration).max(1);
            cursor = start + len;

            let origin = asset_origin(asset, rate);
            let src_len = rate.ticks_to_frame_round(clip.source_duration()).max(1);
            let mut effects = Vec::new();
            if !clip.speed.is_normal() {
                effects.push(json!({
                    "OTIO_SCHEMA": "LinearTimeWarp.1",
                    "name": "",
                    "effect_name": "LinearTimeWarp",
                    "time_scalar": clip.speed.as_f64(),
                }));
            }
            children.push(json!({
                "OTIO_SCHEMA": "Clip.2",
                "name": clip.name,
                "source_range": range(
                    origin + rate.ticks_to_frame_round(clip.source_in), src_len, fps),
                "effects": effects,
                "media_reference": {
                    "OTIO_SCHEMA": "ExternalReference.1",
                    "name": asset.name,
                    "target_url": asset.path.to_string_lossy(),
                    "available_range": range(
                        origin, rate.ticks_to_frame_round(asset.duration()), fps),
                },
            }));
        }
        tracks.push(json!({
            "OTIO_SCHEMA": "Track.1",
            "name": track.name,
            "kind": if track.kind == TrackKind::Video { "Video" } else { "Audio" },
            "children": children,
        }));
    }

    let timeline = json!({
        "OTIO_SCHEMA": "Timeline.1",
        "name": sequence.name,
        "global_start_time": time(rate.ticks_to_frame_round(sequence.settings.start_timecode), fps),
        "tracks": {
            "OTIO_SCHEMA": "Stack.1",
            "name": "tracks",
            "children": tracks,
        },
    });
    (serde_json::to_string_pretty(&timeline).expect("a Value always serialises"), warnings)
}

// ---- reading ----------------------------------------------------------------

/// A time in the file as exact ticks. NTSC rates are recognised so
/// 24000/1001 frames land where Verge's own rate puts them, not a rounding away.
fn to_ticks(value: f64, rate: f64) -> Ticks {
    if rate <= 0.0 {
        return Ticks::ZERO;
    }
    let ntsc = (rate * 1001.0 / 1000.0).round();
    if (rate - ntsc * 1000.0 / 1001.0).abs() < 1e-3 && (rate - rate.round()).abs() > 1e-6 {
        Ticks::from_rational(value.round() as i64 * 1001, ntsc as i64 * 1000)
    } else if (rate - rate.round()).abs() < 1e-6 {
        Ticks::from_rational(value.round() as i64, rate.round() as i64)
    } else {
        Ticks::from_secs_f64(value / rate)
    }
}

fn rt(v: &Value) -> Option<Ticks> {
    Some(to_ticks(v.get("value")?.as_f64()?, v.get("rate")?.as_f64()?))
}

fn tr(v: &Value) -> Option<(Ticks, Ticks)> {
    Some((rt(v.get("start_time")?)?, rt(v.get("duration")?)?))
}

/// Reads a timeline. `resolve` finds footage for each clip; see [`resolve_in`].
pub fn read(
    text: &str,
    mut resolve: impl FnMut(&OtioClip) -> Option<AssetId>,
) -> Result<Placement, OtioError> {
    let root: Value = serde_json::from_str(text)?;
    let schema = root.get("OTIO_SCHEMA").and_then(Value::as_str).unwrap_or("");
    if !schema.starts_with("Timeline") {
        return Err(OtioError::NotATimeline);
    }
    let empty = Vec::new();
    let tracks = root.pointer("/tracks/children").and_then(Value::as_array).unwrap_or(&empty);

    let mut out = Placement::default();
    let mut index = 0u32;
    for track in tracks {
        let kind = match track.get("kind").and_then(Value::as_str) {
            Some("Audio") => TrackKind::Audio,
            _ => TrackKind::Video,
        };
        let mut cursor = Ticks::ZERO;
        for child in track.get("children").and_then(Value::as_array).unwrap_or(&empty) {
            let schema = child.get("OTIO_SCHEMA").and_then(Value::as_str).unwrap_or("");
            let span = child.get("source_range").and_then(tr);
            if schema.starts_with("Gap") {
                cursor += span.map_or(Ticks::ZERO, |s| s.1);
            } else if schema.starts_with("Clip") {
                index += 1;
                let name = child.get("name").and_then(Value::as_str).unwrap_or("").to_string();
                let reference = child.get("media_reference");
                let target = reference
                    .and_then(|r| r.get("target_url"))
                    .and_then(Value::as_str)
                    .map(str::to_string);
                // Without a source range the whole available range plays.
                let available = reference.and_then(|r| r.get("available_range")).and_then(tr);
                let Some((start, duration)) = span.or(available) else {
                    out.warnings.push(format!("{name}: no source range"));
                    continue;
                };
                let source_in = start - available.map_or(Ticks::ZERO, |a| a.0);

                // Speed: constant LinearTimeWarp only. The range is in source
                // time, so the clip lasts the range divided by the scalar.
                let mut scalar = 1.0;
                for effect in child.get("effects").and_then(Value::as_array).unwrap_or(&empty) {
                    match effect.get("OTIO_SCHEMA").and_then(Value::as_str) {
                        Some(s) if s.starts_with("LinearTimeWarp") => {
                            scalar = effect
                                .get("time_scalar")
                                .and_then(Value::as_f64)
                                .unwrap_or(1.0);
                        }
                        _ => out.warnings.push(format!("{name}: an effect was ignored")),
                    }
                }
                let speed = match Speed::from_f64(scalar) {
                    Ok(s) => s,
                    Err(_) => {
                        out.warnings.push(format!("{name}: unusable speed, left out"));
                        out.unresolved.push(index);
                        continue;
                    }
                };
                let timeline_duration = speed.source_to_timeline(duration);

                let found = OtioClip { index, name: name.clone(), track_kind: kind, target };
                match resolve(&found) {
                    Some(asset) if source_in >= Ticks::ZERO => out.clips.push(PlacedClip {
                        event: index,
                        track_kind: kind,
                        asset,
                        name,
                        source_in,
                        timeline_start: cursor,
                        duration: timeline_duration,
                        speed,
                    }),
                    _ => out.unresolved.push(index),
                }
                cursor += timeline_duration;
            } else if schema.starts_with("Transition") {
                out.warnings.push("a transition was ignored".to_string());
            } else {
                out.warnings.push(format!("{schema}: not supported, skipped"));
            }
        }
    }
    Ok(out)
}

/// Finds footage in `project` by the file name in the clip's URL, then by name.
pub fn resolve_in(project: &Project) -> impl FnMut(&OtioClip) -> Option<AssetId> + '_ {
    move |c| {
        let want = c.target.as_deref().map(|t| t.rsplit(['/', '\\']).next().unwrap_or(t));
        project
            .assets
            .iter()
            .find(|a| {
                let on_disk = a.path.file_name().map(|n| n.to_string_lossy());
                want.is_some_and(|w| {
                    on_disk.as_deref().is_some_and(|n| n.eq_ignore_ascii_case(w))
                        || a.name.eq_ignore_ascii_case(w)
                }) || (want.is_none() && a.name.eq_ignore_ascii_case(&c.name))
            })
            .map(|a| a.id)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use ve_core::{Clip, MediaInfo};
    use ve_time::Rate;

    fn project() -> (Project, AssetId) {
        let mut p = Project::with_default_sequence("p");
        let a = p.add_asset(
            "/media/beach.mov",
            MediaInfo { duration: Ticks::from_secs_f64(60.0), ..Default::default() },
        );
        let rate = Rate::FPS_30;
        let id = p.new_clip_id();
        let clip = Clip::new(
            id,
            a,
            "beach",
            rate.frame_to_ticks(30),
            rate.frame_to_ticks(60),
            rate.frame_to_ticks(90),
        );
        p.active_mut().unwrap().tracks[0].insert_clip(clip).unwrap();
        (p, a)
    }

    #[test]
    fn gaps_are_written_and_read_back_as_positions() {
        let (p, a) = project();
        let (text, warnings) = write(&p, p.active().unwrap());
        assert!(warnings.is_empty());
        assert!(text.contains("\"Gap.1\"") && text.contains("Clip.2"));

        let placed = read(&text, resolve_in(&p)).unwrap();
        assert_eq!(placed.clips.len(), 1, "{placed:?}");
        let c = &placed.clips[0];
        let rate = Rate::FPS_30;
        assert_eq!(c.asset, a);
        assert_eq!(c.timeline_start, rate.frame_to_ticks(60));
        assert_eq!(c.source_in, rate.frame_to_ticks(30));
        assert_eq!(c.duration, rate.frame_to_ticks(90));
    }

    #[test]
    fn speed_survives_a_round_trip() {
        let (mut p, _) = project();
        p.active_mut().unwrap().tracks[0].clips_mut().next().unwrap().speed =
            Speed::new(2, 1).unwrap();
        let (text, _) = write(&p, p.active().unwrap());
        let c = &read(&text, resolve_in(&p)).unwrap().clips[0];
        assert_eq!(c.speed, Speed::new(2, 1).unwrap());
        assert_eq!(c.duration, Rate::FPS_30.frame_to_ticks(90));
    }

    #[test]
    fn ntsc_times_land_on_exact_frames() {
        assert_eq!(to_ticks(24.0, 24000.0 / 1001.0), Rate::FPS_23_976.frame_to_ticks(24));
    }

    #[test]
    fn a_dissolve_and_a_non_timeline_are_handled_plainly() {
        let text = r#"{"OTIO_SCHEMA":"Timeline.1","tracks":{"children":[{"kind":"Video","children":[
            {"OTIO_SCHEMA":"Transition.1"}]}]}}"#;
        let placed = read(text, |_| None).unwrap();
        assert_eq!(placed.warnings.len(), 1);
        assert!(matches!(read("{}", |_| None), Err(OtioError::NotATimeline)));
        assert!(matches!(read("nope", |_| None), Err(OtioError::Json(_))));
    }

    #[test]
    fn media_not_in_the_project_is_reported_unresolved() {
        let (p, _) = project();
        let (text, _) = write(&p, p.active().unwrap());
        let placed = read(&text, |_| None).unwrap();
        assert_eq!(placed.unresolved, vec![1]);
    }
}
