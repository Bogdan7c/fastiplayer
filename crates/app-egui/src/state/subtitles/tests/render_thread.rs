//! Функциональные тесты потока-рисовальщика: настоящий поток, настоящая растеризация.
//!
//! Проверяется то, что видит UI: какая картинка досталась кадру, были ли опоздания, не ждёт ли
//! UI поток, что происходит при seek, scrub, паузе, смене дорожки, гибели потока.

use std::time::{Duration, Instant};

use subtitle_core::SubtitleSyncOffset;
use subtitle_raster::{CueSourceId, SubtitleOverlay};

use super::super::active_track::ActiveSubtitleTrack;
use super::super::presentation::test_support::PumpMode;
use super::super::presentation::{PresenterSettings, SubtitlePresenter};
use super::super::render_worker::clock_tracker::{ObservedMode, PlaybackObservation};
use super::super::render_worker::schedule::PlaybackRate;
use super::super::settings::SubtitleSettings;
use super::super::sidecar_job::{FontLibraryPlan, SubtitleLoadFailure, run_sidecar_job};
use super::super::user_style::UserSubtitleStyle;
use super::super::{SubtitleLifecycleView, SubtitleOwner};
use super::*;
use crate::app_wake::{AppWakeOwner, AppWakePort};

pub(super) const WIDTH: u32 = 1280;
pub(super) const HEIGHT: u32 = 720;
pub(super) const GENERATION: u64 = 3;
/// Шаг кадров 25 к/с.
pub(super) const FRAME_MS: u64 = 40;
pub(super) const WAIT: Duration = Duration::from_secs(20);

/// SRT из `count` встык идущих секундных реплик: реплика `n` длится `[n, n + 1)` секунд.
pub(super) fn back_to_back_srt(count: u32) -> String {
    let mut srt = String::new();
    for index in 0..count {
        srt.push_str(&format!(
            "{}\n00:00:{:02},000 --> 00:00:{:02},000\nCue number {index}\n\n",
            index + 1,
            index,
            index + 1
        ));
    }
    srt
}

/// Владелец с загруженной дорожкой из `srt`.
pub(super) fn owner_with_srt(
    media: MediaInstanceId,
    srt: &str,
) -> (SubtitleOwner, tempfile::TempDir) {
    let directory = tempfile::tempdir().expect("tempdir");
    let video = write_movie(directory.path(), Some(srt));
    let mut owner = owner();
    begin_and_wait(&mut owner, media, &video);
    assert_eq!(owner.lifecycle_view(), SubtitleLifecycleView::Ready);
    (owner, directory)
}

/// Идентификаторы реплик, которым принадлежат картинки overlay (без повторов, по порядку).
pub(super) fn cue_events(overlay: &SubtitleOverlay) -> Vec<u32> {
    let mut events: Vec<u32> = overlay
        .bitmaps()
        .iter()
        .filter_map(|bitmap| bitmap.cue_key())
        .map(|key| key.event().get())
        .collect();
    events.dedup();
    events
}

/// Кадр, показанный на `pts_ms`: идентификаторы реплик его overlay (пусто — overlay нет).
pub(super) fn shown_events(
    owner: &mut SubtitleOwner,
    media: MediaInstanceId,
    pts_ms: u64,
) -> Vec<u32> {
    let rect = full_rect(WIDTH, HEIGHT);
    owner
        .overlay_for_frame(current(media, GENERATION), &frame(pts_ms, GENERATION, rect))
        .map(cue_events)
        .unwrap_or_default()
}

pub(super) fn free_running(owner: &mut SubtitleOwner) {
    owner.presenter.set_pump_mode(PumpMode::FreeRunning);
}

pub(super) fn observe(owner: &mut SubtitleOwner, media: MediaInstanceId, mode: ObservedMode) {
    owner.observe_playback(
        current(media, GENERATION),
        PlaybackObservation {
            mode,
            rate: PlaybackRate::NORMAL,
            estimated_step: None,
            position: None,
            read_run: player_core::SubtitleReadRun::INITIAL,
        },
    );
}

#[test]
fn steady_playback_shows_each_frames_own_cue_without_a_single_late_picture() {
    let media = instance(1);
    let (mut owner, _dir) = owner_with_srt(media, &back_to_back_srt(12));
    free_running(&mut owner);

    // Прогрев: самый первый кадр холодный (картинки ещё нет), дальше поток впереди.
    shown_events(&mut owner, media, 0);
    assert!(owner.presenter.wait_for_idle());
    let late_after_warm_up = owner.presentation_stats().late_pictures;

    // 12 секунд кадрами по 40 мс. Между кадрами поток успевает всё (как в жизни: у него 40 мс).
    let mut frames_with_cue = 0;
    for index in 0..300_u64 {
        let pts_ms = index * FRAME_MS;
        let shown = shown_events(&mut owner, media, pts_ms);
        let expected_cue = pts_ms / 1_000;
        if expected_cue < 12 {
            assert_eq!(
                shown,
                vec![u32::try_from(expected_cue).expect("small")],
                "frame at {pts_ms} ms must show exactly its own cue (cue changes land on the frame)"
            );
            frames_with_cue += 1;
        }
        assert!(owner.presenter.wait_for_idle());
    }
    assert_eq!(frames_with_cue, 300);
    // Опозданий нет вообще: даже на кадрах, где реплика меняется.
    assert_eq!(
        owner.presentation_stats().late_pictures,
        late_after_warm_up,
        "the worker stays ahead of steady playback"
    );
}

#[test]
fn static_scene_is_rasterized_once_over_a_hundred_frames() {
    let media = instance(1);
    let long_cue = "1\n00:00:00,000 --> 00:59:00,000\nOne long cue\n";
    let (mut owner, _dir) = owner_with_srt(media, long_cue);

    for index in 0..100_u64 {
        assert_eq!(
            shown_events(&mut owner, media, 1_000 + index * FRAME_MS),
            vec![0]
        );
    }

    let statistics = owner
        .rasterizer_statistics()
        .expect("the worker owns a rasterizer");
    assert_eq!(
        statistics.rasterized_cues, 1,
        "a static scene is drawn once"
    );
    assert_eq!(statistics.rendered_frames, 1);
    assert_eq!(owner.presentation_stats().rasterizer_calls, 1);
}

#[test]
fn lookahead_prepares_the_next_cue_before_its_frame() {
    let media = instance(1);
    let (mut owner, _dir) = owner_with_srt(media, TWO_CUE_SRT);
    free_running(&mut owner);

    // Кадры по 40 мс подходят к границе второй реплики (5 с): поток готовит её заранее.
    for pts_ms in (4_600..5_000).step_by(40) {
        shown_events(&mut owner, media, pts_ms);
        assert!(owner.presenter.wait_for_idle());
    }
    let late_before = owner.presentation_stats().late_pictures;
    let first_frame_of_second_cue = shown_events(&mut owner, media, 5_000);

    assert_eq!(first_frame_of_second_cue, vec![1]);
    assert_eq!(owner.presentation_stats().late_pictures, late_before);
}

#[test]
fn seek_target_is_rendered_before_the_first_frame_after_the_seek() {
    let media = instance(1);
    let (mut owner, _dir) = owner_with_srt(media, &back_to_back_srt(12));
    free_running(&mut owner);
    for index in 0..10_u64 {
        shown_events(&mut owner, media, 1_000 + index * FRAME_MS);
        assert!(owner.presenter.wait_for_idle());
    }
    let late_before = owner.presentation_stats().late_pictures;

    // Seek в 6-ю секунду: видео-кадров ещё нет (на экране старый), цель уже известна.
    observe(
        &mut owner,
        media,
        ObservedMode::Seeking {
            target: Duration::from_millis(6_000),
        },
    );
    assert!(owner.presenter.wait_for_idle());
    observe(&mut owner, media, ObservedMode::Playing);
    let first_frame_after_seek = shown_events(&mut owner, media, 6_040);

    assert_eq!(first_frame_after_seek, vec![6]);
    assert_eq!(
        owner.presentation_stats().late_pictures,
        late_before,
        "the picture was ready when the first frame arrived"
    );
}

#[test]
fn without_the_target_hint_the_first_frame_after_a_seek_is_late_and_shows_nothing_stale() {
    let media = instance(1);
    let (mut owner, _dir) = owner_with_srt(media, &back_to_back_srt(12));
    free_running(&mut owner);
    for index in 0..10_u64 {
        shown_events(&mut owner, media, 1_000 + index * FRAME_MS);
        assert!(owner.presenter.wait_for_idle());
    }
    owner.presenter.set_render_delay(Duration::from_millis(200));
    assert!(owner.presenter.wait_for_idle());
    let late_before = owner.presentation_stats().late_pictures;

    // Seek без подсказки о цели: первый кадр опаздывает, а старая реплика (1 с) не залипает.
    let started = Instant::now();
    let shown = shown_events(&mut owner, media, 6_040);

    assert!(
        shown.is_empty(),
        "the ended cue must not be shown, got {shown:?}"
    );
    assert!(
        started.elapsed() < Duration::from_millis(150),
        "the UI never waits for the render thread"
    );
    assert_eq!(owner.presentation_stats().late_pictures, late_before + 1);
}

#[test]
fn scrub_drops_outdated_targets_and_the_last_target_is_shown() {
    let media = instance(1);
    let (mut owner, _dir) = owner_with_srt(media, &back_to_back_srt(12));
    free_running(&mut owner);
    shown_events(&mut owner, media, 500);
    owner.presenter.set_render_delay(Duration::from_millis(100));
    shown_events(&mut owner, media, 540);

    for target_ms in [1_500, 3_500, 5_500, 8_500] {
        observe(
            &mut owner,
            media,
            ObservedMode::Scrubbing {
                target: Duration::from_millis(target_ms),
            },
        );
    }
    assert!(owner.presenter.wait_for_idle());
    let shown = shown_events(&mut owner, media, 8_520);

    assert_eq!(
        shown,
        vec![8],
        "the picture of the final scrub target is shown"
    );
    let rendered = owner
        .rasterizer_statistics()
        .expect("rasterizer")
        .rendered_frames;
    assert!(
        rendered <= 3,
        "outdated scrub targets are not drawn (drawn {rendered} pictures)"
    );
}

#[test]
fn paused_frame_gets_a_redraw_request_when_its_picture_arrives() {
    let media = instance(1);
    let (mut owner, _dir) = owner_with_srt(media, TWO_CUE_SRT);
    free_running(&mut owner);
    observe(&mut owner, media, ObservedMode::Paused);
    owner.presenter.set_render_delay(Duration::from_millis(100));
    assert!(owner.presenter.wait_for_idle());

    let first = shown_events(&mut owner, media, 2_000);
    assert!(first.is_empty(), "the picture is not ready yet");

    let deadline = Instant::now() + WAIT;
    let mut redraw_requested = false;
    while Instant::now() < deadline && !redraw_requested {
        redraw_requested = owner.poll_jobs();
        std::thread::sleep(Duration::from_millis(5));
    }

    assert!(
        redraw_requested,
        "polling must ask for a redraw of the paused frame"
    );
    assert_eq!(shown_events(&mut owner, media, 2_000), vec![0]);
    assert_eq!(
        owner.presenter.stats().rasterizer_calls,
        1,
        "paused: only the current frame is drawn"
    );
}

#[test]
fn ui_does_not_wait_for_a_busy_worker_and_shows_the_picture_without_ended_cues() {
    // Реплика 0 живёт [0, 6) с, реплика 1 - [4, 10) с: на 5 с видны обе, на 7 с - только вторая.
    let overlapping = "1\n00:00:00,000 --> 00:00:06,000\nFirst\n\n\
                       2\n00:00:04,000 --> 00:00:10,000\nSecond\n";
    let media = instance(1);
    let (mut owner, _dir) = owner_with_srt(media, overlapping);
    assert_eq!(shown_events(&mut owner, media, 5_000), vec![0, 1]);
    free_running(&mut owner);
    owner.presenter.set_render_delay(Duration::from_millis(400));

    let started = Instant::now();
    let shown = shown_events(&mut owner, media, 7_000);

    assert!(
        started.elapsed() < Duration::from_millis(200),
        "selection must not wait for the busy worker"
    );
    assert_eq!(
        shown,
        vec![1],
        "the first cue has ended and must be cut from the old picture"
    );
    let counters = owner.presenter.queue_counters();
    assert_eq!(counters.stale_fallbacks, 1);
}

#[test]
fn late_appended_cue_invalidates_the_pictures_it_overlaps() {
    let source = CueSourceId::new(5);
    let (mut presenter, track) =
        presenter_with_track(source, TWO_CUE_SRT, SubtitleSyncOffset::ZERO);
    let mut track = track;
    let rect = full_rect(WIDTH, HEIGHT);

    // Картинка первой реплики годна до 3 с.
    let first = presenter
        .overlay_for_frame(&track, &frame(2_000, GENERATION, rect))
        .map(cue_events)
        .expect("first cue");
    assert_eq!(first, vec![0]);

    // Встроенная дорожка дописала реплику внутри срока годности той картинки.
    let appended = subtitle_event(2_500, 2_800, "Late arrival");
    track.append_events([appended]);
    let shown = presenter
        .overlay_for_frame(&track, &frame(2_600, GENERATION, rect))
        .map(cue_events)
        .expect("overlay with the late cue");

    assert_eq!(
        shown,
        vec![0, 2],
        "the old picture must not be reused over the new cue"
    );
}

#[test]
fn another_track_never_shows_the_picture_of_the_previous_one() {
    let first_source = CueSourceId::new(1);
    let (mut presenter, first_track) =
        presenter_with_track(first_source, TWO_CUE_SRT, SubtitleSyncOffset::ZERO);
    let (_other_presenter, second_track) =
        presenter_with_track(CueSourceId::new(2), TWO_CUE_SRT, SubtitleSyncOffset::ZERO);
    let rect = full_rect(WIDTH, HEIGHT);
    assert!(
        presenter
            .overlay_for_frame(&first_track, &frame(2_000, GENERATION, rect))
            .is_some()
    );
    presenter.set_pump_mode(PumpMode::FreeRunning);
    presenter.set_render_delay(Duration::from_millis(300));

    let other = presenter.overlay_for_frame(&second_track, &frame(2_000, GENERATION, rect));

    assert!(
        other.is_none(),
        "the old track's picture must never leak into the new track"
    );
}

#[test]
fn dropping_the_presenter_stops_the_render_thread() {
    let source = CueSourceId::new(5);
    let (mut presenter, track) =
        presenter_with_track(source, TWO_CUE_SRT, SubtitleSyncOffset::ZERO);
    let rect = full_rect(WIDTH, HEIGHT);
    assert!(
        presenter
            .overlay_for_frame(&track, &frame(2_000, GENERATION, rect))
            .is_some()
    );
    assert!(presenter.worker_is_running());

    presenter.discard_rasterizer();

    assert!(!presenter.has_rasterizer());
    let deadline = Instant::now() + WAIT;
    while presenter.retiring_workers() > 0 {
        assert!(Instant::now() < deadline, "the stopped thread must finish");
        presenter.poll_worker();
        std::thread::sleep(Duration::from_millis(2));
    }
    // Прежний overlay не просачивается после остановки.
    assert!(
        presenter
            .overlay_for_frame(&track, &frame(2_000, GENERATION, rect))
            .is_none()
    );
}

#[test]
fn worker_death_makes_the_track_unavailable_with_a_reason_and_never_panics() {
    let media = instance(1);
    let (mut owner, _dir) = owner_with_srt(media, TWO_CUE_SRT);
    assert_eq!(shown_events(&mut owner, media, 2_000), vec![0]);

    owner.presenter.crash_worker_for_test();
    let deadline = Instant::now() + WAIT;
    let mut redraw = false;
    while Instant::now() < deadline && !redraw {
        redraw = owner.poll_jobs();
        std::thread::sleep(Duration::from_millis(5));
    }

    assert!(redraw, "the failure must reach the owner");
    assert_eq!(owner.lifecycle_view(), SubtitleLifecycleView::Unavailable);
    assert!(matches!(
        owner.unavailable_failure(),
        Some(SubtitleLoadFailure::RenderWorkerLost)
    ));
    assert!(
        shown_events(&mut owner, media, 2_000).is_empty(),
        "no picture and no panic after the thread is gone"
    );
}

#[test]
fn worker_start_failure_is_reported_as_an_english_reason() {
    let media = instance(1);
    let (mut owner, _dir) = owner_with_srt(media, TWO_CUE_SRT);

    owner.mark_display_track_unavailable_after(
        super::super::render_worker::RenderWorkerFailure::StartFailed {
            kind: std::io::ErrorKind::OutOfMemory,
        },
    );

    assert_eq!(owner.lifecycle_view(), SubtitleLifecycleView::Unavailable);
    let reason = owner.unavailable_failure().expect("reason").to_string();
    assert!(reason.contains("could not be started"), "got: {reason}");
}

/// Презентатор с настоящим потоком и дорожкой из `srt`.
pub(super) fn presenter_with_track(
    source: CueSourceId,
    srt: &str,
    offset: SubtitleSyncOffset,
) -> (SubtitlePresenter, ActiveSubtitleTrack) {
    let directory = tempfile::tempdir().expect("tempdir");
    let video = write_movie(directory.path(), Some(srt));
    let request = discovery_request(&video, FontLibraryPlan::BuildEmbeddedOnly);
    let discovery = expect_discovery(run_sidecar_job(&request, &|| false));
    let timeline = discovery
        .preloaded
        .expect("track is read")
        .result
        .expect("track must load")
        .timeline;
    let config = fastiplayer_config::SubtitlesConfig::default();
    let settings: PresenterSettings = SubtitleSettings::from_config(&config).presenter_settings();
    let mut presenter = SubtitlePresenter::new(
        None,
        &UserSubtitleStyle::from_config(&config),
        settings,
        AppWakePort::disconnected(AppWakeOwner::Subtitles),
    );
    presenter.install_font_library(discovery.fonts.expect("fonts requested"));
    (
        presenter,
        ActiveSubtitleTrack::new(source, timeline, offset, None),
    )
}

/// Простая реплика для дописывания в дорожку.
pub(super) fn subtitle_event(
    start_ms: i64,
    end_ms: i64,
    text: &str,
) -> subtitle_core::SubtitleEvent {
    use subtitle_core::{
        CuePlacement, SpanStyle, StyledText, SubtitleContent, SubtitleEvent, SubtitleLayer,
        SubtitleTextCue, SubtitleTime, TextSpan,
    };
    let cue = SubtitleTextCue {
        text: StyledText::new(vec![TextSpan::text(text, SpanStyle::default())]),
        placement: CuePlacement::default(),
    };
    SubtitleEvent::new(
        SubtitleTime::from_millis(start_ms),
        SubtitleTime::from_millis(end_ms),
        SubtitleLayer::DEFAULT,
        SubtitleContent::Text(cue),
    )
    .expect("valid event")
}
