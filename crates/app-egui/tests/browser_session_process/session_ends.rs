//! Концы сеанса (мост, сессия 04b): что получает страница и что делает окно.
//!
//! - Замена вкладки (решения 40, 127): старая «страница» получает `return_to_page{T,
//!   paused}` РАНЬШЕ своего кода закрытия 4001, T — в пределах тика от последних часов;
//!   новое поколение играет до кадров на показе (оба режима).
//! - Закрытие окна человеком (решения 12, 51): НАСТОЯЩИЙ композитор закрывает окно
//!   (скрипт KWin `closeWindow()` → `xdg_toplevel.close`), странице — прощание с позицией
//!   наших часов, затем 4000; процесс выходит штатно.
//! - Вкладка ушла с 6 с скачанного (решение 42): окно доигрывает все 6 с, показывает
//!   сообщение и закрывается не раньше срока сообщения.
//! - Вкладка ушла на паузе: окно живо дольше срока застоя и ждёт человека; закрытие
//!   окна = «доиграно», процесс выходит. Без окна пауза по-прежнему = выход сразу
//!   (`file_and_drain.rs`, `a_paused_page_makes_the_process_exit_at_once_when_the_tab_is_gone`).
//!
//! Оконные тесты идут под частным `kwin_wayland --virtual` и печатают SKIPPED без него.

use browser_bridge_protocol::session::player::ResumeState;

use super::*;

/// Вторая вкладка (другой ключ сеанса): её `open_session` заменяет первый сеанс.
const OPEN_SESSION_FROM_ANOTHER_TAB: &str = r#"{"type":"open_session","request_id":2,"session_key":"tab-13-video-1","page":{"title":"Episode 4","domain":"example.org"},"intent":"window","hints":{"duration":{"kind":"unknown"},"position":{"kind":"unknown"}}}"#;

/// Шаг часов странице (`CLOCK_INTERVAL` процесса) и запас на проход цикла (кадр окна
/// при 25 к/с — 40 мс, плюс планирование потоков на занятой машине).
const CLOCK_TICK_US: u64 = 250_000;
const PASS_SLACK_US: u64 = 100_000;

/// Срок сообщения о конце сеанса в окне (`END_NOTICE_PERIOD` процесса) минус допуск на
/// чтение журнала тестом (строки читаются из трубы с небольшой задержкой).
pub(super) const END_NOTICE_PERIOD_AT_LEAST: Duration = Duration::from_millis(2_400);

/// Кадров в 6-секундном ролике фикстуры (`video-64-6s.mp4`, 25 к/с).
const SIX_SECOND_VIDEO_FRAMES: u64 = 150;

/// Ролик 6 с одним куском на полосу (звук и видео), без `end_of_stream`: «скачано 6 с»,
/// а конец потока поставит сам процесс, когда вкладка уйдёт.
pub(super) fn six_seconds_downloaded() -> Vec<Message> {
    let stream = Path::new(FIXTURES).join("streams/quality-switch-new-media-source");
    let mut messages: Vec<Message> = ["001.json", "002.json", "003.json", "004.json"]
        .iter()
        .map(|file| {
            let text = std::fs::read_to_string(stream.join(file)).expect("fixture message");
            Message::text(text.trim_end().to_owned())
        })
        .collect();
    // Те же полосы, что объявляют сообщения выше: 0 — звук, 1 — видео.
    for (source_buffer, media) in [(0, "audio-6s.mp4"), (1, "video-64-6s.mp4")] {
        let bytes = std::fs::read(Path::new(FIXTURES).join("media").join(media)).expect("media");
        let header = DataFrameHeader::SourceBufferBytes(SourceBufferBytesHeader {
            generation: StreamGeneration::FIRST,
            source_buffer: SourceBufferIndex::new(source_buffer).expect("source buffer"),
            sequence: AppendSequence(0),
        });
        messages.push(Message::binary(
            encode_data_frame(&header, &bytes).expect("data frame"),
        ));
    }
    messages
}

pub(super) fn send_all(socket: &mut WebSocket<MaybeTlsStream<TcpStream>>, messages: Vec<Message>) {
    for message in messages {
        socket.send(message).expect("stream message is sent");
    }
}

/// Ждёт часов «играю» дальше `position_us`; возвращает их позицию.
pub(super) fn wait_until_playing_past(
    socket: &mut WebSocket<MaybeTlsStream<TcpStream>>,
    position_us: u64,
) -> u64 {
    let messages = read_player_messages_until(socket, "a playing clock", |message| {
        matches!(
            message,
            PlayerSessionMessage::Clock {
                state: ClockState::Playing,
                position_us: position,
                ..
            } if position.as_micros() >= position_us
        )
    });
    last_clock_us(&messages).expect("a clock")
}

fn last_clock_us(messages: &[PlayerSessionMessage]) -> Option<u64> {
    messages.iter().rev().find_map(|message| match message {
        PlayerSessionMessage::Clock { position_us, .. } => Some(position_us.as_micros()),
        _ => None,
    })
}

/// Читает сообщения до прощания и кадра закрытия: (последние часы до прощания,
/// позиция прощания, код закрытия). `clock_seen_us` — последние часы, уже прочитанные
/// тестом раньше. Прощание обязано прийти РАНЬШЕ кадра закрытия, и после него страница
/// не получает ничего.
pub(super) fn read_farewell_then_close(
    socket: &mut WebSocket<MaybeTlsStream<TcpStream>>,
    clock_seen_us: u64,
) -> (u64, u64, CloseCode) {
    let mut last_clock = clock_seen_us;
    let mut farewell = None;
    let deadline = Instant::now() + WAIT;
    loop {
        assert!(
            Instant::now() < deadline,
            "the page never got the close frame"
        );
        match socket.read() {
            Ok(Message::Text(text)) => {
                let message: PlayerSessionMessage =
                    decode_message(text.as_bytes()).expect("player session message");
                assert!(
                    farewell.is_none(),
                    "nothing may follow the farewell: {message:?}"
                );
                match message {
                    PlayerSessionMessage::Clock { position_us, .. } => {
                        last_clock = position_us.as_micros();
                    }
                    PlayerSessionMessage::ReturnToPage {
                        position_us,
                        resume,
                    } => {
                        assert_eq!(resume, ResumeState::Paused, "decision 51: back on pause");
                        farewell = Some(position_us.as_micros());
                    }
                    _ => {}
                }
            }
            Ok(Message::Close(Some(frame))) => {
                let farewell = farewell.expect("return_to_page must come BEFORE the close frame");
                return (last_clock, farewell, frame.code);
            }
            Ok(_) => continue,
            Err(error) => panic!("waiting for the farewell and the close frame: {error}"),
        }
    }
}

/// T прощания — позиция наших часов: не раньше последних часов и не дальше одного тика.
pub(super) fn assert_farewell_within_a_clock_tick(last_clock_us: u64, farewell_us: u64) {
    assert!(
        farewell_us >= last_clock_us,
        "the farewell position {farewell_us} µs is behind the last clock {last_clock_us} µs"
    );
    assert!(
        farewell_us - last_clock_us <= CLOCK_TICK_US + PASS_SLACK_US,
        "the farewell position {farewell_us} µs is more than a clock tick past {last_clock_us} µs"
    );
}

// ---- замена вкладки --------------------------------------------------------------

#[test]
fn another_tab_returns_the_old_one_to_its_position_before_closing_it() {
    replacement_returns_the_old_tab_to_its_position(VideoMode::Headless);
}

#[test]
fn another_tab_returns_the_old_one_to_its_position_before_closing_it_in_a_window() {
    replacement_returns_the_old_tab_to_its_position(VideoMode::Window);
}

fn replacement_returns_the_old_tab_to_its_position(mode: VideoMode) {
    let Some(process) = SessionProcessUnderTest::start("replacement-farewell", mode) else {
        return;
    };
    let bus = &process.bus;
    let mut old_tab = process.open_attached_session();
    send_all(&mut old_tab, six_seconds_downloaded());
    let clock_seen = wait_until_playing_past(&mut old_tab, 1_000_000);

    // Прощание звучит в том же шаге, что открытие нового сеанса (в журнале — раньше
    // строки «Browser session opened», которую ждёт помощник открытия).
    let mut new_tab = open_attached_session_with(bus, OPEN_SESSION_FROM_ANOTHER_TAB);

    let (last_clock, farewell, code) = read_farewell_then_close(&mut old_tab, clock_seen);
    assert_eq!(
        code,
        CloseCode::from(SessionCloseReason::ReplacedByAnotherTab.close_code())
    );
    assert_farewell_within_a_clock_tick(last_clock, farewell);

    // Новая вкладка играет в том же процессе (и том же окне) до конца своего ролика.
    let (steps, _bytes) = recorded_stream("two-source-buffers-mid-box-cuts");
    send_steps(&mut new_tab, steps);
    read_player_messages_until(&mut new_tab, "the new tab's clock ended", |message| {
        matches!(
            message,
            PlayerSessionMessage::Clock {
                state: ClockState::Ended,
                ..
            }
        )
    });
    send_tab_gone_and_expect_close(&mut new_tab);
    let summary = bus.wait_for_log_line("Browser playback summary kind=final");
    // Старая вкладка показала кадры до T (25 к/с), новая — все свои 10: на показе
    // побывало не меньше обоих (запас 2 кадра на границы).
    let old_frames = farewell / 40_000;
    assert!(
        log_number(&summary, "presented_frames") + 2 >= old_frames + FIXTURE_VIDEO_FRAMES,
        "the new generation must reach the presentation after the old one: {summary}"
    );
    bus.wait_for_log_line("Browser session process finished");
}

fn send_steps(socket: &mut WebSocket<MaybeTlsStream<TcpStream>>, steps: Vec<StreamStep>) {
    for step in steps {
        let message = match step {
            StreamStep::Text(text) => Message::text(text),
            StreamStep::Binary(bytes) => Message::binary(bytes),
            StreamStep::AwaitClock(_) => panic!("this recording has no pauses"),
        };
        socket.send(message).expect("stream step is sent");
    }
}

// ---- зритель закрыл окно -----------------------------------------------------------

#[test]
fn closing_the_window_returns_the_page_to_our_clock_and_exits() {
    let Some(process) = SessionProcessUnderTest::start("viewer-close", VideoMode::Window) else {
        return;
    };
    let display = process.display().expect("window mode has a display");
    let bus = &process.bus;
    let mut socket = process.open_attached_session();
    send_all(&mut socket, six_seconds_downloaded());
    let clock_seen = wait_until_playing_past(&mut socket, 1_000_000);

    display.close_fastiplayer_window();

    let (last_clock, farewell, code) = read_farewell_then_close(&mut socket, clock_seen);
    assert_eq!(
        code,
        CloseCode::from(SessionCloseReason::PlayerClosed.close_code())
    );
    assert_farewell_within_a_clock_tick(last_clock, farewell);
    let farewell_log = bus.wait_for_log_line("Session closed with a farewell to the page");
    assert!(farewell_log.contains("FarewellQueued"), "{farewell_log}");
    bus.wait_for_log_line("Browser window process exits");
    // Штатный конец (код выхода 0 — `Completed`), а не падение.
    bus.wait_for_log_line("Browser session process finished");
}

// ---- вкладка ушла ----------------------------------------------------------------

#[test]
fn the_window_plays_what_was_downloaded_after_the_tab_left_then_says_so_and_closes() {
    let Some(process) = SessionProcessUnderTest::start("tab-gone-notice", VideoMode::Window) else {
        return;
    };
    let bus = &process.bus;
    let mut socket = process.open_attached_session();
    send_all(&mut socket, six_seconds_downloaded());
    let playing_at_us = wait_until_playing_past(&mut socket, 200_000);

    send_tab_gone_and_expect_close(&mut socket);
    let tab_left_at = Instant::now();

    bus.wait_for_log_line("Playback drain finished class=played_to_end");
    let played_for = tab_left_at.elapsed();
    // Без страницы окно доиграло скачанное: ~6 с от позиции ухода вкладки (запас 1 с на
    // загруженную машину — плеер не обгоняет реальное время, отставать может).
    let left_to_play = Duration::from_micros(6_000_000 - playing_at_us);
    assert!(
        played_for + Duration::from_secs(1) >= left_to_play,
        "the window stopped after {played_for:?}, {left_to_play:?} were left to play"
    );
    let notice = bus.wait_for_log_line("Browser session end notice shown");
    let notice_at = Instant::now();
    assert!(
        notice.contains("notice=finished cause=tab_gone end=played_to_end"),
        "{notice}"
    );
    let summary = bus.wait_for_log_line("Browser playback summary kind=final");
    assert!(
        log_number(&summary, "presented_frames") + 2 >= SIX_SECOND_VIDEO_FRAMES,
        "every downloaded frame is shown although the tab left: {summary}"
    );
    bus.wait_for_log_line("Browser window closed after the end notice");
    bus.wait_for_log_line("Browser session process finished");
    let shown_for = notice_at.elapsed();
    assert!(
        shown_for >= END_NOTICE_PERIOD_AT_LEAST,
        "the window closed {shown_for:?} after the notice"
    );
}

/// Срок застоя доигрывания для теста паузы: короче, чем тест ждёт (без окна процесс
/// вышел бы и по паузе сразу, и по застою через этот срок).
const SHORT_STALL_MS: &str = "800";

#[test]
fn a_paused_window_waits_for_the_viewer_after_the_tab_left() {
    let Some(process) = SessionProcessUnderTest::start_with_env(
        "tab-gone-paused-window",
        VideoMode::Window,
        &[("FASTIPLAYER_TEST_BROWSER_DRAIN_STALL_MS", SHORT_STALL_MS)],
    ) else {
        return;
    };
    let display = process.display().expect("window mode has a display");
    let bus = &process.bus;
    let mut socket = process.open_attached_session();
    let pause = std::fs::read_to_string(
        Path::new(FIXTURES).join("session/valid/page-page-playback-pause.json"),
    )
    .expect("pause fixture");
    socket
        .send(Message::text(pause.trim_end()))
        .expect("pause is sent");
    send_all(&mut socket, six_seconds_downloaded());
    read_player_messages_until(&mut socket, "a paused clock", |message| {
        matches!(
            message,
            PlayerSessionMessage::Clock {
                state: ClockState::Paused,
                ..
            }
        )
    });

    send_tab_gone_and_expect_close(&mut socket);

    let notice = bus.wait_for_log_line("Browser session end notice shown");
    assert!(
        notice.contains("notice=waiting_for_viewer cause=tab_gone"),
        "{notice}"
    );
    // Дольше и прежнего «выход сразу», и срока застоя: окно ждёт человека.
    bus.expect_no_log_line_for(
        Duration::from_secs(3),
        &["Playback drain finished", "Browser session stopped"],
    );

    display.close_fastiplayer_window();
    bus.wait_for_log_line("Playback drain finished class=viewer_closed");
    bus.wait_for_log_line("Browser session process finished");
}
