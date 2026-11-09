//! Обмен выбором субтитров со страницей (протокол 1.7, сессия 14a, решение 283).
//!
//! Настоящий процесс под частной шиной и частным `kwin_wayland`; страницу играет тест по
//! настоящему сокету: шестисекундный поток и две дорожки страницы рамкой вида 3. Что
//! доказывают тесты:
//! - `subtitle_preference{page_track 2}` сильнее `showing` дорожки 1: разобрана и выбрана
//!   именно дорожка 2 (первая строка готовности — её, `selected=true`), а окно отдало в рендер
//!   кадры со слоем субтитров (`subtitle_overlay_frames` > 0);
//! - выбор зрителя (клавиша C, её зовёт тестовая переменная
//!   `FASTIPLAYER_TEST_BROWSER_SUBTITLE_ACTION` — у MPRIS субтитров нет) уходит странице ровно
//!   одним `subtitle_choice{off}`, а автовыбор и `showing` своих `subtitle_choice` не дают.
//!
//! Нужен `kwin_wayland` (иначе SKIPPED).

use browser_bridge_protocol::session::{PageSubtitleTrackId, SubtitleChoice};
use browser_bridge_protocol::{DataFrameHeader, PageSubtitleBytesHeader, encode_data_frame};

use super::hls_page::open_attached_session_speaking;
use super::session_ends::{send_all, six_seconds_downloaded};
use super::*;

/// `hello` экстеншена протокола 1.7 (подсказка и отчёт выбора субтитров).
const HELLO_1_7: &str = r#"{"type":"hello","protocol":{"major":1,"minor":7},"software_version":"0.1.0","capabilities":[]}"#;

/// Тестовая переменная действия зрителя с субтитрами (`window/subtitle_test_trigger.rs`).
const SUBTITLE_ACTION_VARIABLE: &str = "FASTIPLAYER_TEST_BROWSER_SUBTITLE_ACTION";

/// Строки журнала (`page_subtitles.rs`, `session_media/page_subtitles.rs`, тестовая переменная).
const PREFERENCE_LINE: &str = "Page subtitle preference received";
const TRACK_READY_LINE: &str = "Page subtitle track ready";
const CHOICE_REPORTED_LINE: &str = "Viewer subtitle choice reported to the page";
const TRIGGER_LINE: &str = "Test-only trigger applied a subtitle action";

/// Документ на весь ролик: любой показанный кадр с этой дорожкой уходит со слоем субтитров.
fn document(text: &str) -> String {
    format!("WEBVTT\n\n00:00:00.000 --> 00:01:00.000\n{text}\n")
}

/// Предложение дорожки `track` и её документ одной рамкой вида 3.
fn offered_track(track: u32, page_display: &str, text: &str) -> Vec<Message> {
    let body = document(text);
    let offer = format!(
        r#"{{"type":"subtitle_track_offered","track":{track},"language":{{"kind":"known","tag":"en"}},"label":{{"kind":"none"}},"format_hint":"web_vtt","origin":"page_download","page_display":"{page_display}","byte_length":{}}}"#,
        body.len()
    );
    let header = DataFrameHeader::PageSubtitleBytes(PageSubtitleBytesHeader {
        track: PageSubtitleTrackId::new(track).expect("track"),
        offset: browser_bridge_protocol::ByteOffset::new(0).expect("offset"),
    });
    vec![
        Message::text(offer),
        Message::binary(encode_data_frame(&header, body.as_bytes()).expect("data frame")),
    ]
}

/// Честный конец потока: ролик доиграет сам, пока страница на месте.
fn end_of_stream() -> Message {
    Message::text(r#"{"type":"end_of_stream","generation":1}"#)
}

#[test]
fn the_page_preference_enables_its_track_over_the_showing_one_and_it_is_drawn() {
    let Some(process) =
        SessionProcessUnderTest::start("page-subtitle-preference", VideoMode::Window)
    else {
        return;
    };
    let bus = &process.bus;
    let mut socket = open_attached_session_speaking(&process, HELLO_1_7);
    send_all(
        &mut socket,
        vec![Message::text(
            r#"{"type":"subtitle_preference","choice":{"kind":"page_track","track":2}}"#,
        )],
    );
    send_all(&mut socket, six_seconds_downloaded());
    // Дорожка 2 страница не показывает, дорожку 1 показывает: без подсказки выбрана была бы 1.
    let mut tracks = offered_track(2, "not_showing", "TRACK TWO");
    tracks.extend(offered_track(1, "showing", "TRACK ONE"));
    send_all(&mut socket, tracks);
    send_all(&mut socket, vec![end_of_stream()]);

    let preference = bus.wait_for_log_line(PREFERENCE_LINE);
    assert_eq!(log_number(&preference, "track"), 2, "{preference}");
    let ready = bus.wait_for_log_line(TRACK_READY_LINE);
    let ended = bus.wait_for_log_line("Browser playback ended");
    send_tab_gone_and_expect_close(&mut socket);
    eprintln!("{preference}\n{ready}\n{ended}");

    assert_eq!(
        log_number(&ready, "track"),
        2,
        "the preferred track, not the showing one, is the one parsed and shown: {ready}"
    );
    assert!(ready.contains("selected=true"), "{ready}");
    assert!(log_number(&ended, "presented_frames") > 0, "{ended}");
    assert!(
        log_number(&ended, "subtitle_overlay_frames") > 0,
        "the preferred page track reached the rendered frames: {ended}"
    );
    // Текст реплик в журнал не попадает (инвариант 7).
    assert!(!ready.contains("TRACK TWO"), "{ready}");
}

#[test]
fn a_viewer_subtitle_action_reaches_the_page_as_exactly_one_subtitle_choice() {
    let Some(process) = SessionProcessUnderTest::start_with_env(
        "page-subtitle-choice",
        VideoMode::Window,
        &[(SUBTITLE_ACTION_VARIABLE, "toggle@1500")],
    ) else {
        return;
    };
    let bus = &process.bus;
    let mut socket = open_attached_session_speaking(&process, HELLO_1_7);
    send_all(&mut socket, six_seconds_downloaded());
    // `showing`: страница включила субтитры сама — это не выбор зрителя, отчёта нет.
    send_all(&mut socket, offered_track(1, "showing", "TRACK ONE"));
    send_all(&mut socket, vec![end_of_stream()]);

    // До конца ролика (последние часы ≥ 5 с) — все сообщения странице.
    let messages = read_player_messages_until(&mut socket, "the clock near the end", |message| {
        matches!(
            message,
            PlayerSessionMessage::Clock { position_us, .. } if position_us.as_micros() >= 5_000_000
        )
    });
    let trigger = bus.wait_for_log_line(TRIGGER_LINE);
    let reported = bus.wait_for_log_line(CHOICE_REPORTED_LINE);
    send_tab_gone_and_expect_close(&mut socket);
    eprintln!("{trigger}\n{reported}");

    let choices: Vec<SubtitleChoice> = messages
        .iter()
        .filter_map(|message| match message {
            PlayerSessionMessage::SubtitleChoice { choice } => Some(*choice),
            _ => None,
        })
        .collect();
    assert_eq!(
        choices,
        vec![SubtitleChoice::Off {}],
        "the viewer pressed C once: one report, and `showing` reported nothing"
    );
    assert!(
        reported.contains("choice=\"off\"") || reported.contains("choice=off"),
        "{reported}"
    );
}
