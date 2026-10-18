//! Сквозная проверка потоковых субтитров на настоящем нативном DASH: loopback-сервер с fMP4
//! видео и двумя отдельными TTML-дорожками -> настоящее открытие (`prepare_native_dash_attempt`)
//! -> intent -> `stream_subtitle_source()` -> владелец субтитров -> настоящая лента и транспорт
//! DASH -> настоящий рисовальщик -> GPU -> пиксели реплики на правильном времени плеера (и
//! отсутствие пикселей на неправильном); для IMSC - чернила только в верхней части кадра.
//!
//! Фикстура открытия - `media_open/web/tests/stream_subtitles_dash_vertical.rs`.
//!
//! Что покрыто: вся цепочка от HTTP до пикселей. Что НЕ покрыто: воспроизведение видео рабочим
//! потоком плеера (часы плеера, декодер) - владельцу сообщаются позиции, а время первого пакета
//! видео читается из настоящего демуксера того же открытия.

use std::time::Duration;

use super::super::stream_position::StreamTimelineKind;
use super::super::stream_track::subtitle_source_of_web_intent;
use super::ass_gpu_support::*;
use super::gpu_chain::Harness;
use super::stream_support::{displayed_active_texts, observe_position, wait_for_cues};
use super::web_support::click_menu_row;
use super::*;
use crate::media_open::open_dash_with_subtitles;

macro_rules! gpu_or_skip {
    () => {
        match Harness::with_size(WINDOW_WIDTH, WINDOW_HEIGHT) {
            Some(harness) => harness,
            None => {
                eprintln!("skip: no GPU adapter for the DASH subtitle vertical");
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

#[test]
fn real_dash_ttml_cue_lands_on_the_player_time_of_the_video() {
    let mut harness = gpu_or_skip!();
    let fixture = open_dash_with_subtitles();
    let media = instance(1);
    let source = subtitle_source_of_web_intent(&fixture.intent);
    assert!(
        matches!(source, SubtitleMediaSource::Stream(_)),
        "a native DASH open with text sets hands out a stream source"
    );
    let mut owner = owner_with_config(&SubtitlesConfig::default());
    owner.begin_media(media, source);
    observe_position(
        &mut owner,
        media,
        fixture.first_video_player_time,
        StreamTimelineKind::Vod,
    );

    click_menu_row(&mut owner, "English (stream)");
    wait_for_cues(&mut owner, 1);

    // TTML отсчитывается от начала Period, как и видео: реплика [0,5; 1,5) с.
    assert_eq!(displayed_active_texts(&owner, 1_000), ["Plain DASH cue"]);
    let picture = full_window_picture;
    let before = render_placed(&mut owner, &mut harness, media, picture(), 400);
    let inside = render_placed(&mut owner, &mut harness, media, picture(), 1_000);
    let after = render_placed(&mut owner, &mut harness, media, picture(), 1_600);

    assert!(has_ink(&inside), "the cue is drawn at its player time");
    assert!(!has_ink(&before), "not before the cue starts");
    assert!(!has_ink(&after), "gone after the cue ends");
}

#[test]
fn real_dash_imsc_top_region_draws_ink_only_in_the_upper_part() {
    let mut harness = gpu_or_skip!();
    let fixture = open_dash_with_subtitles();
    let media = instance(1);
    let mut owner = owner_with_config(&SubtitlesConfig::default());
    owner.begin_media(media, subtitle_source_of_web_intent(&fixture.intent));
    observe_position(
        &mut owner,
        media,
        fixture.first_video_player_time,
        StreamTimelineKind::Vod,
    );

    click_menu_row(&mut owner, "German (stream)");
    wait_for_cues(&mut owner, 1);
    assert_eq!(displayed_active_texts(&owner, 1_000), ["Top DASH cue"]);

    let frame = render_placed(
        &mut owner,
        &mut harness,
        media,
        full_window_picture(),
        1_000,
    );
    let ink = bounds_of(&frame, WINDOW_WIDTH, is_light).expect("the cue is drawn");
    assert!(
        ink.max_y < WINDOW_HEIGHT / 2,
        "an IMSC cue in the top region must not be drawn at the bottom: {ink:?}"
    );
}

#[test]
fn real_dash_default_track_is_selected_by_the_main_role_and_delivers_its_cue() {
    // Роль main у английской дорожки + настройка "субтитры включены": без клика дорожка
    // выбирается автовыбором, и лента сама качает документ.
    let fixture = open_dash_with_subtitles();
    let media = instance(1);
    let mut owner = owner_with_config(&enabled_config());
    owner.begin_media(media, subtitle_source_of_web_intent(&fixture.intent));
    observe_position(&mut owner, media, Duration::ZERO, StreamTimelineKind::Vod);

    wait_for_cues(&mut owner, 1);

    assert_eq!(displayed_active_texts(&owner, 1_000), ["Plain DASH cue"]);
}
