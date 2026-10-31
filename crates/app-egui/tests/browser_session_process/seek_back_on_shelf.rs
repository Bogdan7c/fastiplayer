//! Сквозной тест «помним, пока помнит страница» (04a, живая находка): страница отдаёт
//! байты один раз и повторно их не шлёт (у её настоящего MSE `SourceBuffer` они ещё
//! лежат), а зритель/страница перематывается назад дальше окна `buffer_behind_s`.
//!
//! До решения 31.10.2026 полка держала прошлое только `buffer_behind_s`: перемотка
//! назад уходила к странице, страница перематывалась и не присылала ни байта — картинка
//! висела до срока перемотки плеера. Здесь окно «назад» — 0 с (config пользователя), а
//! страница перематывается на 0,2 с после 1,8 с просмотра: при старом правиле этого
//! времени на полке уже не было бы.
//!
//! Запись — первое поколение `quality-switch-new-media-source` (звук + видео H.264
//! 25 к/с, ключевые кадры раз в 1,2 с), без последующих смен качества.

use super::*;

/// Запись в фикстурах протокола.
const RECORDING: &str = "quality-switch-new-media-source";
/// Куда страница перематывается назад.
const PAGE_SEEK_TARGET_US: u64 = 200_000;
/// После перемотки часы «играет» обязаны пройти через цель (с запасом на шаг часов
/// 4/с и посадку на ключевой кадр с полки).
const LANDING_WINDOW_US: u64 = 400_000;
/// И пойти дальше по полке: секунда после цели — точно не случайный кадр посадки.
const PLAYED_ON_FROM_SHELF_US: u64 = 1_000_000;

#[test]
fn a_seek_back_past_the_behind_window_plays_from_the_shelf_without_the_page_resending() {
    seek_back_plays_from_the_shelf(VideoMode::Headless);
}

/// То же в окне браузерного режима (путь открытия `AppState`).
#[test]
fn a_seek_back_past_the_behind_window_plays_from_the_shelf_in_a_window() {
    seek_back_plays_from_the_shelf(VideoMode::Window);
}

/// Поднимает процесс с `config.toml`, где окно «назад» — 0 с.
fn start_with_zero_behind(mode: VideoMode) -> Option<SessionProcessUnderTest> {
    let mut config = fastiplayer_config::AppConfig::default();
    config.browser_session.buffer_behind_s = 0;
    let config_toml = config.to_pretty_toml().expect("config text");
    let name = "seek-back-on-shelf";
    let video = match mode {
        VideoMode::Headless => TestVideo::Headless,
        VideoMode::Window => TestVideo::Window(window::VirtualWaylandDisplay::start(name)?),
    };
    let bus_name = match mode {
        VideoMode::Headless => format!("{name}-headless"),
        VideoMode::Window => format!("{name}-window"),
    };
    let bus = PrivateBusWithBrowserSession::start_with(&bus_name, Some(&config_toml), &video);
    Some(SessionProcessUnderTest {
        bus,
        _video: video,
        mode,
    })
}

/// Позиции часов «играет» по порядку прихода.
fn playing_positions(messages: &[PlayerSessionMessage]) -> Vec<u64> {
    messages
        .iter()
        .filter_map(|message| match message {
            PlayerSessionMessage::Clock {
                position_us,
                state: ClockState::Playing,
                ..
            } => Some(position_us.as_micros()),
            _ => None,
        })
        .collect()
}

fn seek_back_plays_from_the_shelf(mode: VideoMode) {
    let Some(process) = start_with_zero_behind(mode) else {
        return;
    };
    let mut socket = process.open_attached_session();
    let (steps, _expected_bytes) = recorded_stream(RECORDING);

    // Первое поколение целиком (до первой паузы записи), и ни байта больше: страница
    // отдала это один раз.
    let mut await_clock_us = None;
    for step in steps {
        let message = match step {
            StreamStep::Text(text) => Message::text(text),
            StreamStep::Binary(bytes) => Message::binary(bytes),
            StreamStep::AwaitClock(position_us) => {
                await_clock_us = Some(position_us);
                break;
            }
        };
        socket.send(message).expect("stream step is sent");
    }
    let watched_until_us = await_clock_us.expect("the recording pauses after its first generation");
    read_player_messages_until(&mut socket, "the clock of the watched part", |message| {
        matches!(
            message,
            PlayerSessionMessage::Clock {
                position_us,
                state: ClockState::Playing,
                ..
            } if position_us.as_micros() >= watched_until_us
        )
    });

    // Страница перематывается назад сама и ничего не досылает.
    let page_seek = format!(
        r#"{{"type":"seek_started","target_us":{PAGE_SEEK_TARGET_US},"cause":{{"kind":"page_seek"}}}}"#
    );
    socket
        .send(Message::text(page_seek))
        .expect("page seek is sent");
    let messages = read_player_messages_until(
        &mut socket,
        "the clock playing on from the shelf after the seek back",
        |message| {
            matches!(
                message,
                PlayerSessionMessage::Clock {
                    position_us,
                    state: ClockState::Playing,
                    ..
                } if (PAGE_SEEK_TARGET_US + PLAYED_ON_FROM_SHELF_US
                    ..watched_until_us)
                    .contains(&position_us.as_micros())
            )
        },
    );
    let playing = playing_positions(&messages);
    assert!(
        playing.iter().any(|&position| {
            (PAGE_SEEK_TARGET_US..PAGE_SEEK_TARGET_US + LANDING_WINDOW_US).contains(&position)
        }),
        "the player did not land at the page seek target: {playing:?}"
    );
    // Просить страницу перемотаться было незачем: она сама туда ушла.
    assert!(
        !messages
            .iter()
            .any(|message| matches!(message, PlayerSessionMessage::Seek { .. })),
        "the player asked the page to seek: {messages:?}"
    );

    send_tab_gone_and_expect_close(&mut socket);
    let summary = process
        .bus
        .wait_for_log_line("Browser playback summary kind=final");
    assert!(log_number(&summary, "presented_frames") > 0, "{summary}");
    process
        .bus
        .wait_for_log_line("Browser session process finished");
}
