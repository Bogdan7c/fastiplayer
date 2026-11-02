//! Один SourceBuffer «видео + звук» (страница `mse-muxed`, сессия 09a моста) — до кадра.
//!
//! Страница кладёт в одну полосу fMP4, где в каждом фрагменте обе дорожки, а в `mdat`
//! сначала идут все кадры видео фрагмента, потом весь его звук. Демуксер эпизода читает
//! трубу только вперёд, а пакеты отдаёт по времени вперемешку (кадр, звук, кадр, …) —
//! значит, внутри `mdat` ему нужно возвращаться назад дальше, чем помнит окно чтения.
//! Найдено стендом: передача посреди просмотра (старт поколения на позиции страницы)
//! показывала один кадр и падала «out-of-bounds seek for a non-seekable stream».
//!
//! Фикстура `muxed-h264-aac-long-runs.mp4` (см. `fixtures/generate.sh`): 1,94 с, 25 к/с,
//! три фрагмента по 0,64 с (начала видео 0; 0,661; 1,301 с), видео ~80 КиБ на фрагмент.
//! Границы звука и видео у неё совпадают — расходящиеся границы проверяет
//! `muxed_boundary_overhang_to_frame.rs`. Настоящие декодеры, настоящее время.

#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::panic,
    reason = "integration-тест: падение с понятным сообщением и есть результат"
)]

mod support;

use std::time::Duration;

use support::muxed_page::{
    FRAME, MuxedPage, assert_audio_about, assert_ended_cleanly, assert_frames_run_to,
    assert_starts_at, frames_after_first_seek, frames_before_first_seek, open_at, open_from_start,
};
use support::playback::play_controlled;

/// Фикстура с длинными прогонами дорожек в `mdat`.
const FIXTURE: &str = "muxed-h264-aac-long-runs.mp4";
/// Длительность ролика (страница сообщает её как `duration` MSE).
const CLIP: Duration = Duration::from_micros(1_941_406);
/// Время последнего кадра ролика (первый кадр у ffmpeg чуть длиннее остальных).
const LAST_FRAME: Duration = Duration::from_micros(1_901_406);
/// Сколько кадров в ролике.
const FRAMES_IN_CLIP: usize = 48;
/// Начало второго фрагмента (ключевой кадр).
const SECOND_FRAGMENT: Duration = Duration::from_micros(661_406);
/// Допуск на длительность звука: старт декодера и последний неполный пакет AAC.
const AUDIO_TOLERANCE: Duration = Duration::from_millis(150);

/// Страница со всеми тремя фрагментами и концом потока.
fn page_with_everything() -> (MuxedPage, pushed_media_source::PushedStreamOpener) {
    let (mut page, opener) = MuxedPage::new(FIXTURE, CLIP);
    assert_eq!(
        page.media.fragments.len(),
        3,
        "the fixture has three fragments"
    );
    page.push_everything();
    (page, opener)
}

/// Muxed-поток с начала: все кадры и весь звук (без позиции и перемотки — та же
/// поломка, только раньше: уже второй кадр первого фрагмента лежит «позади» звука).
#[test]
fn a_muxed_stream_plays_every_frame_and_its_audio_from_the_start() {
    let (_page, mut opener) = page_with_everything();

    let report = play_controlled(open_from_start(&mut opener), |_control| {});

    assert_ended_cleanly(&report);
    assert_eq!(
        report.frames.len(),
        FRAMES_IN_CLIP,
        "every frame once: {:?}",
        report
            .frames
            .iter()
            .map(|frame| frame.pts)
            .collect::<Vec<_>>()
    );
    assert_starts_at(&report.frames, Duration::ZERO);
    assert_frames_run_to(&report.frames, LAST_FRAME);
    assert_audio_about(&report, CLIP, AUDIO_TOLERANCE);
}

/// Дефект стенда: поколение открыто сразу на позиции страницы внутри фрагмента
/// (передача посреди просмотра) — кадры и звук идут с цели до конца.
#[test]
fn a_muxed_stream_installed_at_the_page_position_starts_there() {
    let (page, mut opener) = page_with_everything();
    let target = Duration::from_millis(1_145);
    page.writer.note_page_seek(target);
    let prepared = open_at(&mut opener, target);

    let report = play_controlled(prepared, |_control| {});

    assert_ended_cleanly(&report);
    assert_starts_at(&report.frames, target);
    assert_frames_run_to(&report.frames, LAST_FRAME);
    assert_audio_about(&report, CLIP - target, AUDIO_TOLERANCE);
    assert_eq!(page.monitor.take_page_seek_request(), None);
}

/// Обычная перемотка плеером внутри muxed-эпизода (вперёд по полке, в середину
/// последнего фрагмента): посадка на цель, дальше кадры и звук до конца.
#[test]
fn a_muxed_stream_seeks_forward_on_the_shelf_after_the_start() {
    let (page, mut opener) = page_with_everything();
    let target = Duration::from_millis(1_500);
    let mut seek_position = None;

    let report = play_controlled(open_from_start(&mut opener), |control| {
        if seek_position.is_none() && control.observation().position >= FRAME * 5 {
            seek_position = Some(control.observation().position);
            control.seek(target);
        }
    });

    assert_ended_cleanly(&report);
    let after = frames_after_first_seek(&report);
    assert_starts_at(&after, target);
    assert_frames_run_to(&after, LAST_FRAME);
    assert_eq!(
        page.monitor.take_page_seek_request(),
        None,
        "an on-shelf seek must not contact the page"
    );
    // Звук: до перемотки + от цели до конца.
    let played_before = seek_position.expect("seeked");
    let expected_audio = played_before + (CLIP - target);
    assert!(
        report.audio_duration() + Duration::from_millis(300) >= expected_audio,
        "audio {:?}, expected at least about {expected_audio:?}",
        report.audio_duration()
    );
    assert!(report.audio_rms() > 0.05, "the audio is silent");
}

/// Перемотка назад внутри muxed-эпизода после старта с позиции страницы: со второго
/// фрагмента на начало первого (новый эпизод с самого начала полки).
#[test]
fn a_muxed_stream_seeks_back_after_starting_at_the_page_position() {
    let (page, mut opener) = page_with_everything();
    let start = SECOND_FRAGMENT + FRAME * 2;
    page.writer.note_page_seek(start);
    let prepared = open_at(&mut opener, start);
    let target = Duration::from_millis(300);
    let mut seeked = false;

    let report = play_controlled(prepared, |control| {
        if !seeked && control.observation().position >= start + FRAME * 3 {
            seeked = true;
            control.seek(target);
        }
    });

    assert!(seeked, "the player never moved past the start position");
    assert_ended_cleanly(&report);
    assert_starts_at(&frames_before_first_seek(&report), start);
    let after = frames_after_first_seek(&report);
    assert_starts_at(&after, target);
    assert_frames_run_to(&after, LAST_FRAME);
    assert!(report.audio_rms() > 0.05, "the audio is silent");
}
