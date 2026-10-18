//! Сброс media и поток-рисовальщик: у потока не бывает цели без зеркала дорожки.
//!
//! Гонка, из-за которой плавал тест смены языка: при сбросе media владелец сначала снимал
//! шрифты (поток обнуляет запись о выданном и перерисовывает), и только потом отпускал зеркало.
//! Если поток просыпался между двумя командами, он рисовал мёртвую дорожку, а счётчик
//! `rasterizer_calls` считал и эту, уже ненужную, картинку.

use subtitle_core::SubtitleSyncOffset;
use subtitle_raster::CueSourceId;

use super::super::presentation::test_support::PumpMode;
use super::super::presentation::worker_link::SentCommandKind;
use super::render_thread::{
    GENERATION, HEIGHT, WIDTH, back_to_back_srt, owner_with_srt, presenter_with_track, shown_events,
};
use super::*;

const TWO_CUES: &str = "1\n00:00:01,000 --> 00:00:03,000\nOne\n";

/// Смена media отпускает зеркала ДО команды снятия шрифтов: порядок отправки детерминирован,
/// поэтому тест ловит возврат к старому порядку без всякого везения со временем.
#[test]
fn clearing_media_releases_track_mirrors_before_touching_the_fonts() {
    let media = instance(1);
    let (mut owner, _dir) = owner_with_srt(media, &back_to_back_srt(6));
    assert_eq!(shown_events(&mut owner, media, 1_500), vec![1]);
    let _before = owner.presenter.take_sent_command_log();

    owner.clear_media();

    let sent = owner.presenter.take_sent_command_log();
    let forget = sent
        .iter()
        .position(|kind| *kind == SentCommandKind::ForgetTrack)
        .expect("the mirror of the old media must be released");
    let clear_fonts = sent
        .iter()
        .position(|kind| *kind == SentCommandKind::ClearFonts)
        .expect("the attached fonts must be cleared");
    assert!(
        forget < clear_fonts,
        "the worker must lose the mirror before it is told to redraw: {sent:?}"
    );
}

/// Снятие шрифтов после отпускания зеркала не заставляет поток рисовать: цели без зеркала
/// рисовать не из чего.
#[test]
fn worker_does_not_draw_a_released_track_when_fonts_are_cleared_afterwards() {
    let source = CueSourceId::new(5);
    let (mut presenter, track) = presenter_with_track(source, TWO_CUES, SubtitleSyncOffset::ZERO);
    let rect = full_rect(WIDTH, HEIGHT);
    assert!(
        presenter
            .overlay_for_frame(&track, &frame(1_500, GENERATION, rect))
            .is_some()
    );
    presenter.set_pump_mode(PumpMode::FreeRunning);
    let drawn_before = presenter
        .rasterizer_statistics()
        .expect("worker")
        .rendered_frames;

    presenter.release_mirrored_tracks();
    presenter.clear_attached_fonts();

    let drawn_after = presenter
        .rasterizer_statistics()
        .expect("worker")
        .rendered_frames;
    assert_eq!(
        drawn_after, drawn_before,
        "the dead track must not be drawn"
    );
}

/// `rasterizer_calls` считает картинки, принятые UI: нарисованная для сменившихся входов и
/// выброшенная картинка попадает только в `dropped_outdated_pictures`.
#[test]
fn discarded_picture_is_not_counted_as_accepted_rasterization() {
    let source = CueSourceId::new(5);
    let (mut presenter, track) = presenter_with_track(source, TWO_CUES, SubtitleSyncOffset::ZERO);
    presenter.set_pump_mode(PumpMode::FreeRunning);
    let rect = full_rect(WIDTH, HEIGHT);
    presenter.overlay_for_frame(&track, &frame(1_500, GENERATION, rect));
    // Входы сменились, пока поток рисовал: его картинка уже никому не нужна.
    presenter.forget_shown_overlay();
    assert!(presenter.wait_for_idle());

    let stats = presenter.stats();
    let drawn = presenter
        .rasterizer_statistics()
        .expect("worker")
        .rendered_frames;
    assert!(drawn >= 1, "the worker did draw for the old inputs");
    assert!(stats.dropped_outdated_pictures >= 1);
    assert_eq!(stats.rasterizer_calls, 0, "nothing was accepted by the UI");
}
