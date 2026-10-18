//! Сквозная проверка потоковых субтитров на настоящем HLS: loopback-сервер с MPEG-TS видео
//! (первый PTS не ноль) и WebVTT-сегментом с `X-TIMESTAMP-MAP` -> настоящее открытие
//! (`prepare_native`) -> intent -> `stream_subtitle_source()` -> владелец субтитров -> настоящая
//! лента и транспорт HLS -> настоящий рисовальщик -> GPU -> пиксели реплики на правильном времени
//! плеера (и отсутствие пикселей на неправильном).
//!
//! Что покрыто: вся цепочка от HTTP до пикселей, включая публикацию точки отсчёта времени
//! демуксером. Что НЕ покрыто: воспроизведение видео рабочим потоком плеера (часы плеера,
//! декодер) - вместо него владельцу сообщаются позиции, а время первого пакета видео читается
//! из настоящего демуксера того же открытия.

use std::time::Duration;

use super::super::stream_position::StreamTimelineKind;
use super::super::stream_track::subtitle_source_of_web_intent;
use super::ass_gpu_support::*;
use super::gpu_chain::Harness;
use super::stream_support::{displayed_active_texts, observe_position, wait_for_cues};
use super::web_support::click_menu_row;
use super::*;
use crate::media_open::open_hls_with_subtitles;

macro_rules! gpu_or_skip {
    () => {
        match Harness::with_size(WINDOW_WIDTH, WINDOW_HEIGHT) {
            Some(harness) => harness,
            None => {
                eprintln!("skip: no GPU adapter for the HLS subtitle vertical");
                return;
            }
        }
    };
}

/// Светлый пиксель текста: красный канал «видео» и затемнённой плашки ниже порога.
fn is_light(pixel: [u8; 4]) -> bool {
    pixel[0] > 150
}

fn has_ink(pixels: &[u8]) -> bool {
    bounds_of(pixels, WINDOW_WIDTH, is_light).is_some()
}

fn millis(time: Duration) -> u64 {
    u64::try_from(time.as_millis()).expect("test time fits")
}

#[test]
fn real_hls_webvtt_cue_lands_on_the_player_time_of_the_video() {
    let mut harness = gpu_or_skip!();
    let fixture = open_hls_with_subtitles();
    let media = instance(1);
    let source = subtitle_source_of_web_intent(&fixture.intent);
    assert!(
        matches!(source, SubtitleMediaSource::Stream(_)),
        "a native HLS open with a subtitle group hands out a stream source"
    );
    let mut owner = owner_with_config(&SubtitlesConfig::default());
    owner.begin_media(media, source);
    let first_video = fixture.first_video_player_time;
    observe_position(&mut owner, media, first_video, StreamTimelineKind::Vod);

    click_menu_row(&mut owner, "English (stream)");
    wait_for_cues(&mut owner, 1);

    // Карта: LOCAL 0 = тик первого кадра + 0,5 с; реплика [0,5; 1,5) от LOCAL, значит на шкале
    // плеера она идёт с (первый кадр + 1,0 с) до (первый кадр + 2,0 с).
    let cue_start = millis(first_video) + 1_000;
    let cue_end = millis(first_video) + 2_000;
    assert_eq!(
        displayed_active_texts(&owner, cue_start + 10),
        ["Mapped cue"],
        "the cue starts at the player time the map points to"
    );
    let just_before_start = render_placed(
        &mut owner,
        &mut harness,
        media,
        full_window_picture(),
        cue_start - 10,
    );
    let inside = render_placed(
        &mut owner,
        &mut harness,
        media,
        full_window_picture(),
        cue_start + 500,
    );
    let just_after_end = render_placed(
        &mut owner,
        &mut harness,
        media,
        full_window_picture(),
        cue_end + 10,
    );
    // Неправильные трактовки времени: LOCAL как время плеера (0,7 с) и сырое время потока
    // (метка первого кадра + 1,0 с = 2,42 с).
    let as_if_local_time =
        render_placed(&mut owner, &mut harness, media, full_window_picture(), 700);
    let as_if_raw_stream_time = render_placed(
        &mut owner,
        &mut harness,
        media,
        full_window_picture(),
        millis(first_video) + 1_500 + 1_000,
    );

    assert!(has_ink(&inside), "the cue is drawn at its player time");
    assert!(
        !has_ink(&just_before_start),
        "not 10 ms before the cue starts"
    );
    assert!(!has_ink(&just_after_end), "gone 10 ms after the cue ends");
    assert!(
        !has_ink(&as_if_local_time),
        "the WebVTT-local time is not the player time"
    );
    assert!(
        !has_ink(&as_if_raw_stream_time),
        "the raw MPEG-TS time is not the player time"
    );
}

#[test]
fn real_hls_default_rendition_is_selected_by_the_manifest_flags_and_delivers_its_cue() {
    // Манифест помечает английскую дорожку DEFAULT=YES, AUTOSELECT=YES, а настройка "субтитры
    // включены" - да: никакого клика, дорожка выбирается автовыбором и лента сама качает сегмент.
    let fixture = open_hls_with_subtitles();
    let media = instance(1);
    let mut owner = owner_with_config(&enabled_config());
    owner.begin_media(media, subtitle_source_of_web_intent(&fixture.intent));
    observe_position(&mut owner, media, Duration::ZERO, StreamTimelineKind::Vod);

    wait_for_cues(&mut owner, 1);

    assert_eq!(
        displayed_active_texts(&owner, millis(fixture.first_video_player_time) + 1_500),
        ["Mapped cue"]
    );
}
