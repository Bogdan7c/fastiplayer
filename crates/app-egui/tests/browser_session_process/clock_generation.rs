//! Сквозные тесты 16a2: номер поколения в часах (протокол 1.10) у всех видов поколения.
//!
//! Страница 1.10 получает в каждом тике `generation` — тот же номер, что она объявила
//! (`generation_started` у MSE, `file_source_added` у файла, `hls_source_added` у HLS);
//! страница 1.9 — прежний тик без поля (её строгий разбор отверг бы незнакомое поле). Смену
//! номера 1 → 2 у MSE проверяет `page_play_after_end.rs`. Без окна: здесь важны тики, а не
//! кадры.

use super::file_and_drain::FilePage;
use super::hls_page::{
    HlsPage, PageResource, announce_hls_generation, fmp4_vod_playlist, fragmented_movie,
    open_attached_session_speaking,
};
use super::next_episode::first_episode_to_the_end;
use super::page_play_after_end::HELLO_1_10;
use super::session_ends::send_all;
use super::*;

/// `hello` экстеншена протокола 1.9: номера в часах он не знает.
const HELLO_1_9: &str = r#"{"type":"hello","protocol":{"major":1,"minor":9},"software_version":"0.1.0","capabilities":[]}"#;

/// Поле `generation` каждого тика из `messages` (`Some(None)` — тик без поля).
fn clock_generations(messages: &[PlayerSessionMessage]) -> Vec<Option<StreamGeneration>> {
    messages
        .iter()
        .filter_map(|message| match message {
            PlayerSessionMessage::Clock { generation, .. } => Some(*generation),
            _ => None,
        })
        .collect()
}

/// Тиков было, и каждый несёт ровно `expected`.
fn assert_every_tick_names(messages: &[PlayerSessionMessage], expected: Option<u32>) {
    let generations = clock_generations(messages);
    assert!(!generations.is_empty(), "the page heard no clock at all");
    let expected = expected.map(|number| StreamGeneration::new(number).expect("generation"));
    assert!(
        generations.iter().all(|generation| *generation == expected),
        "expected every tick to carry {expected:?}: {generations:?}"
    );
}

/// MSE до конца на странице этой версии; возвращает всё, что страница услышала.
fn mse_played_to_the_end(name: &str, hello: &str) -> Vec<PlayerSessionMessage> {
    let Some(process) = SessionProcessUnderTest::start(name, VideoMode::Headless) else {
        return Vec::new();
    };
    let mut socket = open_attached_session_speaking(&process, hello);
    send_all(&mut socket, first_episode_to_the_end());
    let messages = read_player_messages_until(&mut socket, "the end of the stream", |message| {
        matches!(
            message,
            PlayerSessionMessage::Clock {
                state: ClockState::Ended,
                ..
            }
        )
    });
    send_tab_gone_and_expect_close(&mut socket);
    process
        .bus
        .wait_for_log_line("Browser session process finished");
    messages
}

/// MSE: у страницы 1.10 каждый тик — номер `generation_started` (1), от «жду» до `ended`.
#[test]
fn a_1_10_page_hears_the_mse_generation_in_every_clock() {
    let messages = mse_played_to_the_end("clock-gen-mse-1-10", HELLO_1_10);
    if messages.is_empty() {
        return;
    }
    assert_every_tick_names(&messages, Some(1));
}

/// MSE: страница 1.9 слышит те же часы без поля.
#[test]
fn a_1_9_page_hears_the_clock_without_the_generation() {
    let messages = mse_played_to_the_end("clock-gen-mse-1-9", HELLO_1_9);
    if messages.is_empty() {
        return;
    }
    assert_every_tick_names(&messages, None);
}

/// Файл `<video src>`: номер — тот, что объявил `file_source_added`.
#[test]
fn a_1_10_page_hears_the_file_generation_in_every_clock() {
    let Some(process) = SessionProcessUnderTest::start("clock-gen-file", VideoMode::Headless)
    else {
        return;
    };
    let mut socket = open_attached_session_speaking(&process, HELLO_1_10);
    for text in [
        r#"{"type":"generation_started","generation":1}"#,
        r#"{"type":"file_source_added","generation":1}"#,
    ] {
        socket.send(Message::text(text)).expect("message is sent");
    }
    let messages = FilePage::new().serve_until_ended(&mut socket);
    assert_every_tick_names(&messages, Some(1));
    send_tab_gone_and_expect_close(&mut socket);
    process
        .bus
        .wait_for_log_line("Browser session process finished");
}

/// HLS VOD: номер — тот, что объявил `hls_source_added` (поддельная страница объявляет
/// поколение 1).
#[test]
fn a_1_10_page_hears_the_hls_generation_in_every_clock() {
    let Some(process) = SessionProcessUnderTest::start("clock-gen-hls", VideoMode::Headless) else {
        return;
    };
    let movie = fragmented_movie();
    let mut resources = vec![
        (
            0,
            PageResource::Body(fmp4_vod_playlist(1, 10, movie.fragments.len())),
        ),
        (1, PageResource::Body(movie.init.clone())),
    ];
    for (index, fragment) in movie.fragments.iter().enumerate() {
        resources.push((10 + index as u32, PageResource::Body(fragment.clone())));
    }
    let mut socket = open_attached_session_speaking(&process, HELLO_1_10);
    announce_hls_generation(&mut socket, 0);
    let messages = HlsPage::new(resources).serve_until_ended(&mut socket);
    assert_every_tick_names(&messages, Some(1));
    send_tab_gone_and_expect_close(&mut socket);
    process
        .bus
        .wait_for_log_line("Browser session process finished");
}
