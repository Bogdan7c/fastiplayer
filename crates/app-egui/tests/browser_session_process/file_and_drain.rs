//! Сквозные тесты 03c: `<video src>` по запросам байтов и доигрывание после ухода
//! вкладки. Настоящий бинарь, настоящий плеер без окна, звук — тихие часы.
//!
//! - файл: «страница» объявляет `file_source_added` и отвечает на каждый
//!   `fetch_range` байтами фикстуры (progressive MP4 с оглавлением в конце, ~100 КиБ)
//!   рамками вида 2 и `range_fetch_finished`; плеер играет файл до конца;
//! - вкладка ушла (`tab_gone`) посреди воспроизведения: процесс доигрывает полученное
//!   и выходит по «доиграно»; на паузе — выходит сразу.
//!
//! Порядок событий проверяется порядком строк журнала: тест читает их по очереди.

use browser_bridge_protocol::session::page::{RangeFetchOutcome, ResourceSize};
use browser_bridge_protocol::{
    ByteOffset, PageSessionMessage, RangeBytesHeader, RangeRequestId, encode_message,
};

use super::*;

/// Фикстура файла: progressive MP4, оглавление в конце, 4 с, 100 кадров, H.264 + AAC.
/// Лежит в `pushed-media-source`, где её сверяют тесты «до кадра».
const FILE_FIXTURE: &str = concat!(
    env!("CARGO_MANIFEST_DIR"),
    "/../pushed-media-source/fixtures/file-h264-aac-moov-end.mp4"
);

/// Кадров в файле-фикстуре (25 к/с × 4 с).
const FILE_FRAMES: u64 = 100;

/// Самая большая рамка байтов (протокол: ≤ 1 МиБ).
const MAX_FRAME_BYTES: usize = 1024 * 1024;

/// Отвечает на `fetch_range` байтами файла, как это делает background экстеншена.
struct FilePage {
    bytes: Vec<u8>,
    /// Все полученные запросы: (номер, смещение, длина).
    requests: Vec<(u32, u64, u64)>,
}

impl FilePage {
    fn new() -> Self {
        Self {
            bytes: std::fs::read(FILE_FIXTURE).expect("file fixture"),
            requests: Vec::new(),
        }
    }

    fn answer(
        &mut self,
        socket: &mut WebSocket<MaybeTlsStream<TcpStream>>,
        message: &PlayerSessionMessage,
    ) {
        let PlayerSessionMessage::FetchRange {
            request_id,
            offset,
            length,
        } = message
        else {
            return;
        };
        self.requests
            .push((request_id.0, offset.as_bytes(), length.as_bytes()));
        let start = usize::try_from(offset.as_bytes()).expect("offset");
        let end =
            (start + usize::try_from(length.as_bytes()).expect("length")).min(self.bytes.len());
        let mut position = start;
        while position < end {
            let frame_end = (position + MAX_FRAME_BYTES).min(end);
            let header = DataFrameHeader::RangeBytes(RangeBytesHeader {
                request_id: *request_id,
                offset: ByteOffset::new(position as u64).expect("offset"),
            });
            let frame = encode_data_frame(&header, &self.bytes[position..frame_end])
                .expect("range data frame");
            socket.send(Message::binary(frame)).expect("range bytes");
            position = frame_end;
        }
        let finished = PageSessionMessage::RangeFetchFinished {
            request_id: RangeRequestId(request_id.0),
            outcome: RangeFetchOutcome::Completed {
                resource_size: ResourceSize::Known {
                    size_bytes: ByteOffset::new(self.bytes.len() as u64).expect("size"),
                },
            },
        };
        let text = encode_message(&finished).expect("range_fetch_finished");
        socket.send(Message::text(text)).expect("range result");
    }

    /// Отвечает на запросы байтов, пока плеер не доиграет файл; возвращает всё, что
    /// страница получила по дороге.
    fn serve_until_ended(
        &mut self,
        socket: &mut WebSocket<MaybeTlsStream<TcpStream>>,
    ) -> Vec<PlayerSessionMessage> {
        let mut messages = Vec::new();
        let deadline = Instant::now() + WAIT;
        loop {
            assert!(
                Instant::now() < deadline,
                "the file never played to the end"
            );
            let batch = read_player_messages_until(socket, "the next player message", |_| true);
            for message in batch {
                self.answer(socket, &message);
                let ended = matches!(
                    message,
                    PlayerSessionMessage::Clock {
                        state: ClockState::Ended,
                        ..
                    }
                );
                messages.push(message);
                if ended {
                    break;
                }
            }
            if matches!(
                messages.last(),
                Some(PlayerSessionMessage::Clock {
                    state: ClockState::Ended,
                    ..
                })
            ) {
                break;
            }
        }
        messages
    }
}

/// Файл целиком: страница отвечает на запросы, плеер играет до конца ролика.
#[test]
fn a_file_generation_is_fetched_by_ranges_and_played_to_the_end() {
    file_generation_is_fetched_by_ranges_and_played_to_the_end(VideoMode::Headless);
}

/// То же в окне: поколение-файл идёт путём открытия `AppState` окна.
#[test]
fn a_file_generation_is_fetched_by_ranges_and_played_to_the_end_in_a_window() {
    file_generation_is_fetched_by_ranges_and_played_to_the_end(VideoMode::Window);
}

fn file_generation_is_fetched_by_ranges_and_played_to_the_end(mode: VideoMode) {
    let Some(process) = SessionProcessUnderTest::start("file-plays", mode) else {
        return;
    };
    let bus = &process.bus;
    let mut socket = process.open_attached_session();
    for text in [
        r#"{"type":"generation_started","generation":1}"#,
        r#"{"type":"file_source_added","generation":1}"#,
    ] {
        socket.send(Message::text(text)).expect("message is sent");
    }
    let mut page = FilePage::new();
    let messages = page.serve_until_ended(&mut socket);

    // Страница получила запросы: первый — начало файла, и был запрос за оглавлением в
    // конце (смещение дальше первых килобайт).
    assert_eq!(page.requests.first().map(|r| (r.0, r.1)), Some((1, 0)));
    assert!(
        page.requests
            .iter()
            .any(|(_, offset, _)| *offset > page.bytes.len() as u64 / 2),
        "no request for the movie box at the end: {:?}",
        page.requests
    );
    let clock_positions: Vec<u64> = messages
        .iter()
        .filter_map(|message| match message {
            PlayerSessionMessage::Clock { position_us, .. } => Some(position_us.as_micros()),
            _ => None,
        })
        .collect();
    assert!(
        clock_positions.windows(2).all(|pair| pair[0] <= pair[1]),
        "clock went backwards: {clock_positions:?}"
    );
    assert!(
        !messages
            .iter()
            .any(|message| matches!(message, PlayerSessionMessage::BufferState { .. })),
        "a file generation never gets buffer_state"
    );

    // Вкладка ушла после конца: доиграно сразу, итоги — в журнале.
    send_tab_gone_and_expect_close(&mut socket);
    bus.wait_for_log_line("Playback drain finished class=played_to_end");
    let ranges = bus.wait_for_log_line("Browser file range requests");
    assert_eq!(
        log_number(&ranges, "range_requests_sent"),
        page.requests.len() as u64,
        "{ranges}"
    );
    assert!(log_number(&ranges, "range_bytes_requested") > 0, "{ranges}");
    let summary = bus.wait_for_log_line("Browser playback summary kind=final");
    let presented = log_number(&summary, "presented_frames");
    assert!(
        (FILE_FRAMES - 3..=FILE_FRAMES).contains(&presented),
        "expected about {FILE_FRAMES} presented frames: {summary}"
    );
    assert_eq!(log_number(&summary, "position_regressions"), 0, "{summary}");
    assert!(log_number(&summary, "audio_played_ms") > 0, "{summary}");
    bus.wait_for_log_line("Browser session process finished");
}

/// Позиция, которую страница называет, пока поколение-файл открывается (как при
/// переключении на `<video src>` посреди просмотра).
const FILE_PAGE_POSITION_US: u64 = 2_000_000;

/// Поколение-файл стартует с позиции страницы: демуксер файла фоновый и синхронно не
/// перематывается, поэтому плеер перематывается командой при первом показе
/// (`AfterLoad::SeekTo`, 03e). Без неё файл играл бы с нуля — все 100 кадров.
#[test]
fn a_file_generation_starts_at_the_page_position() {
    file_generation_starts_at_the_page_position(VideoMode::Headless);
}

/// То же в окне: перемотка при первом показе идёт через пульт плеера окна.
#[test]
fn a_file_generation_starts_at_the_page_position_in_a_window() {
    file_generation_starts_at_the_page_position(VideoMode::Window);
}

fn file_generation_starts_at_the_page_position(mode: VideoMode) {
    let Some(process) = SessionProcessUnderTest::start("file-page-position", mode) else {
        return;
    };
    let bus = &process.bus;
    let mut socket = process.open_attached_session();
    let page_seek = format!(
        r#"{{"type":"seek_started","target_us":{FILE_PAGE_POSITION_US},"cause":{{"kind":"page_seek"}}}}"#
    );
    for text in [
        r#"{"type":"generation_started","generation":1}"#,
        r#"{"type":"file_source_added","generation":1}"#,
        page_seek.as_str(),
    ] {
        socket.send(Message::text(text)).expect("message is sent");
    }
    let messages = FilePage::new().serve_until_ended(&mut socket);

    // Часы «играет» начинаются с позиции страницы и ниже неё не бывают: страница не
    // видит отката к началу файла (шаг часов 250 мс — окно с запасом).
    let playing: Vec<u64> = messages
        .iter()
        .filter_map(|message| match message {
            PlayerSessionMessage::Clock {
                position_us,
                state: ClockState::Playing,
                ..
            } => Some(position_us.as_micros()),
            _ => None,
        })
        .collect();
    assert!(
        playing.first().is_some_and(|&first| {
            (FILE_PAGE_POSITION_US..FILE_PAGE_POSITION_US + 400_000).contains(&first)
        }),
        "the first playing clock is not right after the page position: {playing:?}"
    );
    assert!(
        playing
            .iter()
            .all(|&position| position + 100_000 >= FILE_PAGE_POSITION_US),
        "a playing clock before the page position: {playing:?}"
    );
    send_tab_gone_and_expect_close(&mut socket);
    let summary = bus.wait_for_log_line("Browser playback summary kind=final");
    // Показано примерно то, что после позиции страницы (2 с из 4 — 50 кадров); с нуля
    // было бы все 100. Запас — кадры до перемотки при первом показе.
    let presented = log_number(&summary, "presented_frames");
    assert!(
        (40..=65).contains(&presented),
        "expected about half of {FILE_FRAMES} frames: {summary}"
    );
    bus.wait_for_log_line("Browser session process finished");
}

/// Настройки `[network]` пользователя доходят до источника файла через процесс:
/// первый кусок предзагрузки из config-а задаёт размер первого `fetch_range`
/// (умолчание — 64 КиБ). Ломается любое звено: чтение config-а, перевод в политику,
/// фабрика файлов.
#[test]
fn the_network_settings_of_the_user_size_the_file_requests() {
    let mut config = fastiplayer_config::AppConfig::default();
    config.network.prefetch_initial_chunk_kb = 8;
    config.network.prefetch_chunk_mb = 1;
    config.network.read_ahead_mb = 4;
    let config_toml = config.to_pretty_toml().expect("config text");
    let bus = PrivateBusWithBrowserSession::start_with_config("file-config", Some(&config_toml));
    let mut socket = open_attached_session(&bus);
    for text in [
        r#"{"type":"generation_started","generation":1}"#,
        r#"{"type":"file_source_added","generation":1}"#,
    ] {
        socket.send(Message::text(text)).expect("message is sent");
    }
    let mut page = FilePage::new();

    let messages = read_player_messages_until(&mut socket, "the first fetch_range", |message| {
        matches!(message, PlayerSessionMessage::FetchRange { .. })
    });
    page.answer(&mut socket, messages.last().expect("fetch_range"));

    assert_eq!(
        page.requests.first().map(|request| (request.1, request.2)),
        Some((0, 8 * 1024)),
        "the first request must be the 8 KiB chunk from [network]"
    );
    assert!(
        page.requests.iter().all(|request| request.2 <= 1024 * 1024),
        "no request is longer than prefetch_chunk_mb: {:?}",
        page.requests
    );
    send_tab_gone_and_expect_close(&mut socket);
}

/// Записанный поток MSE БЕЗ `end_of_stream` страницы: эту строку тест вырезает —
/// конец потока должен поставить сам процесс, когда сеанс кончится.
fn recorded_stream_without_end_of_stream() -> Vec<StreamStep> {
    let (steps, _expected_bytes) = recorded_stream("two-source-buffers-mid-box-cuts");
    steps
        .into_iter()
        .filter(|step| match step {
            StreamStep::Text(text) => !text.contains("end_of_stream"),
            StreamStep::Binary(_) | StreamStep::AwaitClock(_) => true,
        })
        .collect()
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

/// Вкладка ушла, а плеер ещё не доиграл: процесс жив, кадры показываются уже ПОСЛЕ ухода
/// вкладки (все 10), и только потом — выход по «доиграно».
#[test]
fn the_process_plays_what_it_got_after_the_tab_is_gone_and_then_exits() {
    plays_what_it_got_after_the_tab_is_gone_and_then_exits(VideoMode::Headless);
}

/// То же в окне: доигрывание читает плеер окна, а не безоконный.
#[test]
fn the_window_plays_what_it_got_after_the_tab_is_gone_and_then_exits() {
    plays_what_it_got_after_the_tab_is_gone_and_then_exits(VideoMode::Window);
}

fn plays_what_it_got_after_the_tab_is_gone_and_then_exits(mode: VideoMode) {
    let Some(process) = SessionProcessUnderTest::start("tab-gone-drain", mode) else {
        return;
    };
    let bus = &process.bus;
    let mut socket = process.open_attached_session();
    send_steps(&mut socket, recorded_stream_without_end_of_stream());

    // Ждём, когда плеер реально начал (первые часы), и убеждаемся, что он ещё не кончил.
    let messages = read_player_messages_until(&mut socket, "the first clock", |message| {
        matches!(message, PlayerSessionMessage::Clock { .. })
    });
    assert!(
        !matches!(
            messages.last(),
            Some(PlayerSessionMessage::Clock {
                state: ClockState::Ended,
                ..
            })
        ),
        "the playback must still be going when the tab leaves: {messages:?}"
    );
    send_tab_gone_and_expect_close(&mut socket);

    // Порядок строк журнала: сеанс кончился → плеер доиграл → доигрывание закончилось
    // → процесс вышел. Если бы процесс вышел сразу, «плеер доиграл» не появилось бы.
    bus.wait_for_log_line("Browser session ended");
    bus.wait_for_log_line("Browser playback ended");
    bus.wait_for_log_line("Playback drain finished class=played_to_end");
    let summary = bus.wait_for_log_line("Browser playback summary kind=final");
    assert_eq!(
        log_number(&summary, "presented_frames"),
        FIXTURE_VIDEO_FRAMES,
        "every frame must be shown even though the tab left: {summary}"
    );
    assert_eq!(log_number(&summary, "position_regressions"), 0, "{summary}");
    assert!(log_number(&summary, "audio_played_ms") > 0, "{summary}");
    bus.wait_for_log_line("Browser session process finished");
}

/// Страница на паузе в момент ухода вкладки: окна, где зритель мог бы нажать «играть»,
/// в 03c нет, процесс выходит сразу, не доигрывая.
#[test]
fn a_paused_page_makes_the_process_exit_at_once_when_the_tab_is_gone() {
    let bus = PrivateBusWithBrowserSession::start("tab-gone-paused");
    let mut socket = open_attached_session(&bus);
    let pause = std::fs::read_to_string(
        Path::new(FIXTURES).join("session/valid/page-page-playback-pause.json"),
    )
    .expect("pause fixture");
    socket
        .send(Message::text(pause.trim_end()))
        .expect("pause is sent");
    send_steps(&mut socket, recorded_stream_without_end_of_stream());
    // Плеер загрузил видео на паузе: часы идут, но не двигаются.
    let messages = read_player_messages_until(&mut socket, "a paused clock", |message| {
        matches!(
            message,
            PlayerSessionMessage::Clock {
                state: ClockState::Paused,
                ..
            }
        )
    });
    assert!(!messages.is_empty());

    send_tab_gone_and_expect_close(&mut socket);

    bus.wait_for_log_line("Playback drain finished class=page_paused");
    let summary = bus.wait_for_log_line("Browser playback summary kind=final");
    assert!(
        log_number(&summary, "presented_frames") < FIXTURE_VIDEO_FRAMES,
        "a paused video is not played after the tab left: {summary}"
    );
    bus.wait_for_log_line("Browser session process finished");
}

/// Всплеск и `tab_gone` без паузы: событие конца сеанса приходит процессу раньше, чем
/// поток приёма разобрал поколение. Видео всё равно проигрывается целиком — процесс
/// ждёт, пока вход экземпляра разобран, а не решает «нечего играть» по голому событию.
#[test]
fn a_burst_followed_at_once_by_tab_gone_is_still_played() {
    let bus = PrivateBusWithBrowserSession::start("burst-tab-gone");
    let mut socket = open_attached_session(&bus);
    send_steps(&mut socket, recorded_stream_without_end_of_stream());

    // Без ожидания первых часов: вкладка уходит сразу за последним байтом.
    send_tab_gone_and_expect_close(&mut socket);

    bus.wait_for_log_line("Playback drain finished class=played_to_end");
    let summary = bus.wait_for_log_line("Browser playback summary kind=final");
    assert_eq!(
        log_number(&summary, "presented_frames"),
        FIXTURE_VIDEO_FRAMES,
        "{summary}"
    );
    assert_eq!(log_number(&summary, "position_regressions"), 0, "{summary}");
    bus.wait_for_log_line("Browser session process finished");
}

/// Пауза страницы, стоящая в том же всплеске перед `tab_gone`, не теряется: выход с
/// классом «пауза», а не «играть».
#[test]
fn a_pause_inside_the_burst_before_tab_gone_is_not_lost() {
    let bus = PrivateBusWithBrowserSession::start("burst-pause-tab-gone");
    let mut socket = open_attached_session(&bus);
    let pause = std::fs::read_to_string(
        Path::new(FIXTURES).join("session/valid/page-page-playback-pause.json"),
    )
    .expect("pause fixture");
    socket
        .send(Message::text(pause.trim_end()))
        .expect("pause is sent");
    send_steps(&mut socket, recorded_stream_without_end_of_stream());

    send_tab_gone_and_expect_close(&mut socket);

    bus.wait_for_log_line("Playback drain finished class=page_paused");
    bus.wait_for_log_line("Browser session process finished");
}

/// `file_source_added` после `source_buffer_added` закрывает сервер кодом 4003 ещё до
/// приёмников (сторож поколений, `FileSourceAfterSourceBuffer`): поколение с MSE не
/// становится файлом, поэтому загруженное поколение сегментов файлом не заменяется.
#[test]
fn a_file_announced_after_a_source_buffer_is_closed_by_the_gate() {
    let bus = PrivateBusWithBrowserSession::start("file-after-buffer");
    let mut socket = open_attached_session(&bus);
    for text in [
        r#"{"type":"generation_started","generation":1}"#,
        r#"{"type":"source_buffer_added","generation":1,"source_buffer":0,"mime_type":"video/mp4; codecs=\"avc1.42c00a\""}"#,
        r#"{"type":"file_source_added","generation":1}"#,
    ] {
        socket.send(Message::text(text)).expect("message is sent");
    }

    assert_eq!(
        read_close_code(&mut socket),
        CloseCode::from(SessionCloseReason::ProtocolViolation.close_code())
    );
}
