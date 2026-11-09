//! Субтитры страницы в браузерном окне (мост, сессия 13b1, протокол 1.5).
//!
//! Настоящий процесс под частной шиной и частным `kwin_wayland`; страницу играет тест по
//! настоящему сокету: шестисекундный поток, затем `subtitle_track_offered{showing}` и байты
//! документа рамкой вида 3. Что доказывают тесты:
//! - документ собран, лёг на склад, стал дорожкой текущего медиа и выбран (строка журнала
//!   готовности с `selected=true`), а окно отдало в рендер кадры со слоем субтитров
//!   (`subtitle_overlay_frames` > 0 в сводке конца медиа и в итоговой);
//! - пока на экране плашка рекламы (весь ролик), тот же документ выбран, но ни одного кадра
//!   со слоем нет. Сводка берётся на конце медиа: уход вкладки снимает плашку, и досмотр
//!   после него рисовал бы субтитры снова.
//!
//! Нужен `kwin_wayland` (иначе SKIPPED).

use browser_bridge_protocol::session::PageSubtitleTrackId;
use browser_bridge_protocol::{DataFrameHeader, PageSubtitleBytesHeader, encode_data_frame};

use super::hls_page::open_attached_session_speaking;
use super::session_ends::{send_all, six_seconds_downloaded};
use super::*;

/// `hello` экстеншена протокола 1.5 (субтитры страницы).
const HELLO_1_5: &str = r#"{"type":"hello","protocol":{"major":1,"minor":5},"software_version":"0.1.0","capabilities":[]}"#;

/// Строки журнала (`crates/app-egui/src/page_subtitles.rs`).
const DOCUMENT_LINE: &str = "Page subtitle document received";
const TRACK_READY_LINE: &str = "Page subtitle track ready";

/// Одна реплика на весь ролик: любой показанный кадр должен уйти со слоем субтитров.
const DOCUMENT: &str = "WEBVTT\n\n00:00:00.000 --> 00:01:00.000\nPAGE SUBTITLE\n";

/// Предложение дорожки 1 (`showing`) и её документ одной рамкой вида 3.
fn offered_document() -> Vec<Message> {
    let offer = format!(
        r#"{{"type":"subtitle_track_offered","track":1,"language":{{"kind":"known","tag":"en"}},"label":{{"kind":"text","text":"English"}},"format_hint":"web_vtt","origin":"page_download","page_display":"showing","byte_length":{}}}"#,
        DOCUMENT.len()
    );
    let header = DataFrameHeader::PageSubtitleBytes(PageSubtitleBytesHeader {
        track: PageSubtitleTrackId::new(1).expect("track"),
        offset: browser_bridge_protocol::ByteOffset::new(0).expect("offset"),
    });
    vec![
        Message::text(offer),
        Message::binary(encode_data_frame(&header, DOCUMENT.as_bytes()).expect("data frame")),
    ]
}

/// Реклама без кнопки (счётчик): плашка на экране всё время прогона.
fn ad_countdown() -> Message {
    Message::text(
        r#"{"type":"ad_state","ad":{"kind":"showing","pod":{"kind":"unknown"},"skip":{"kind":"countdown","seconds":30}}}"#,
    )
}

/// Окно играет шестисекундный ролик до конца с дорожкой страницы; возвращает строку
/// готовности и сводку конца медиа (`Browser playback ended …`).
fn play_with_page_subtitles(name: &str, before_stream: Vec<Message>) -> Option<(String, String)> {
    let process = SessionProcessUnderTest::start(name, VideoMode::Window)?;
    let mut socket = open_attached_session_speaking(&process, HELLO_1_5);
    send_all(&mut socket, before_stream);
    send_all(&mut socket, six_seconds_downloaded());
    send_all(&mut socket, offered_document());
    // Честный конец потока: ролик доиграет сам, пока страница (и плашка) на месте.
    send_all(
        &mut socket,
        vec![Message::text(r#"{"type":"end_of_stream","generation":1}"#)],
    );
    let bus = &process.bus;
    let received = bus.wait_for_log_line(DOCUMENT_LINE);
    assert_eq!(log_number(&received, "track"), 1, "{received}");
    assert_eq!(
        log_number(&received, "bytes"),
        DOCUMENT.len() as u64,
        "{received}"
    );
    let ready = bus.wait_for_log_line(TRACK_READY_LINE);
    let ended = bus.wait_for_log_line("Browser playback ended");
    send_tab_gone_and_expect_close(&mut socket);
    Some((ready, ended))
}

#[test]
fn a_showing_page_document_is_selected_and_drawn_on_window_frames() {
    let Some((ready, ended)) = play_with_page_subtitles("page-subtitles", Vec::new()) else {
        return;
    };
    eprintln!("{ready}\n{ended}");
    assert_eq!(log_number(&ready, "track"), 1, "{ready}");
    assert_eq!(log_number(&ready, "cues"), 1, "{ready}");
    assert!(
        ready.contains("format=\"web_vtt\"") || ready.contains("format=web_vtt"),
        "{ready}"
    );
    assert!(ready.contains("selected=true"), "{ready}");
    assert!(log_number(&ended, "presented_frames") > 0, "{ended}");
    assert!(
        log_number(&ended, "subtitle_overlay_frames") > 0,
        "page subtitles reached the rendered frames: {ended}"
    );
    // Текст реплики и подпись страницы в журнал не попадают (инвариант 7).
    assert!(
        !ready.contains("PAGE SUBTITLE") && !ready.contains("English"),
        "{ready}"
    );
}

#[test]
fn while_an_ad_plate_is_up_the_selected_page_track_draws_nothing() {
    let Some((ready, ended)) = play_with_page_subtitles("page-subtitles-ad", vec![ad_countdown()])
    else {
        return;
    };
    eprintln!("{ready}\n{ended}");
    assert!(
        ready.contains("selected=true"),
        "the ad does not change the choice: {ready}"
    );
    assert!(log_number(&ended, "presented_frames") > 0, "{ended}");
    assert_eq!(
        log_number(&ended, "subtitle_overlay_frames"),
        0,
        "no subtitle layer while the ad plate is on screen: {ended}"
    );
}
