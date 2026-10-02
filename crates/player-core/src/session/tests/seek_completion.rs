use super::test_support::*;
use super::*;
use crate::PendingVideoPacketTimestamps;

fn final_seek_harness_with_actual_position(
    target: Duration,
    actual: Duration,
) -> SeekRegressionHarness {
    let video_track = fake_track(1, TrackKind::Video);
    let demuxer = scripted_seek_demuxer(vec![video_track.clone()], target, actual, Vec::new());
    let mut harness = SeekRegressionHarness::new(vec![video_track], demuxer);
    harness
        .session
        .pipeline
        .set_present_video_frame(decoded_frame_for_tests(Duration::from_secs(1), 1));
    harness.start_final_seek(MediaTime::from_duration(target));
    let _events_before_present = harness.session.take_events();

    harness
}

/// Собирает video-only playing seek, где demux actual раньше requested target.
fn playing_final_seek_harness_with_actual_position(
    target: Duration,
    actual: Duration,
) -> SeekRegressionHarness {
    let video_track = fake_track(1, TrackKind::Video);
    let demuxer = scripted_seek_demuxer(vec![video_track.clone()], target, actual, Vec::new());
    let mut harness = SeekRegressionHarness::new(vec![video_track], demuxer);
    harness
        .session
        .pipeline
        .set_present_video_frame(decoded_frame_for_tests(Duration::from_secs(1), 1));
    harness
        .session
        .dispatch_command(PlayerCommand::Play)
        .unwrap();
    harness.start_final_seek(MediaTime::from_duration(target));
    let _events_before_present = harness.session.take_events();

    harness
}

mod commit_gates;
mod output_floor_freeze;
mod preroll_backpressure;
mod wakeup_resume;
