//! CMX3600 edit decision lists.
//!
//! ```text
//! TITLE: My Cut
//! FCM: NON-DROP FRAME
//!
//! 001  A001C003 V     C        01:00:00:00 01:00:05:00 01:00:00:00 01:00:05:00
//! * FROM CLIP NAME: beach.mov
//! ```
//!
//! Times in a list are timecode, and a list does not say what rate its
//! timecode counts at, so every function that reads or writes one is given the
//! rate. Events hold **frame indices** at that rate rather than ticks: a frame
//! is what the file contains, and keeping it so means a read followed by a
//! write reproduces the file's numbers exactly.

use std::fmt::Write as _;

use ve_core::{AssetId, MediaAsset, Project, Sequence, Source, Speed, TrackKind};
use ve_time::{Rate, Ticks, Timecode};

#[derive(Debug, thiserror::Error)]
pub enum EdlError {
    #[error("no edit events found — is this a CMX3600 list?")]
    Empty,
}

/// Which parts of the picture an event touches.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum EdlTrackKind {
    Video,
    Audio,
    /// `B`, or `AA/V`: sound and picture together.
    Both,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Transition {
    Cut,
    /// A dissolve of this many frames.
    Dissolve(u32),
    /// A wipe (pattern code, frames), or any other effect this build does not model.
    Other(String, u32),
}

/// One numbered line of a list, with the comments that belong to it.
#[derive(Debug, Clone, PartialEq)]
pub struct EdlEvent {
    pub number: u32,
    pub reel: String,
    /// The track field verbatim — `V`, `A`, `A2`, `AA/V`, `B`.
    pub track: String,
    pub kind: EdlTrackKind,
    pub transition: Transition,
    pub source_in: i64,
    pub source_out: i64,
    pub record_in: i64,
    pub record_out: i64,
    /// From `* FROM CLIP NAME:`.
    pub clip_name: Option<String>,
    /// From `* SOURCE FILE:`.
    pub source_file: Option<String>,
    /// From an `M2` line: the playback rate in frames per second, negative
    /// for reverse.
    pub motion_fps: Option<f64>,
}

impl EdlEvent {
    pub fn record_frames(&self) -> i64 {
        self.record_out - self.record_in
    }
}

/// A whole list.
#[derive(Debug, Clone, PartialEq)]
pub struct Edl {
    pub title: String,
    pub rate: Rate,
    pub events: Vec<EdlEvent>,
    pub warnings: Vec<String>,
}

// ---- reading ----------------------------------------------------------------

impl Edl {
    /// Reads a list whose timecode counts at `rate`.
    ///
    /// Lenient: a line that cannot be understood costs that line and a
    /// warning, not the import.
    pub fn parse(text: &str, rate: Rate) -> Result<Edl, EdlError> {
        let mut edl =
            Edl { title: String::new(), rate, events: Vec::new(), warnings: Vec::new() };

        for (i, raw) in text.lines().enumerate() {
            let line = raw.trim();
            if line.is_empty() {
                continue;
            }
            let at = i + 1;

            if let Some(t) = line.strip_prefix("TITLE:") {
                edl.title = t.trim().to_string();
            } else if line.starts_with("FCM:") {
                // Drop or non-drop is a property of the timecode strings,
                // which `Timecode::parse` reads from their separators.
            } else if let Some(comment) = line.strip_prefix('*') {
                let comment = comment.trim();
                let Some(event) = edl.events.last_mut() else { continue };
                if let Some(v) = comment.strip_prefix("FROM CLIP NAME:") {
                    event.clip_name = Some(v.trim().to_string());
                } else if let Some(v) = comment.strip_prefix("SOURCE FILE:") {
                    event.source_file = Some(v.trim().to_string());
                }
            } else if let Some(m) = line.strip_prefix("M2") {
                let fields: Vec<&str> = m.split_whitespace().collect();
                match (edl.events.last_mut(), fields.get(1).and_then(|f| f.parse::<f64>().ok()))
                {
                    (Some(event), Some(fps)) => event.motion_fps = Some(fps),
                    _ => edl.warnings.push(format!("line {at}: unreadable motion line")),
                }
            } else if line.chars().next().is_some_and(|c| c.is_ascii_digit()) {
                match parse_event(line, rate) {
                    Ok(event) => edl.events.push(event),
                    Err(why) => edl.warnings.push(format!("line {at}: {why}")),
                }
            } else {
                // `AUD 3`, `EFFECTS NAME IS ..`, and anything else a vendor adds.
            }
        }

        if edl.events.is_empty() {
            return Err(EdlError::Empty);
        }
        Ok(edl)
    }
}

fn parse_event(line: &str, rate: Rate) -> Result<EdlEvent, String> {
    let t: Vec<&str> = line.split_whitespace().collect();
    if t.len() < 8 {
        return Err("too few columns for an edit event".into());
    }
    let number = t[0].parse::<u32>().map_err(|_| "bad event number".to_string())?;

    let tc = |s: &str| -> Result<i64, String> {
        Timecode::parse(s, rate)
            .map(Timecode::to_frame)
            .map_err(|_| format!("bad timecode {s}"))
    };
    let n = t.len();
    let (source_in, source_out, record_in, record_out) =
        (tc(t[n - 4])?, tc(t[n - 3])?, tc(t[n - 2])?, tc(t[n - 1])?);

    let track = t[2].to_string();
    let kind = match track.as_str() {
        "V" => EdlTrackKind::Video,
        "B" | "AA/V" | "A/V" => EdlTrackKind::Both,
        s if s.starts_with('A') => EdlTrackKind::Audio,
        _ => return Err(format!("unknown track {track}")),
    };

    let frames = t.get(4).filter(|_| n > 8).and_then(|d| d.parse::<u32>().ok()).unwrap_or(0);
    let transition = match t[3] {
        "C" => Transition::Cut,
        "D" => Transition::Dissolve(frames),
        other => Transition::Other(other.to_string(), frames),
    };

    if record_out <= record_in {
        return Err("record out is not after record in".into());
    }

    Ok(EdlEvent {
        number,
        reel: t[1].to_string(),
        track,
        kind,
        transition,
        source_in,
        source_out,
        record_in,
        record_out,
        clip_name: None,
        source_file: None,
        motion_fps: None,
    })
}

// ---- placing ----------------------------------------------------------------

/// Where an event's footage is, as the caller found it.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Resolved {
    pub asset: AssetId,
    /// Frame index (at the list's rate) of the asset's first frame: its own
    /// start timecode. Source times in a list are absolute timecode, so this
    /// is what turns them into an offset into the file.
    pub origin: i64,
}

/// A clip a list asks for, described rather than made.
#[derive(Debug, Clone, PartialEq)]
pub struct PlacedClip {
    pub event: u32,
    pub track_kind: TrackKind,
    pub asset: AssetId,
    pub name: String,
    pub source_in: Ticks,
    pub timeline_start: Ticks,
    pub duration: Ticks,
    pub speed: Speed,
}

#[derive(Debug, Clone, Default, PartialEq)]
pub struct Placement {
    pub clips: Vec<PlacedClip>,
    /// Events the caller could not find footage for.
    pub unresolved: Vec<u32>,
    pub warnings: Vec<String>,
}

impl Edl {
    /// Describes the clips this list asks for.
    ///
    /// `record_origin` is the frame the sequence starts at (its start
    /// timecode), so a list recorded from 01:00:00:00 lands at zero.
    /// `resolve` finds the footage for an event; see [`resolve_in`].
    pub fn place(
        &self,
        record_origin: i64,
        mut resolve: impl FnMut(&EdlEvent) -> Option<Resolved>,
    ) -> Placement {
        let mut out = Placement::default();

        for e in &self.events {
            let Some(found) = resolve(e) else {
                out.unresolved.push(e.number);
                continue;
            };
            if let Transition::Dissolve(_) | Transition::Other(..) = e.transition {
                out.warnings.push(format!("event {}: transition placed as a cut", e.number));
            }

            let speed = match e.motion_fps {
                None => Speed::NORMAL,
                Some(fps) if fps <= 0.0 => {
                    out.warnings
                        .push(format!("event {}: reverse or freeze not supported", e.number));
                    out.unresolved.push(e.number);
                    continue;
                }
                Some(fps) => match Speed::from_f64(fps / self.rate.as_f64()) {
                    Ok(s) => s,
                    Err(_) => {
                        out.unresolved.push(e.number);
                        continue;
                    }
                },
            };

            let (rec, src) = (e.record_in - record_origin, e.source_in - found.origin);
            if rec < 0 || src < 0 {
                out.warnings
                    .push(format!("event {}: starts before the sequence or file", e.number));
                out.unresolved.push(e.number);
                continue;
            }

            let timeline_start = self.rate.frame_to_ticks(rec);
            let duration = self.rate.frame_to_ticks(rec + e.record_frames()) - timeline_start;
            let name = e.clip_name.clone().unwrap_or_else(|| e.reel.clone());

            let kinds: &[TrackKind] = match e.kind {
                EdlTrackKind::Video => &[TrackKind::Video],
                EdlTrackKind::Audio => &[TrackKind::Audio],
                EdlTrackKind::Both => &[TrackKind::Video, TrackKind::Audio],
            };
            for &track_kind in kinds {
                out.clips.push(PlacedClip {
                    event: e.number,
                    track_kind,
                    asset: found.asset,
                    name: name.clone(),
                    source_in: self.rate.frame_to_ticks(src),
                    timeline_start,
                    duration,
                    speed,
                });
            }
        }
        out
    }
}

/// The frame index at which `asset`'s footage starts, from the timecode its
/// container recorded; zero when it recorded none or it cannot be read.
pub fn asset_origin(asset: &MediaAsset, rate: Rate) -> i64 {
    asset
        .info
        .timecode
        .as_deref()
        .and_then(|s| Timecode::parse(s, rate).ok())
        .map_or(0, Timecode::to_frame)
}

/// A resolver that finds footage in `project` by name: the event's
/// `SOURCE FILE`, then its clip name, then its reel, each compared with the
/// asset's file name and display name, ignoring case.
pub fn resolve_in(
    project: &Project,
    rate: Rate,
) -> impl FnMut(&EdlEvent) -> Option<Resolved> + '_ {
    move |e| {
        let candidates =
            [e.source_file.as_deref(), e.clip_name.as_deref(), Some(e.reel.as_str())];
        for want in candidates.into_iter().flatten() {
            let want = file_name(want);
            let hit = project.assets.iter().find(|a| {
                let on_disk = a.path.file_name().map(|n| n.to_string_lossy());
                on_disk.is_some_and(|n| n.eq_ignore_ascii_case(want))
                    || a.name.eq_ignore_ascii_case(want)
            });
            if let Some(a) = hit {
                return Some(Resolved { asset: a.id, origin: asset_origin(a, rate) });
            }
        }
        None
    }
}

fn file_name(path: &str) -> &str {
    path.rsplit(['/', '\\']).next().unwrap_or(path)
}

// ---- writing ----------------------------------------------------------------

impl Edl {
    /// Describes `sequence` as a list: one event per enabled clip that plays
    /// a file, video tracks first, then audio, each in time order.
    ///
    /// Compositions, graphics and multicam sources have no reel to name and
    /// are left out with a warning, as are disabled clips.
    pub fn from_sequence(project: &Project, sequence: &Sequence) -> Edl {
        let rate = sequence.rate();
        let origin = rate.ticks_to_frame_round(sequence.settings.start_timecode);
        let mut edl = Edl {
            title: sequence.name.clone(),
            rate,
            events: Vec::new(),
            warnings: Vec::new(),
        };
        let mut reels: Vec<(AssetId, String)> = Vec::new();

        let tracks = sequence.video_tracks().map(|t| (t, "V".to_string())).chain(
            sequence.audio_tracks().enumerate().map(|(i, t)| {
                (t, if i == 0 { "A".to_string() } else { format!("A{}", i + 1) })
            }),
        );

        for (track, label) in tracks {
            for clip in track.clips() {
                let Source::Asset(id) = clip.source else {
                    edl.warnings.push(format!("{}: not a file, left out", clip.name));
                    continue;
                };
                if !clip.enabled {
                    edl.warnings.push(format!("{}: disabled, left out", clip.name));
                    continue;
                }
                let Some(asset) = project.asset(id) else { continue };

                let reel = match reels.iter().find(|(a, _)| *a == id) {
                    Some((_, r)) => r.clone(),
                    None => {
                        let r = reel_name(asset, &reels);
                        reels.push((id, r.clone()));
                        r
                    }
                };

                let src_in =
                    asset_origin(asset, rate) + rate.ticks_to_frame_round(clip.source_in);
                let src_len = rate.ticks_to_frame_round(clip.source_duration()).max(1);
                let rec_in = origin + rate.ticks_to_frame_round(clip.timeline_start);
                let rec_len = rate.ticks_to_frame_round(clip.duration).max(1);

                edl.events.push(EdlEvent {
                    number: edl.events.len() as u32 + 1,
                    reel,
                    track: label.clone(),
                    kind: if track.kind == TrackKind::Video {
                        EdlTrackKind::Video
                    } else {
                        EdlTrackKind::Audio
                    },
                    transition: Transition::Cut,
                    source_in: src_in,
                    source_out: src_in + src_len,
                    record_in: rec_in,
                    record_out: rec_in + rec_len,
                    clip_name: Some(clip.name.clone()),
                    source_file: Some(asset.path.to_string_lossy().into_owned()),
                    motion_fps: (!clip.speed.is_normal())
                        .then(|| clip.speed.as_f64() * rate.as_f64()),
                });
            }
        }
        edl
    }

    /// Renders the list. Strict where the reader is liberal: fixed columns,
    /// `\n` line endings, a blank line between events.
    pub fn to_text(&self) -> String {
        let tc = |f: i64| Timecode::from_frame(f, self.rate).to_string();
        let mut out = String::new();
        let _ = writeln!(out, "TITLE: {}", self.title);
        let fcm = if self.rate.is_drop_frame() { "DROP FRAME" } else { "NON-DROP FRAME" };
        let _ = writeln!(out, "FCM: {fcm}\n");

        for e in &self.events {
            let _ = writeln!(
                out,
                "{:03}  {:<8} {:<5} {:<8} {} {} {} {}",
                e.number,
                e.reel,
                e.track,
                "C",
                tc(e.source_in),
                tc(e.source_out),
                tc(e.record_in),
                tc(e.record_out),
            );
            if let Some(fps) = e.motion_fps {
                let _ = writeln!(
                    out,
                    "M2   {:<8} {:>5.1}                {}",
                    e.reel,
                    fps,
                    tc(e.source_in)
                );
            }
            if let Some(n) = &e.clip_name {
                let _ = writeln!(out, "* FROM CLIP NAME: {n}");
            }
            if let Some(f) = &e.source_file {
                let _ = writeln!(out, "* SOURCE FILE: {f}");
            }
            out.push('\n');
        }
        out
    }
}

/// Up to eight upper-case letters and digits from the file name, made unique
/// among the reels already handed out. The full name travels in the comments.
fn reel_name(asset: &MediaAsset, taken: &[(AssetId, String)]) -> String {
    let stem =
        asset.path.file_stem().map(|s| s.to_string_lossy().into_owned()).unwrap_or_default();
    let mut base: String = stem
        .chars()
        .filter(char::is_ascii_alphanumeric)
        .map(|c| c.to_ascii_uppercase())
        .collect();
    base.truncate(8);
    if base.is_empty() {
        base = "AX".into();
    }
    let mut name = base.clone();
    let mut n = 1;
    while taken.iter().any(|(_, r)| *r == name) {
        let suffix = n.to_string();
        let keep = base.len().min(8 - suffix.len());
        name = format!("{}{}", &base[..keep], suffix);
        n += 1;
    }
    name
}

#[cfg(test)]
mod tests {
    use super::*;
    use ve_core::{Clip, MediaInfo};

    fn project_with_clip(rate_tc: Option<&str>) -> (Project, AssetId) {
        let mut p = Project::with_default_sequence("p");
        let info = MediaInfo {
            duration: Ticks::from_secs_f64(60.0),
            timecode: rate_tc.map(str::to_string),
            ..Default::default()
        };
        let asset = p.add_asset("/media/Beach Shot 01.mov", info);
        (p, asset)
    }

    fn place(p: &mut Project, asset: AssetId, src_in: f64, start: f64, dur: f64) {
        let id = p.new_clip_id();
        let seq = p.active_mut().unwrap();
        let rate = seq.rate();
        let mut c = Clip::new(
            id,
            asset,
            "Beach",
            rate.frame_to_ticks((src_in * 30.0) as i64),
            rate.frame_to_ticks((start * 30.0) as i64),
            rate.frame_to_ticks((dur * 30.0) as i64),
        );
        c.enabled = true;
        seq.tracks[0].insert_clip(c).unwrap();
    }

    #[test]
    fn writes_the_columns_a_reader_expects() {
        let (mut p, a) = project_with_clip(Some("01:00:00:00"));
        place(&mut p, a, 2.0, 0.0, 5.0);
        let edl = Edl::from_sequence(&p, p.active().unwrap());
        let text = edl.to_text();
        assert!(text.contains("FCM: NON-DROP FRAME"));
        assert!(text.contains(
            "001  BEACHSHO V     C        01:00:02:00 01:00:07:00 00:00:00:00 00:00:05:00"
        ));
        assert!(text.contains("* FROM CLIP NAME: Beach"));
    }

    #[test]
    fn what_is_written_is_read_back_onto_the_same_frames() {
        let (mut p, a) = project_with_clip(Some("01:00:00:00"));
        place(&mut p, a, 2.0, 1.0, 5.0);
        let text = Edl::from_sequence(&p, p.active().unwrap()).to_text();

        let back = Edl::parse(&text, Rate::FPS_30).unwrap();
        assert_eq!(back.events.len(), 1);
        let placed = back.place(0, resolve_in(&p, Rate::FPS_30));
        assert!(placed.unresolved.is_empty(), "{placed:?}");
        let c = &placed.clips[0];
        let rate = Rate::FPS_30;
        assert_eq!(c.asset, a);
        assert_eq!(c.source_in, rate.frame_to_ticks(60));
        assert_eq!(c.timeline_start, rate.frame_to_ticks(30));
        assert_eq!(c.duration, rate.frame_to_ticks(150));
    }

    #[test]
    fn speed_change_round_trips_through_m2() {
        let (mut p, a) = project_with_clip(None);
        place(&mut p, a, 0.0, 0.0, 2.0);
        p.active_mut().unwrap().tracks[0].clips_mut().next().unwrap().speed =
            Speed::new(2, 1).unwrap();
        let text = Edl::from_sequence(&p, p.active().unwrap()).to_text();
        assert!(text.contains("M2   BEACHSHO  60.0"), "{text}");
        let back = Edl::parse(&text, Rate::FPS_30).unwrap();
        let placed = back.place(0, resolve_in(&p, Rate::FPS_30));
        assert_eq!(placed.clips[0].speed, Speed::new(2, 1).unwrap());
    }

    #[test]
    fn a_dissolve_is_reported_and_placed_as_a_cut() {
        let text = "TITLE: t\nFCM: NON-DROP FRAME\n\n\
            001  AX       V     C        00:00:00:00 00:00:02:00 00:00:00:00 00:00:02:00\n\
            002  AX       V     D    015 00:00:05:00 00:00:07:00 00:00:02:00 00:00:04:00\n";
        let edl = Edl::parse(text, Rate::FPS_30).unwrap();
        assert_eq!(edl.events[1].transition, Transition::Dissolve(15));
        let placed =
            edl.place(0, |_| Some(Resolved { asset: AssetId::from_raw(1), origin: 0 }));
        assert_eq!(placed.clips.len(), 2);
        assert_eq!(placed.warnings.len(), 1);
    }

    #[test]
    fn a_bad_line_costs_the_line_not_the_list() {
        let text = "001  AX V C 00:00:00:00 00:00:01:00 00:00:00:00 00:00:01:00\n\
                    002  AX V C nonsense\n";
        let edl = Edl::parse(text, Rate::FPS_30).unwrap();
        assert_eq!(edl.events.len(), 1);
        assert_eq!(edl.warnings.len(), 1);
        assert!(matches!(Edl::parse("hello", Rate::FPS_30), Err(EdlError::Empty)));
    }

    #[test]
    fn drop_frame_lists_are_written_with_semicolons() {
        let (mut p, a) = project_with_clip(None);
        place(&mut p, a, 0.0, 0.0, 1.0);
        p.active_mut().unwrap().settings.rate = Rate::FPS_29_97;
        let text = Edl::from_sequence(&p, p.active().unwrap()).to_text();
        assert!(text.contains("FCM: DROP FRAME"));
        assert!(text.contains("00:00:00;00"));
    }

    #[test]
    fn colliding_reel_names_are_made_unique() {
        let mut p = Project::new("p");
        let a = p.add_asset("/m/Interview_A.mov", MediaInfo::default());
        let b = p.add_asset("/m/Interview_B.mov", MediaInfo::default());
        let first = reel_name(p.asset(a).unwrap(), &[]);
        let second = reel_name(p.asset(b).unwrap(), &[(a, first.clone())]);
        assert_eq!(first, "INTERVIE");
        assert_eq!(second, "INTERVI1");
    }
}
