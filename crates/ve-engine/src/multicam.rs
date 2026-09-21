//! What the angle viewer shows at one instant.
//!
//! The viewer is a grid of every camera in a group, all at the same moment, with
//! the one currently on screen marked. Working out what that grid contains is
//! the same kind of question [`crate::plan`] answers for the picture — which
//! file, at which instant — so it is answered the same way: purely, from the
//! project and a time, with no decoding and no GPU.
//!
//! The result is a list of media requests. The interface asks the decode service
//! for each one and draws whatever has arrived, exactly as the preview does with
//! the frames it needs.

use ve_core::{AngleId, AssetId, ClipId, MulticamId, Project, Sequence, TrackId, TrackKind};
use ve_time::Ticks;

/// One tile of the viewer.
#[derive(Debug, Clone, PartialEq)]
pub struct AngleView {
    pub angle: AngleId,
    /// The number that cuts to this angle — one-based, as the tile is labelled
    /// and as the keyboard reaches it.
    pub number: usize,
    pub name: String,
    pub asset: AssetId,
    /// Where in this camera's own media the instant falls.
    ///
    /// `None` when the camera was not rolling yet, or had already stopped. The
    /// tile is still listed: an angle that disappeared from the grid the moment
    /// it ran out would be a worse surprise than one that says it has nothing
    /// here.
    pub source_time: Option<Ticks>,
    /// Whether the clip under the playhead is showing this angle.
    pub active: bool,
    /// Whether this angle is switched on. A disabled angle keeps its number and
    /// its place in the grid.
    pub enabled: bool,
}

impl AngleView {
    /// The frame to ask the decode service for, if this angle has one here.
    pub fn request(&self) -> Option<(AssetId, Ticks)> {
        self.source_time.map(|t| (self.asset, t))
    }
}

/// The multicam clip under the playhead, and every angle it could be showing.
#[derive(Debug, Clone, PartialEq)]
pub struct MulticamAt {
    pub clip: ClipId,
    pub track: TrackId,
    pub group: MulticamId,
    pub active: AngleId,
    /// Where the playhead falls on the **group's** timeline, which is what each
    /// angle's offset is measured from.
    pub group_time: Ticks,
    /// Every angle, in number order.
    pub angles: Vec<AngleView>,
}

impl MulticamAt {
    /// The angle a one-based number cuts to, if there is one.
    pub fn numbered(&self, number: usize) -> Option<&AngleView> {
        self.angles.iter().find(|a| a.number == number)
    }

    /// Everything the viewer needs decoded for this instant, without duplicates.
    pub fn requests(&self) -> Vec<(AssetId, Ticks)> {
        let mut requests: Vec<(AssetId, Ticks)> =
            self.angles.iter().filter_map(|a| a.request()).collect();
        requests.sort_unstable_by_key(|(asset, at)| (*asset, at.raw()));
        requests.dedup();
        requests
    }
}

/// Finds the multicam clip under `at` and resolves every one of its angles.
///
/// `prefer` is the track the user last touched. A sequence can carry multicam
/// clips on several tracks at once, and the viewer has to be about one of them;
/// preferring the track being edited means pressing a number cuts the clip the
/// user is looking at rather than whichever one happens to be uppermost.
///
/// Falls back to the topmost video track carrying a multicam clip here, so the
/// viewer still fills in before any track has been touched.
pub fn multicam_at(
    project: &Project,
    sequence: &Sequence,
    at: Ticks,
    prefer: Option<TrackId>,
) -> Option<MulticamAt> {
    let preferred = prefer
        .and_then(|id| sequence.track(id))
        .and_then(|track| resolve(project, track.id, track.clip_at(at)?, at));
    if preferred.is_some() {
        return preferred;
    }

    // Video tracks in composite order, topmost first — the one whose picture
    // would be on screen is the one the viewer should be about.
    sequence
        .tracks
        .iter()
        .filter(|t| t.kind == TrackKind::Video)
        .rev()
        .find_map(|track| resolve(project, track.id, track.clip_at(at)?, at))
}

fn resolve(
    project: &Project,
    track: TrackId,
    clip: &ve_core::Clip,
    at: Ticks,
) -> Option<MulticamAt> {
    let (group_id, active) = clip.source.multicam()?;
    let group = project.multicam(group_id)?;
    // Group time, not timeline time: the clip's window into the group is what
    // every angle's offset is measured against.
    let group_time = clip.source_time_at(at)?;

    let angles = group
        .angles
        .iter()
        .enumerate()
        .map(|(index, angle)| AngleView {
            angle: angle.id,
            number: index + 1,
            name: angle.name.clone(),
            asset: angle.asset,
            source_time: group.source_time_at(angle.id, group_time).filter(|t| {
                // Past the end of the file is as much "nothing here" as before
                // the beginning of it.
                *t <= project.asset_duration(angle.asset)
            }),
            active: angle.id == active,
            enabled: angle.enabled,
        })
        .collect();

    Some(MulticamAt { clip: clip.id, track, group: group_id, active, group_time, angles })
}

#[cfg(test)]
mod tests {
    use super::*;
    use ve_core::{Clip, MulticamAngle, Source};

    fn info(seconds: i64) -> ve_core::MediaInfo {
        ve_core::MediaInfo {
            duration: Ticks::from_seconds(seconds),
            video: Some(ve_core::VideoStreamInfo {
                size: ve_core::Size::new(640, 360),
                rate: ve_time::Rate::FPS_25,
                duration: Ticks::from_seconds(seconds),
                frame_count: None,
                codec: "h264".into(),
                pixel_format: "yuv420p".into(),
                sample_aspect_ratio: (1, 1),
            }),
            audio: None,
            container: "mp4".into(),
            timecode: None,
        }
    }

    struct Fixture {
        project: Project,
        sequence: ve_core::SequenceId,
        v1: TrackId,
        v2: TrackId,
        group: MulticamId,
        angles: Vec<AngleId>,
    }

    /// Three cameras of 60 seconds each: camera 2 has ten seconds of pre-roll,
    /// camera 3 joins twenty seconds in.
    fn fixture() -> Fixture {
        let mut project = Project::with_default_sequence("Viewer");
        let assets: Vec<AssetId> = ["a", "b", "c"]
            .iter()
            .map(|n| project.add_asset(format!("/media/{n}.mp4"), info(60)))
            .collect();
        let sequence = project.active_sequence.unwrap();
        let (v1, v2) = {
            let s = project.sequence(sequence).unwrap();
            let video: Vec<TrackId> =
                s.tracks.iter().filter(|t| t.kind == TrackKind::Video).map(|t| t.id).collect();
            (video[0], video[1])
        };

        let group = project.add_multicam_group("Interview");
        let mut angles = Vec::new();
        for (index, offset) in [0i64, 10, -20].iter().enumerate() {
            let id = project.new_angle_id();
            let angle = MulticamAngle::new(id, format!("Cam {}", index + 1), assets[index])
                .with_offset(Ticks::from_seconds(*offset));
            project.multicam_mut(group).unwrap().push_angle(angle).unwrap();
            angles.push(id);
        }

        Fixture { project, sequence, v1, v2, group, angles }
    }

    impl Fixture {
        fn place(
            &mut self,
            track: TrackId,
            source: Source,
            start_s: i64,
            dur_s: i64,
        ) -> ClipId {
            let id = self.project.new_clip_id();
            let clip = Clip::new(
                id,
                source,
                "cut",
                Ticks::ZERO,
                Ticks::from_seconds(start_s),
                Ticks::from_seconds(dur_s),
            );
            self.project
                .sequence_mut(self.sequence)
                .unwrap()
                .track_mut(track)
                .unwrap()
                .insert_clip(clip)
                .unwrap();
            id
        }

        fn at(&self, seconds: i64, prefer: Option<TrackId>) -> Option<MulticamAt> {
            multicam_at(
                &self.project,
                self.project.sequence(self.sequence).unwrap(),
                Ticks::from_seconds(seconds),
                prefer,
            )
        }
    }

    #[test]
    fn every_angle_resolves_at_its_own_offset() {
        let mut f = fixture();
        let angle = f.angles[0];
        f.place(f.v1, Source::Multicam { group: f.group, angle }, 0, 40);

        let view = f.at(30, None).expect("a multicam clip is under the playhead");
        assert_eq!(view.group_time, Ticks::from_seconds(30));
        assert_eq!(view.angles.len(), 3);

        let times: Vec<Option<f64>> =
            view.angles.iter().map(|a| a.source_time.map(|t| t.as_secs_f64())).collect();
        assert_eq!(times, vec![Some(30.0), Some(40.0), Some(10.0)]);
        assert_eq!(view.angles.iter().map(|a| a.number).collect::<Vec<_>>(), vec![1, 2, 3]);
    }

    #[test]
    fn the_angle_on_screen_is_the_one_marked_active() {
        let mut f = fixture();
        let angle = f.angles[2];
        f.place(f.v1, Source::Multicam { group: f.group, angle }, 0, 40);

        let view = f.at(25, None).unwrap();
        assert_eq!(view.active, angle);
        let active: Vec<bool> = view.angles.iter().map(|a| a.active).collect();
        assert_eq!(active, vec![false, false, true]);
    }

    /// A camera that was not rolling still has a tile and still has its number.
    /// Dropping it from the grid would renumber everything after it, under the
    /// user's fingers, part way through a shot.
    #[test]
    fn a_camera_that_was_not_rolling_keeps_its_tile_and_its_number() {
        let mut f = fixture();
        let angle = f.angles[0];
        f.place(f.v1, Source::Multicam { group: f.group, angle }, 0, 40);

        let view = f.at(5, None).unwrap();
        assert_eq!(view.angles.len(), 3);
        assert_eq!(view.angles[2].number, 3);
        assert_eq!(view.angles[2].source_time, None, "camera 3 joins at twenty seconds");
        assert_eq!(view.angles[2].request(), None);
        assert!(view.angles[0].source_time.is_some());
    }

    #[test]
    fn a_camera_that_has_run_out_has_nothing_here_either() {
        let mut f = fixture();
        let angle = f.angles[0];
        f.place(f.v1, Source::Multicam { group: f.group, angle }, 0, 70);

        // At group time 55, camera 2 is 65 seconds into a 60 second file.
        let view = f.at(55, None).unwrap();
        assert_eq!(view.angles[1].source_time, None);
        assert!(view.angles[0].source_time.is_some(), "camera 1 is still running");
    }

    #[test]
    fn the_viewer_asks_for_one_decode_per_angle_that_has_a_frame() {
        let mut f = fixture();
        let angle = f.angles[0];
        f.place(f.v1, Source::Multicam { group: f.group, angle }, 0, 40);

        assert_eq!(f.at(30, None).unwrap().requests().len(), 3);
        assert_eq!(
            f.at(5, None).unwrap().requests().len(),
            2,
            "the camera with nothing here is not decoded"
        );
    }

    #[test]
    fn the_track_being_edited_wins_over_the_topmost_one() {
        let mut f = fixture();
        f.place(f.v1, Source::Multicam { group: f.group, angle: f.angles[0] }, 0, 40);
        f.place(f.v2, Source::Multicam { group: f.group, angle: f.angles[1] }, 0, 40);

        // With no preference, the upper track's clip is the one on screen.
        assert_eq!(f.at(10, None).unwrap().active, f.angles[1]);
        // With one, the clip the user is editing.
        assert_eq!(f.at(10, Some(f.v1)).unwrap().active, f.angles[0]);
        assert_eq!(f.at(10, Some(f.v1)).unwrap().track, f.v1);
    }

    #[test]
    fn a_preferred_track_with_nothing_here_falls_back_rather_than_going_blank() {
        let mut f = fixture();
        f.place(f.v2, Source::Multicam { group: f.group, angle: f.angles[1] }, 0, 40);

        // V1 is empty at this instant; the viewer still finds the clip on V2.
        let view = f.at(10, Some(f.v1)).expect("the fallback finds the other track");
        assert_eq!(view.track, f.v2);
    }

    #[test]
    fn a_plain_clip_is_not_a_multicam_clip() {
        let mut f = fixture();
        let asset = f.project.assets[0].id;
        f.place(f.v1, Source::Asset(asset), 0, 40);
        assert_eq!(f.at(10, None), None);
    }

    #[test]
    fn nothing_under_the_playhead_means_nothing_to_show() {
        let mut f = fixture();
        f.place(f.v1, Source::Multicam { group: f.group, angle: f.angles[0] }, 0, 10);
        assert_eq!(f.at(30, None), None);
    }

    #[test]
    fn a_clip_trimmed_into_the_group_reads_the_angles_from_its_own_window() {
        let mut f = fixture();
        let id = f.project.new_clip_id();
        // Ten seconds into the group, placed at timeline zero.
        let clip = Clip::new(
            id,
            Source::Multicam { group: f.group, angle: f.angles[0] },
            "trimmed",
            Ticks::from_seconds(10),
            Ticks::ZERO,
            Ticks::from_seconds(20),
        );
        f.project
            .sequence_mut(f.sequence)
            .unwrap()
            .track_mut(f.v1)
            .unwrap()
            .insert_clip(clip)
            .unwrap();

        let view = f.at(5, None).unwrap();
        assert_eq!(view.group_time, Ticks::from_seconds(15), "ten in, five along");
        assert_eq!(view.angles[0].source_time, Some(Ticks::from_seconds(15)));
        assert_eq!(view.angles[1].source_time, Some(Ticks::from_seconds(25)));
    }

    #[test]
    fn a_disabled_angle_is_listed_as_disabled_rather_than_hidden() {
        let mut f = fixture();
        f.place(f.v1, Source::Multicam { group: f.group, angle: f.angles[0] }, 0, 40);
        f.project.multicam_mut(f.group).unwrap().angles[1].enabled = false;

        let view = f.at(10, None).unwrap();
        assert_eq!(view.angles.len(), 3);
        assert!(!view.angles[1].enabled);
        assert_eq!(view.angles[2].number, 3, "numbers do not shuffle");
    }

    #[test]
    fn a_number_reaches_the_angle_it_labels() {
        let mut f = fixture();
        f.place(f.v1, Source::Multicam { group: f.group, angle: f.angles[0] }, 0, 40);
        let view = f.at(10, None).unwrap();

        assert_eq!(view.numbered(1).map(|a| a.angle), Some(f.angles[0]));
        assert_eq!(view.numbered(3).map(|a| a.angle), Some(f.angles[2]));
        assert_eq!(view.numbered(4), None);
        assert_eq!(view.numbered(0), None);
    }
}
