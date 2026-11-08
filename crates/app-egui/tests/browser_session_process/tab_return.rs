//! «Магия возврата вкладки» на стороне плеера (протокол 1.4, «сирота», сессия 11b1,
//! решения 240–241): настоящий процесс с окном под частным `kwin_wayland`, поддельный
//! экстеншен на 1.4.
//!
//! Что доказывают тесты:
//! - страница ушла (`page_left`), а связь держится: окно на паузе ждёт зрителя, часы
//!   (пульс сироты) идут по той же связи;
//! - play зрителя (MPRIS окна — тот же путь, что кнопка) — экстеншену `reopen_page{T}` с
//!   секундой плеера;
//! - `page_returned` и новое поколение — тот же сеанс в том же окне: новое поколение
//!   загружено и доиграно до конца, кадры дошли до показа, процесс не вышел;
//! - отказ `page_reopen_failed` — запасной путь 04b: тост, досмотр, «Source tab closed»,
//!   окно закрыто.
//!
//! Нужен `kwin_wayland` (иначе SKIPPED) и `busctl`.

use super::hls_page::open_attached_session_speaking;
use super::page_and_mpris::{MPRIS_PLAYER, call_browser, wait_until_controllable};
use super::session_ends::{
    END_NOTICE_PERIOD_AT_LEAST, send_all, six_seconds_downloaded, wait_until_playing_past,
};
use super::*;
use tungstenite::protocol::CloseFrame;

/// `hello` экстеншена протокола 1.4 (сирота и возврат страницы).
const HELLO_1_4: &str = r#"{"type":"hello","protocol":{"major":1,"minor":4},"software_version":"0.1.0","capabilities":[]}"#;

const PAGE_LEFT: &str = r#"{"type":"page_left","reason":"closed"}"#;
const PAGE_RETURNED: &str = r#"{"type":"page_returned"}"#;
const REOPEN_FAILED: &str = r#"{"type":"page_reopen_failed","reason":"not_the_same_video"}"#;

/// Тот же короткий застой, что у теста паузы 04b: окно должно ждать дольше него.
const SHORT_STALL_MS: &str = "800";

/// Длина кадра фикстур (25 к/с).
const FRAME_US: u64 = 40_000;

/// Конец короткого ролика вернувшейся страницы (10 кадров по 40 мс) с запасом на тик.
const SHORT_CLIP_END_US: u64 = 600_000;

/// Короткий ролик (10 кадров, 0,4 с) новым поколением `generation`: так его пришлёт
/// вернувшаяся страница (новый `MediaSource`). Другой ролик, чем до ухода: конец его часов
/// (~0,4 с) не спутать с концом прежнего поколения (6 с).
fn short_clip_as_generation(generation: u32) -> Vec<Message> {
    let (steps, _bytes) = recorded_stream("two-source-buffers-mid-box-cuts");
    let number = StreamGeneration::new(generation).expect("generation");
    steps
        .into_iter()
        .map(|step| match step {
            StreamStep::Text(text) => Message::text(text.replace(
                r#""generation":1"#,
                &format!(r#""generation":{generation}"#),
            )),
            StreamStep::Binary(bytes) => {
                let frame = browser_bridge_protocol::decode_data_frame(&bytes).expect("frame");
                let DataFrameHeader::SourceBufferBytes(mut header) = frame.header else {
                    panic!("the recording has only source buffer frames");
                };
                header.generation = number;
                Message::binary(
                    encode_data_frame(&DataFrameHeader::SourceBufferBytes(header), frame.payload)
                        .expect("data frame"),
                )
            }
            StreamStep::AwaitClock(_) => panic!("this recording has no pauses"),
        })
        .collect()
}

/// Окно с потоком страницы на паузе, затем страница ушла, оставив связь. Возвращает
/// процесс, сокет сироты и позицию паузы.
fn paused_orphan(name: &str) -> Option<(SessionProcessUnderTest, Socket, u64)> {
    let process = SessionProcessUnderTest::start_with_env(
        name,
        VideoMode::Window,
        &[("FASTIPLAYER_TEST_BROWSER_DRAIN_STALL_MS", SHORT_STALL_MS)],
    )?;
    let mut socket = open_attached_session_speaking(&process, HELLO_1_4);
    // Зритель смотрел больше секунды, потом страница встала на паузу: T не ноль.
    send_all(&mut socket, six_seconds_downloaded());
    wait_until_playing_past(&mut socket, 1_000_000);
    let pause = std::fs::read_to_string(
        Path::new(FIXTURES).join("session/valid/page-page-playback-pause.json"),
    )
    .expect("pause fixture");
    socket
        .send(Message::text(pause.trim_end()))
        .expect("pause is sent");
    let paused = read_player_messages_until(&mut socket, "a paused clock", |message| {
        matches!(
            message,
            PlayerSessionMessage::Clock {
                state: ClockState::Paused,
                ..
            }
        )
    });
    let paused_at_us = clock_us(paused.last()).expect("a paused clock");
    socket
        .send(Message::text(PAGE_LEFT))
        .expect("page_left is sent");
    let notice = process
        .bus
        .wait_for_log_line("Browser session end notice shown");
    assert!(
        notice.contains("notice=waiting_for_viewer cause=tab_gone"),
        "{notice}"
    );
    Some((process, socket, paused_at_us))
}

type Socket = WebSocket<MaybeTlsStream<TcpStream>>;

fn clock_us(message: Option<&PlayerSessionMessage>) -> Option<u64> {
    match message {
        Some(PlayerSessionMessage::Clock { position_us, .. }) => Some(position_us.as_micros()),
        _ => None,
    }
}

/// Зритель нажал play на MPRIS окна → плеер просит страницу назад; возвращает T просьбы.
fn press_play_and_read_the_reopen_request(
    process: &SessionProcessUnderTest,
    socket: &mut Socket,
) -> u64 {
    wait_until_controllable(&process.bus);
    call_browser(&process.bus, MPRIS_PLAYER, "Play");
    let messages = read_player_messages_until(socket, "reopen_page", |message| {
        matches!(message, PlayerSessionMessage::ReopenPage { .. })
    });
    match messages.last() {
        Some(PlayerSessionMessage::ReopenPage { position_us }) => position_us.as_micros(),
        other => panic!("expected reopen_page, got {other:?}"),
    }
}

#[test]
fn play_after_the_page_left_brings_the_page_back_into_the_same_window() {
    let Some((process, mut socket, paused_at_us)) = paused_orphan("tab-return-magic") else {
        return;
    };
    let bus = &process.bus;
    // Пульс сироты: часы по той же связи и после ухода страницы.
    read_player_messages_until(&mut socket, "a clock of the orphan link", |message| {
        matches!(message, PlayerSessionMessage::Clock { .. })
    });
    // Окно ждёт человека дольше срока застоя: связь не рвётся, процесс не выходит.
    bus.expect_no_log_line_for(
        Duration::from_secs(2),
        &["Playback drain finished", "Browser session stopped"],
    );

    let reopen_at_us = press_play_and_read_the_reopen_request(&process, &mut socket);
    // T — секунда плеера на паузе (часы и просьба — из одного наблюдения, допуск — тик).
    assert!(
        reopen_at_us.abs_diff(paused_at_us) <= 300_000,
        "reopen_page at {reopen_at_us} µs, the player was paused at {paused_at_us} µs"
    );
    bus.wait_for_log_line("Browser window tells the viewer about the page reopen class=reopening");

    // Экстеншен вернул страницу: тот же сеанс, новое поколение (другой, короткий ролик).
    socket
        .send(Message::text(PAGE_RETURNED))
        .expect("page_returned is sent");
    send_all(&mut socket, short_clip_as_generation(2));

    // Машина выхода снова ведёт активный сеанс, досмотр отменён, а не доигран.
    bus.wait_for_log_line_without(
        "Process lifecycle followed the page link change=Returned outcome=Followed",
        &["Playback drain finished"],
    );
    bus.wait_for_log_line_without(
        "Browser stream generation loaded generation=2",
        &["Playback drain finished"],
    );
    // Конец часов — конец короткого ролика: его кадры показало то же окно.
    let ended = read_player_messages_until(&mut socket, "the returned clip ended", |message| {
        matches!(
            message,
            PlayerSessionMessage::Clock {
                state: ClockState::Ended,
                ..
            }
        )
    });
    let ended_at_us = clock_us(ended.last()).expect("an ended clock");
    assert!(
        ended_at_us <= SHORT_CLIP_END_US,
        "the clock ended at {ended_at_us} µs: that is not the returned page's short clip"
    );

    // Обычный конец вернувшейся страницы (старый путь `tab_gone`): итог показа.
    send_tab_gone_and_expect_close(&mut socket);
    let summary = bus.wait_for_log_line("Browser playback summary kind=final");
    // Кадры поколения 1 до T и все 10 кадров вернувшейся страницы (запас 2 на границы).
    assert!(
        log_number(&summary, "presented_frames") + 2
            >= reopen_at_us / FRAME_US + FIXTURE_VIDEO_FRAMES,
        "the returned page's generation must reach the presentation: {summary}"
    );
    bus.wait_for_log_line("Browser session process finished");
}

#[test]
fn a_refused_reopen_falls_back_to_the_drain_and_the_window_closes() {
    let Some((process, mut socket, _paused_at_us)) = paused_orphan("tab-return-refused") else {
        return;
    };
    let bus = &process.bus;
    press_play_and_read_the_reopen_request(&process, &mut socket);
    socket
        .send(Message::text(REOPEN_FAILED))
        .expect("page_reopen_failed is sent");
    // Экстеншен сдался: закрывает сироту своим кодом «вкладка ушла».
    socket
        .close(Some(CloseFrame {
            code: CloseCode::from(SessionCloseReason::TabGone.close_code()),
            reason: "".into(),
        }))
        .expect("the orphan link is closed");

    bus.wait_for_log_line(
        "Browser window tells the viewer about the page reopen class=not_the_same_video",
    );
    bus.wait_for_log_line("Playback drain finished class=played_to_end");
    let notice = bus.wait_for_log_line("notice=finished");
    let notice_at = Instant::now();
    assert!(
        notice.contains("notice=finished cause=tab_gone end=played_to_end"),
        "{notice}"
    );
    bus.wait_for_log_line("Browser window closed after the end notice");
    bus.wait_for_log_line("Browser session process finished");
    assert!(notice_at.elapsed() >= END_NOTICE_PERIOD_AT_LEAST);
}
