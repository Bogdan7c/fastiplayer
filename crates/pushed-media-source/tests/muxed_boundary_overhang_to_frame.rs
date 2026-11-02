//! Полоса «видео + звук», у которой границы дорожек соседних фрагментов расходятся —
//! до кадра (сессия 09a моста, вариант A владельца: полка по дорожкам, как MSE).
//!
//! Кадр AAC длится 21 мс, и его граница не совпадает с границей видео: звук фрагмента
//! кончается на 5–16 мс позже начала видео следующего. У стенда `mse-muxed` расхождения
//! 5,3 / 10,6 / 15,9 мс — у фикстуры `file-h264-aac-fragmented.mp4` (4 с, 25 к/с,
//! десять фрагментов по ~0,4 с, видео + AAC) ровно те же. Раньше полка сравнивала
//! объединённые отрезки соседей: накрытие больше допуска 10 мс считалось повтором, и
//! следующий фрагмент выбрасывал предыдущий с полки. На стенде это давало вечный
//! Buffering у дыры (страница её не заполнит — у неё всё лежит) и перемотку назад,
//! которая просит страницу и не дожидается.
//!
//! Тут же — страница, которая кладёт видео и звук ОТДЕЛЬНЫМИ `appendBuffer` в один
//! SourceBuffer: фрагменты «только видео» и «только звук» одного времени.

#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::panic,
    reason = "integration-тест: падение с понятным сообщением и есть результат"
)]

mod support;

use std::time::Duration;

use pushed_media_source::{PushedStreamOpener, ShelfSeekOutlook};
use support::media::split_fragment_by_track;
use support::muxed_page::{
    MuxedPage, assert_audio_about, assert_ended_cleanly, assert_frames_run_to, assert_starts_at,
    frames_after_first_seek, open_at, open_from_start,
};
use support::playback::{PlaybackReport, play_controlled};

/// Фикстура с расходящимися границами дорожек.
const FIXTURE: &str = "file-h264-aac-fragmented.mp4";
/// Длительность ролика (страница сообщает её как `duration` MSE).
const CLIP: Duration = Duration::from_micros(4_021_406);
/// Время последнего кадра ролика.
const LAST_FRAME: Duration = Duration::from_micros(3_981_406);
/// Сколько кадров в ролике.
const FRAMES_IN_CLIP: usize = 100;
/// Цель внутри третьего фрагмента (0,821–1,221 с): его звук кончается на 10,6 мс позже
/// начала видео следующего, а сам он накрывает предыдущий на 15,9 мс.
const INSIDE_OVERHANGING_FRAGMENT: Duration = Duration::from_millis(1_000);
/// Допуск на длительность звука: старт декодера и последний неполный пакет AAC.
const AUDIO_TOLERANCE: Duration = Duration::from_millis(150);

/// Страница со всеми фрагментами и концом потока.
fn page_with_everything() -> (MuxedPage, PushedStreamOpener) {
    let (mut page, opener) = MuxedPage::new(FIXTURE, CLIP);
    assert_eq!(page.media.fragments.len(), 10, "ten fragments of ~0.4 s");
    page.push_everything();
    (page, opener)
}

/// Все кадры ролика по одному разу.
fn assert_every_frame_once(report: &PlaybackReport) {
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
}

/// Всё, что страница прислала, остаётся на полке: перемотка внутрь фрагмента,
/// накрывающего соседа звуком, садится с полки.
#[test]
fn a_fragment_overhanging_the_next_one_stays_on_the_shelf() {
    let (page, mut opener) = page_with_everything();
    let _player = open_from_start(&mut opener);

    assert_eq!(
        page.monitor.seek_outlook(INSIDE_OVERHANGING_FRAGMENT),
        ShelfSeekOutlook::FromShelf,
        "a fragment the page still holds fell off the shelf"
    );
}

/// Дефект стенда: страница прислала всё, поколение поставлено на её позицию до
/// подачи — плеер играет до конца, не встаёт в Buffering у «дыры» на стыке.
#[test]
fn a_stream_installed_at_the_page_position_plays_across_overhanging_boundaries() {
    let (page, mut opener) = page_with_everything();
    let start = Duration::from_millis(200);
    page.writer.note_page_seek(start);
    let prepared = open_at(&mut opener, start);

    let report = play_controlled(prepared, |_control| {});

    assert_ended_cleanly(&report);
    assert!(!report.buffered_after_start, "the player waited for data");
    assert_starts_at(&report.frames, start);
    assert_frames_run_to(&report.frames, LAST_FRAME);
    assert_audio_about(&report, CLIP - start, AUDIO_TOLERANCE);
    assert_eq!(page.monitor.take_page_seek_request(), None);
}

/// С начала до конца без застревания, затем перемотка назад в такой фрагмент — с
/// полки, без просьбы странице.
#[test]
fn a_seek_back_into_an_overhanging_fragment_lands_from_the_shelf() {
    let (page, mut opener) = page_with_everything();
    let mut seeked = false;

    let report = play_controlled(open_from_start(&mut opener), |control| {
        if !seeked && control.observation().position >= Duration::from_millis(2_600) {
            seeked = true;
            control.seek(INSIDE_OVERHANGING_FRAGMENT);
        }
    });

    assert!(seeked, "the player never reached the seek point");
    assert_ended_cleanly(&report);
    assert_starts_at(&report.frames, Duration::ZERO);
    let after = frames_after_first_seek(&report);
    assert_starts_at(&after, INSIDE_OVERHANGING_FRAGMENT);
    assert_frames_run_to(&after, LAST_FRAME);
    assert_eq!(
        page.monitor.take_page_seek_request(),
        None,
        "the page is never asked for what it already sent"
    );
    assert!(report.audio_rms() > 0.05, "the audio is silent");
}

/// Страница кладёт видео и звук отдельными фрагментами в один SourceBuffer (общий
/// init с двумя дорожками): «только видео 0–0,4 с», «только звук 0–0,43 с», … Звук
/// не заменяет видео того же времени (оба лежат на полке) и подаётся в тот же
/// эпизод: все кадры и весь звук.
#[test]
fn video_and_audio_appended_separately_into_one_source_buffer_play_together() {
    let (mut page, mut opener) = MuxedPage::new(FIXTURE, CLIP);
    let single_track_fragments: Vec<Vec<u8>> = page
        .media
        .fragments
        .iter()
        .flat_map(|fragment| split_fragment_by_track(fragment))
        .collect();
    assert_eq!(single_track_fragments.len(), 20, "video and audio of each");
    page.push_and_end(&single_track_fragments);
    assert_eq!(
        page.monitor.seek_outlook(INSIDE_OVERHANGING_FRAGMENT),
        ShelfSeekOutlook::FromShelf,
        "video and audio of the same time both stay on the shelf"
    );

    let report = play_controlled(open_from_start(&mut opener), |_control| {});

    assert_ended_cleanly(&report);
    assert_every_frame_once(&report);
    assert_frames_run_to(&report.frames, LAST_FRAME);
    assert_audio_about(&report, CLIP, AUDIO_TOLERANCE);
}

/// С начала: все кадры и весь звук, без ожидания данных на стыках. (Проходил и до
/// правила по дорожкам: фрагменты подавались в трубу раньше, чем следующий выбрасывал
/// их с полки, — это страховка от регрессии подачи, а не доказательство дефекта.)
#[test]
fn a_muxed_stream_with_overhanging_audio_plays_every_frame_from_the_start() {
    let (_page, mut opener) = page_with_everything();

    let report = play_controlled(open_from_start(&mut opener), |_control| {});

    assert_ended_cleanly(&report);
    assert!(!report.buffered_after_start, "the player waited for data");
    assert_every_frame_once(&report);
    assert_frames_run_to(&report.frames, LAST_FRAME);
    assert_audio_about(&report, CLIP, AUDIO_TOLERANCE);
}
