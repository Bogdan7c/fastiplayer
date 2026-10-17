//! Регрессионные тесты потока-рисовальщика: перемотка назад, зацикливание, гонки дописывания,
//! смена media. Помощники общие с `render_thread`.

use std::time::Duration;

use subtitle_core::SubtitleSyncOffset;
use subtitle_raster::CueSourceId;

use super::super::SubtitleOwner;
use super::super::presentation::test_support::PumpMode;
use super::super::render_worker::clock_tracker::ObservedMode;
use super::render_thread::{
    FRAME_MS, GENERATION, HEIGHT, WIDTH, back_to_back_srt, cue_events, free_running, observe,
    owner_with_srt, presenter_with_track, shown_events, subtitle_event,
};
use super::*;

/// Разбирает `wait_for_idle` на понятный провал: поток зациклился и не приходит в простой.
fn assert_worker_becomes_idle(owner: &mut SubtitleOwner, what: &str) {
    assert!(
        owner.presenter.wait_for_idle(),
        "the render thread must become idle after {what} (it is spinning)"
    );
}

fn warm_up_playback_at(owner: &mut SubtitleOwner, media: MediaInstanceId, start_ms: u64) {
    for index in 0..8_u64 {
        shown_events(owner, media, start_ms + index * FRAME_MS);
        assert!(owner.presenter.wait_for_idle());
    }
}

#[test]
fn backward_seek_leaves_the_worker_idle_and_the_first_frame_shows_its_cue() {
    let media = instance(1);
    let (mut owner, _dir) = owner_with_srt(media, &back_to_back_srt(12));
    free_running(&mut owner);
    warm_up_playback_at(&mut owner, media, 6_000);
    let late_before = owner.presentation_stats().late_pictures;

    observe(
        &mut owner,
        media,
        ObservedMode::Seeking {
            target: Duration::from_millis(500),
        },
    );
    assert_worker_becomes_idle(&mut owner, "a backward seek");
    let diagnostics = owner.presenter.worker_diagnostics().expect("worker");
    assert_eq!(diagnostics.repeated_jobs_suppressed, 0);
    assert!(
        diagnostics.raster.rendered_frames <= 12,
        "bounded work, drew {} pictures",
        diagnostics.raster.rendered_frames
    );

    observe(&mut owner, media, ObservedMode::Playing);
    let first_frame = shown_events(&mut owner, media, 540);

    assert_eq!(
        first_frame,
        vec![0],
        "the target picture must still be there"
    );
    assert_eq!(owner.presentation_stats().late_pictures, late_before);
}

#[test]
fn backward_scrub_series_leaves_the_worker_idle_and_the_last_target_is_shown() {
    let media = instance(1);
    let (mut owner, _dir) = owner_with_srt(media, &back_to_back_srt(12));
    free_running(&mut owner);
    warm_up_playback_at(&mut owner, media, 6_000);

    for target_ms in [5_000, 3_000, 2_000, 500] {
        observe(
            &mut owner,
            media,
            ObservedMode::Scrubbing {
                target: Duration::from_millis(target_ms),
            },
        );
    }
    assert_worker_becomes_idle(&mut owner, "a backward scrub");
    let diagnostics = owner.presenter.worker_diagnostics().expect("worker");
    assert_eq!(diagnostics.repeated_jobs_suppressed, 0);

    let late_before = owner.presentation_stats().late_pictures;
    let shown = shown_events(&mut owner, media, 520);

    assert_eq!(shown, vec![0]);
    assert_eq!(owner.presentation_stats().late_pictures, late_before);
}

#[test]
fn cues_shorter_than_a_frame_step_and_a_step_change_never_make_the_worker_spin() {
    // Реплики по 30 мс каждые 100 мс: шаги 40 и 60 мс попадают то в реплику, то в паузу.
    let mut srt = String::new();
    for index in 0..60_u32 {
        let start = index * 100;
        srt.push_str(&format!(
            "{}\n00:00:{:02},{:03} --> 00:00:{:02},{:03}\nTick {index}\n\n",
            index + 1,
            start / 1_000,
            start % 1_000,
            (start + 30) / 1_000,
            (start + 30) % 1_000
        ));
    }
    let media = instance(1);
    let (mut owner, _dir) = owner_with_srt(media, &srt);
    free_running(&mut owner);

    let mut pts_ms = 0_u64;
    for step in [40_u64, 60] {
        for _ in 0..60 {
            shown_events(&mut owner, media, pts_ms);
            assert_worker_becomes_idle(&mut owner, "a frame");
            pts_ms += step;
        }
    }

    let diagnostics = owner.presenter.worker_diagnostics().expect("worker");
    assert_eq!(diagnostics.repeated_jobs_suppressed, 0);
    assert!(
        diagnostics.raster.rendered_frames < 400,
        "bounded work, drew {} pictures",
        diagnostics.raster.rendered_frames
    );
}

#[test]
fn replaced_track_with_the_same_source_never_reuses_the_old_picture() {
    let source = CueSourceId::new(5);
    let (mut presenter, original) =
        presenter_with_track(source, TWO_CUE_SRT, SubtitleSyncOffset::ZERO);
    let (_other, replacement) = presenter_with_track(
        source,
        "1\n00:00:01,000 --> 00:00:03,000\nA different text\n",
        SubtitleSyncOffset::ZERO,
    );
    let rect = full_rect(WIDTH, HEIGHT);
    let first = presenter
        .overlay_for_frame(&original, &frame(2_000, GENERATION, rect))
        .cloned()
        .expect("original picture");
    assert_eq!(presenter.stats().rasterizer_calls, 1);

    // Та же дорожка (тот же источник), но событий меньше: полная пересинхронизация.
    let second = presenter
        .overlay_for_frame(&replacement, &frame(2_000, GENERATION, rect))
        .cloned()
        .expect("replacement picture");

    assert_eq!(
        presenter.stats().rasterizer_calls,
        2,
        "drawn again, not reused"
    );
    // Растеризатор ключует кэш источником и событием, поэтому содержимое картинки у той же
    // пары совпадёт; важно, что UI и поток не посчитали старую картинку действительной.
    assert_eq!(first.canvas(), second.canvas());
}

#[test]
fn picture_drawn_before_a_late_append_is_dropped_when_it_arrives_after_the_local_invalidation() {
    let source = CueSourceId::new(5);
    let (mut presenter, mut track) =
        presenter_with_track(source, TWO_CUE_SRT, SubtitleSyncOffset::ZERO);
    let rect = full_rect(WIDTH, HEIGHT);
    presenter.set_pump_mode(PumpMode::FreeRunning);
    presenter.set_render_delay(Duration::from_millis(300));

    // Поток начинает рисовать картинку [2 с, 3 с) по старому состоянию дорожки...
    presenter.overlay_for_frame(&track, &frame(2_000, GENERATION, rect));
    // Даём потоку войти в рисование (оно длится 300 мс), чтобы дописывание гарантированно
    // пришло ПОСЛЕ начала рисования по старому состоянию.
    std::thread::sleep(Duration::from_millis(100));
    // ...а UI уже узнал о дописанной реплике внутри этого срока.
    track.append_events([subtitle_event(2_500, 2_800, "Late arrival")]);
    presenter.overlay_for_frame(&track, &frame(2_600, GENERATION, rect));
    assert!(presenter.wait_for_idle());
    let shown = presenter
        .overlay_for_frame(&track, &frame(2_600, GENERATION, rect))
        .map(cue_events)
        .expect("overlay");

    assert_eq!(
        shown,
        vec![0, 2],
        "the stale in-flight picture must never be selected"
    );
    assert!(presenter.stats().dropped_outdated_pictures >= 1);
}

#[test]
fn media_change_forgets_the_previous_canvas_step_and_clock() {
    let source = CueSourceId::new(5);
    let (mut presenter, track) =
        presenter_with_track(source, TWO_CUE_SRT, SubtitleSyncOffset::ZERO);
    let rect = full_rect(WIDTH, HEIGHT);
    for pts_ms in [2_000, 2_040, 2_080] {
        presenter.overlay_for_frame(&track, &frame(pts_ms, GENERATION, rect));
    }
    assert!(presenter.remembers_playback_context());

    presenter.release_mirrored_tracks();

    assert!(!presenter.remembers_playback_context());
}

#[test]
fn attached_fonts_commands_carry_the_new_inputs_so_no_picture_is_drawn_with_the_old_revision() {
    let source = CueSourceId::new(5);
    let (mut presenter, track) =
        presenter_with_track(source, TWO_CUE_SRT, SubtitleSyncOffset::ZERO);
    let rect = full_rect(WIDTH, HEIGHT);
    assert!(
        presenter
            .overlay_for_frame(&track, &frame(2_000, GENERATION, rect))
            .is_some()
    );

    presenter.clear_attached_fonts();
    assert!(presenter.wait_for_idle());

    assert_eq!(
        presenter.stats().dropped_outdated_pictures,
        0,
        "the worker must not spend a frame on the old revision"
    );
}
