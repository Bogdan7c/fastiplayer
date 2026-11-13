//! Шкала потока меняется на ходу (мост 16d1, решения 318–320): настоящий процесс.
//!
//! Зонд 16d1 показал на коде до сессии:
//! - эфир MSE, который страница сделала записью (бесконечная длительность → конечная →
//!   `endOfStream()`, как у hls.js, Shaka, dash.js), доигрывался, но окно оставалось эфиром:
//!   без длины в MPRIS и с меткой LIVE;
//! - перемотка страницы за первую объявленную длительность срезалась до неё (страница
//!   объявила 3 с, цель 4 с — плеер встал на 3 с), даже когда байты дальше уже лежали.
//!
//! Тесты доказывают результат: длину в MPRIS окна, приземление перемотки по часам плеера
//! и кадры, дошедшие до показа после смены шкалы.

use super::next_episode::{mpris_length_us, wait_for_mpris_length_near};
use super::session_ends::{send_all, six_seconds_downloaded, wait_until_playing_past};
use super::*;

/// Эфир: длительность бесконечна (так её называет экстеншен для `+Infinity`).
pub(super) const LIVE_DURATION: &str =
    r#"{"type":"duration_changed","generation":1,"duration":{"kind":"live"}}"#;
/// Эфир кончился и стал записью: страница назвала конечную длительность — 6 с, весь ролик.
pub(super) const RECORDING_DURATION: &str = r#"{"type":"duration_changed","generation":1,"duration":{"kind":"finite","duration_us":6000000}}"#;
/// Страница поставила `endOfStream()` после конечной длительности.
pub(super) const END_OF_STREAM: &str = r#"{"type":"end_of_stream","generation":1}"#;
/// Первая длительность, которую назвала страница: меньше скачанного (как у эфира YouTube,
/// чья длительность растёт на ходу).
const FIRST_DURATION: &str = r#"{"type":"duration_changed","generation":1,"duration":{"kind":"finite","duration_us":3000000}}"#;

/// Длина 6-секундного ролика фикстуры в MPRIS (мкс).
pub(super) const RECORDING_LENGTH_US: u64 = 6_000_000;
/// Кадров в 6-секундном ролике фикстуры (25 к/с) минус пара кадров у краёв.
const RECORDING_FRAMES_AT_LEAST: u64 = 148;
/// Цель перемотки страницы — за первой длительностью (3 с), внутри скачанного (6 с).
const SEEK_PAST_FIRST_DURATION_US: u64 = 4_000_000;
/// Допуск приземления: первый тик часов после перемотки (тик раз в 250 мс).
const LANDING_SLACK_US: u64 = 400_000;

/// Ролик 6 с одним куском на полосу, объявленный длительностью `duration_message`.
pub(super) fn six_seconds_announced_as(duration_message: &'static str) -> Vec<Message> {
    let mut messages = six_seconds_downloaded();
    // Второе сообщение фикстуры — её длительность; заменяем словом страницы из теста.
    messages[1] = Message::text(duration_message);
    messages
}

#[test]
fn a_live_stream_that_became_a_recording_gets_a_length_and_plays_to_its_end() {
    let Some(process) = SessionProcessUnderTest::start("live-becomes-recording", VideoMode::Window)
    else {
        return;
    };
    let bus = &process.bus;
    let mut socket = process.open_attached_session();
    send_all(&mut socket, six_seconds_announced_as(LIVE_DURATION));
    wait_until_playing_past(&mut socket, 1_000_000);
    assert_eq!(
        mpris_length_us(bus),
        None,
        "a live stream has no length in MPRIS"
    );

    send_all(
        &mut socket,
        vec![
            Message::text(RECORDING_DURATION),
            Message::text(END_OF_STREAM),
        ],
    );

    bus.wait_for_log_line("Live timeline became a recording");
    wait_for_mpris_length_near(bus, RECORDING_LENGTH_US);
    let ended = bus.wait_for_log_line("Browser playback ended");
    assert!(
        log_number(&ended, "presented_frames") >= RECORDING_FRAMES_AT_LEAST,
        "every frame reaches the screen after the timeline changed: {ended}"
    );
}

#[test]
fn a_page_seek_past_the_first_announced_duration_lands_at_its_target() {
    let Some(process) =
        SessionProcessUnderTest::start("seek-past-first-duration", VideoMode::Headless)
    else {
        return;
    };
    let mut socket = process.open_attached_session();
    send_all(&mut socket, six_seconds_announced_as(FIRST_DURATION));
    wait_until_playing_past(&mut socket, 500_000);

    send_all(
        &mut socket,
        vec![super::generation_switch_clicks::page_seek(
            SEEK_PAST_FIRST_DURATION_US,
        )],
    );

    // Первые часы «играю» за пределами того, что сыграно до перемотки: там, где встал плеер.
    let messages =
        read_player_messages_until(&mut socket, "a clock after the page seek", |message| {
            matches!(
                message,
                PlayerSessionMessage::Clock {
                    state: ClockState::Playing,
                    position_us,
                    ..
                } if position_us.as_micros() >= 2_000_000
            )
        });
    let landed_us = super::session_ends::last_clock_us(&messages).expect("a clock");
    assert!(
        (SEEK_PAST_FIRST_DURATION_US..=SEEK_PAST_FIRST_DURATION_US + LANDING_SLACK_US)
            .contains(&landed_us),
        "the page seek to {SEEK_PAST_FIRST_DURATION_US} µs landed at {landed_us} µs"
    );
}
