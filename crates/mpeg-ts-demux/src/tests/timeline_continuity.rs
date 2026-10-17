//! Unit-тесты `PresentationTimeline`: эпохи, привязка по звуку, PCR, seek.

use std::time::Duration;

use super::*;

const AUDIO_PID: u16 = 0x101;
const VIDEO_PID: u16 = 0x100;
/// AC-3 кадр 1536 сэмплов @ 48 кГц.
const AUDIO_FRAME: i64 = 2_880;
/// 10 к/с.
const VIDEO_STEP: i64 = 9_000;
/// Старт ffmpeg-записи.
const RECORDING_START: i64 = 126_000;

fn rebasing() -> PresentationTimeline {
    PresentationTimeline::new(
        TimelineOwnership::RebasedByDemuxer,
        TimestampDiscontinuityThresholds::default(),
    )
}

fn audio(timeline: &mut PresentationTimeline, raw: i64, byte: u64) -> ElementaryPlacement {
    timeline.place_elementary(
        AUDIO_PID,
        ElementaryClock::Audio,
        raw,
        Some(AUDIO_FRAME),
        byte,
    )
}

fn video(timeline: &mut PresentationTimeline, raw: i64, byte: u64) -> ElementaryPlacement {
    timeline.place_elementary(VIDEO_PID, ElementaryClock::Video, raw, None, byte)
}

/// Первая запись: `frames` аудиокадров и видеокадры той же длины.
fn play_first_recording(timeline: &mut PresentationTimeline, audio_frames: i64) {
    for frame in 0..audio_frames {
        assert_eq!(
            audio(timeline, RECORDING_START + frame * AUDIO_FRAME, 0).epoch,
            EpochPlacement::STREAM_START
        );
    }
    for frame in 0..(audio_frames * AUDIO_FRAME / VIDEO_STEP) {
        assert_eq!(
            video(timeline, RECORDING_START + frame * VIDEO_STEP, 0).epoch,
            EpochPlacement::STREAM_START
        );
    }
}

#[test]
fn regular_stream_keeps_raw_timestamps_untouched() {
    let mut timeline = rebasing();
    // Небольшой скачок вперёд (потеря пакетов) — настоящее время, не разрыв.
    for raw in [
        RECORDING_START,
        RECORDING_START + AUDIO_FRAME,
        RECORDING_START + 90_000,
    ] {
        let placement = audio(&mut timeline, raw, 0);
        assert_eq!(placement.epoch.present(raw), raw);
        assert_eq!(placement.revision, EpochRevision::Unchanged);
    }
    let pcr = timeline.place_program_clock(RECORDING_START);
    assert_eq!(
        pcr,
        ProgramClockPlacement::OnTimeline {
            pcr_90khz: RECORDING_START,
            epoch: EpochPlacement::STREAM_START
        }
    );
}

#[test]
fn audio_first_after_backward_jump_continues_exactly_and_video_follows() {
    let mut timeline = rebasing();
    play_first_recording(&mut timeline, 25);
    let first_end = RECORDING_START + 25 * AUDIO_FRAME;

    let joined = audio(&mut timeline, RECORDING_START, 5_000);
    assert_eq!(joined.epoch.id, TimelineEpochId::StartsAtByte(5_000));
    assert_eq!(joined.epoch.present(RECORDING_START), first_end);

    // Видео второй записи переходит в ту же эпоху: собственная A/V
    // синхронизация записи сохраняется одним сдвигом.
    let video_join = video(&mut timeline, RECORDING_START + 480, 5_100);
    assert_eq!(video_join.epoch, joined.epoch);
    assert_eq!(video_join.revision, EpochRevision::Unchanged);
}

#[test]
fn video_first_epoch_is_refined_by_first_audio_frame() {
    let mut timeline = rebasing();
    play_first_recording(&mut timeline, 25);
    let first_audio_end = RECORDING_START + 25 * AUDIO_FRAME;

    // Видео пришло первым и поставило эпоху по своему шагу (предварительно).
    let provisional = video(&mut timeline, RECORDING_START + 4_000, 7_000);
    let video_expected = RECORDING_START + (25 * AUDIO_FRAME / VIDEO_STEP) * VIDEO_STEP;
    assert_eq!(
        provisional.epoch.present(RECORDING_START + 4_000),
        video_expected
    );

    // Первый аудиокадр уточняет сдвиг: звук без дыры и перекрытия.
    let refined = audio(&mut timeline, RECORDING_START, 7_200);
    assert_eq!(refined.revision, EpochRevision::OffsetRefined);
    assert_eq!(refined.epoch.id, provisional.epoch.id);
    assert_eq!(refined.epoch.present(RECORDING_START), first_audio_end);

    // Дальше видео идёт с уточнённым сдвигом.
    let next_video = video(&mut timeline, RECORDING_START + 4_000 + VIDEO_STEP, 7_400);
    assert_eq!(next_video.epoch, refined.epoch);
    // Второй аудиокадр — без новых уточнений.
    assert_eq!(
        audio(&mut timeline, RECORDING_START + AUDIO_FRAME, 7_500).revision,
        EpochRevision::Unchanged
    );
}

#[test]
fn forward_jump_beyond_threshold_is_a_new_epoch() {
    let mut timeline = rebasing();
    audio(&mut timeline, RECORDING_START, 0);
    let far_future = RECORDING_START + 90_000 * 3_600;
    let placement = audio(&mut timeline, far_future, 1_000);
    assert_eq!(
        placement.epoch.present(far_future),
        RECORDING_START + AUDIO_FRAME
    );
}

#[test]
fn program_clock_waits_for_elementary_epoch_after_its_jump() {
    let mut timeline = rebasing();
    play_first_recording(&mut timeline, 25);
    let last_pcr = RECORDING_START + 70_000;
    assert!(matches!(
        timeline.place_program_clock(last_pcr),
        ProgramClockPlacement::OnTimeline { .. }
    ));
    // Скачок PCR раньше любого PES новой записи: эпохи ещё нет.
    assert_eq!(
        timeline.place_program_clock(RECORDING_START - 20_000),
        ProgramClockPlacement::AwaitingEpoch
    );
    let joined = audio(&mut timeline, RECORDING_START, 9_000);
    let placement = timeline.place_program_clock(RECORDING_START - 10_000);
    assert_eq!(
        placement,
        ProgramClockPlacement::OnTimeline {
            pcr_90khz: joined.epoch.present(RECORDING_START - 10_000),
            epoch: joined.epoch,
        }
    );
}

#[test]
fn restart_in_epoch_maps_with_anchor_offset_and_forgets_lane_history() {
    let mut timeline = rebasing();
    play_first_recording(&mut timeline, 25);
    let joined = audio(&mut timeline, RECORDING_START, 5_000);

    let mut after_seek = rebasing();
    after_seek.restart_in_epoch(joined.epoch);
    // Пакеты после anchor-а — сразу на непрерывной шкале, без истории первой записи.
    let placement = video(&mut after_seek, RECORDING_START + 480, 5_100);
    assert_eq!(placement.epoch, joined.epoch);

    after_seek.restart_from_stream_start();
    assert_eq!(
        audio(&mut after_seek, RECORDING_START, 0).epoch,
        EpochPlacement::STREAM_START
    );
}

#[test]
fn transport_owned_timeline_never_rebases() {
    let mut timeline = PresentationTimeline::new(
        TimelineOwnership::OwnedByTransport,
        TimestampDiscontinuityThresholds::default(),
    );
    play_first_recording(&mut timeline, 25);
    let placement = audio(&mut timeline, RECORDING_START, 5_000);
    assert_eq!(placement.epoch, EpochPlacement::STREAM_START);
    assert_eq!(
        timeline.place_program_clock(0),
        ProgramClockPlacement::OnTimeline {
            pcr_90khz: 0,
            epoch: EpochPlacement::STREAM_START
        }
    );
}

#[test]
fn thresholds_come_from_options() {
    let mut timeline = PresentationTimeline::new(
        TimelineOwnership::RebasedByDemuxer,
        TimestampDiscontinuityThresholds {
            backward_tolerance: Duration::ZERO,
            forward_threshold: Duration::from_millis(100),
        },
    );
    audio(&mut timeline, RECORDING_START, 0);
    // 0.2 с вперёд при пороге 0.1 с — уже разрыв: звук продолжается без дыры.
    let jumped = RECORDING_START + 18_000;
    let placement = audio(&mut timeline, jumped, 1_000);
    assert_eq!(
        placement.epoch.present(jumped),
        RECORDING_START + AUDIO_FRAME
    );
}
