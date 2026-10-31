//! Сквозной тест процесса браузерного режима (мост в браузер, сессии 02 и 03b).
//!
//! Настоящий бинарь `fastiplayer` будит частная шина D-Bus по `.service`-файлу —
//! так же, как его разбудит посредник экстеншена. Тест играет роль посредника и
//! background экстеншена:
//! 1. звонок на незапущенное имя → шина запускает `fastiplayer --browser-session`;
//! 2. `hello` → `hello_accepted`, `open_session` → порт и пропуск;
//! 3. WebSocket на 127.0.0.1: `attach` → `attached`;
//! 4. записанный поток из фикстур протокола (fMP4, два SourceBuffer, разрезы посреди
//!    боксов, `end_of_stream`) играет НАСТОЯЩИЙ плеер без окна: страница получает
//!    часы до состояния `ended` и состояние буфера, журнал — каждый кадр и звук;
//! 5. `tab_gone` → код закрытия 4002, процесс выходит сам и пишет итог плеера и
//!    сколько байт дошло — ровно размер исходных медиафайлов.
//!
//! Второй тест — отказ: защищённое видео (`encv`) → странице `stream_rejected`,
//! процесс живёт дальше до `tab_gone`.
//!
//! Требует `dbus-daemon` (как и прочие тесты шины в проекте). Настоящий рабочий стол
//! не трогается: у частной шины свой каталог служб, свой адрес и нет других служб;
//! каталоги XDG процесса — временные (config не читается из домашнего), звук идёт в
//! тихую карту-симулятор (`FASTIPLAYER_TEST_BROWSER_AUDIO=simulated`).

#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::panic,
    reason = "integration-тест: падение с понятным сообщением и есть результат"
)]

use std::io::{BufRead, BufReader};
use std::path::{Path, PathBuf};
use std::process::{Child, Command, Stdio};
use std::sync::mpsc;
use std::time::{Duration, Instant};

use std::net::TcpStream;

use browser_bridge_protocol::session::player::{ClockState, StreamRejection};
use browser_bridge_protocol::{
    AppendSequence, DataFrameHeader, PlayerControlMessage, PlayerSessionMessage,
    SessionCloseReason, SessionGrant, SourceBufferBytesHeader, SourceBufferIndex, StreamGeneration,
    decode_message, encode_data_frame,
};
use desktop_integration::{
    BrowserEntryBus, BrowserEntryClient, BrowserEntryClientConfig,
    browser_session_activation_service_file,
};
use tungstenite::protocol::frame::coding::CloseCode;
use tungstenite::stream::MaybeTlsStream;
use tungstenite::{Message, WebSocket};

/// Потолок ожидания: процесс поднимается и выходит за доли секунды, запас на CI.
const WAIT: Duration = Duration::from_secs(30);

const FIXTURES: &str = concat!(
    env!("CARGO_MANIFEST_DIR"),
    "/../browser-bridge-protocol/fixtures"
);

const HELLO: &str = r#"{"type":"hello","protocol":{"major":1,"minor":0},"software_version":"0.1.0","capabilities":[]}"#;
const OPEN_SESSION: &str = r#"{"type":"open_session","request_id":1,"session_key":"tab-12-video-3","page":{"title":"Episode 3","domain":"example.org"},"intent":"window","hints":{"duration":{"kind":"unknown"},"position":{"kind":"unknown"}}}"#;

/// Частная шина с каталогом служб, где лежит `.service` настоящего бинаря.
struct PrivateBusWithBrowserSession {
    address: String,
    daemon: Child,
    /// Строки stderr шины: туда же пишут запущенные ею службы (наш процесс).
    log_lines: mpsc::Receiver<String>,
    directory: PathBuf,
}

impl PrivateBusWithBrowserSession {
    /// `name` различает каталоги тестов одного процесса (тесты идут параллельно).
    fn start(name: &str) -> Self {
        Self::start_with_config(name, None)
    }

    /// То же, но с `config.toml` пользователя в каталоге XDG процесса (процесс его
    /// только читает).
    fn start_with_config(name: &str, config_toml: Option<&str>) -> Self {
        Self::start_with(name, config_toml, &TestVideo::Headless)
    }

    /// Общий подъём: каталоги, шина и как процесс показывает видео.
    fn start_with(name: &str, config_toml: Option<&str>, video: &TestVideo) -> Self {
        Self::start_with_env(name, config_toml, video, &[])
    }

    /// То же с тестовыми переменными процесса (например, короткий срок застоя
    /// доигрывания `FASTIPLAYER_TEST_BROWSER_DRAIN_STALL_MS`).
    fn start_with_env(
        name: &str,
        config_toml: Option<&str>,
        video: &TestVideo,
        process_env: &[(&str, &str)],
    ) -> Self {
        let directory = std::env::temp_dir().join(format!(
            "fastiplayer-browser-session-e2e-{}-{name}",
            std::process::id()
        ));
        let services = directory.join("services");
        std::fs::create_dir_all(&services).expect("service directory");
        let service_file =
            browser_session_activation_service_file(Path::new(env!("CARGO_BIN_EXE_fastiplayer")))
                .expect("binary path is quotable");
        std::fs::write(
            services.join("io.github.Bogdan7c.Fastiplayer.BrowserSession.service"),
            service_file,
        )
        .expect("service file");

        let socket = directory.join("bus");
        let address = format!("unix:path={}", socket.display());
        let config = directory.join("bus.conf");
        std::fs::write(&config, bus_config(&address, &services)).expect("bus config");

        // Свои каталоги XDG: процесс не прочтёт config.toml разработчика (и не
        // напишет ничего вне временного каталога).
        let xdg = |kind: &str| {
            let path = directory.join("xdg").join(kind);
            std::fs::create_dir_all(&path).expect("XDG directory");
            path
        };
        if let Some(config_toml) = config_toml {
            // Каталог приложения в XDG config — `fastiplayer` (константа слоя config).
            let app_directory = xdg("config").join("fastiplayer");
            std::fs::create_dir_all(&app_directory).expect("config directory");
            std::fs::write(app_directory.join("config.toml"), config_toml).expect("config.toml");
        }
        let home = directory.join("home");
        std::fs::create_dir_all(&home).expect("HOME directory");
        let mut daemon_command = Command::new("dbus-daemon");
        match video {
            // Без окна: на CI нет дисплея; окно проверяет `window.rs` этого набора.
            TestVideo::Headless => {
                daemon_command.env("FASTIPLAYER_TEST_BROWSER_VIDEO", "headless");
            }
            // Окно на частном виртуальном дисплее: настоящий рабочий стол не трогается.
            TestVideo::Window(display) => {
                daemon_command
                    .env("WAYLAND_DISPLAY", display.socket_name())
                    .env("XDG_RUNTIME_DIR", display.runtime_dir())
                    .env_remove("DISPLAY");
            }
        }
        for (variable, value) in process_env {
            daemon_command.env(variable, value);
        }
        let mut daemon = daemon_command
            .arg(format!("--config-file={}", config.display()))
            .args(["--nofork", "--print-address=1"])
            // Запущенный шиной процесс наследует окружение демона: и он, и его
            // клиент паузы обычного плеера пойдут на эту частную шину.
            .env("DBUS_SESSION_BUS_ADDRESS", &address)
            .env("XDG_CONFIG_HOME", xdg("config"))
            .env("XDG_DATA_HOME", xdg("data"))
            .env("XDG_STATE_HOME", xdg("state"))
            .env("XDG_CACHE_HOME", xdg("cache"))
            // Звук — в тихую карту с настоящим временем: на CI звуковой карты нет.
            .env("FASTIPLAYER_TEST_BROWSER_AUDIO", "simulated")
            .env("HOME", &home)
            // Ручная отладка: `FASTIPLAYER_TEST_PROCESS_RUST_LOG` заменяет фильтр журнала.
            .env(
                "RUST_LOG",
                std::env::var("FASTIPLAYER_TEST_PROCESS_RUST_LOG")
                    .unwrap_or_else(|_| "info".into()),
            )
            // Без цветовых escape-кодов: тест ищет в логе текст `received_bytes=N`.
            .env("NO_COLOR", "1")
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .spawn()
            .expect("dbus-daemon must be installed for this test");
        let stdout = daemon.stdout.take().expect("address pipe");
        let printed_address = BufReader::new(stdout)
            .lines()
            .next()
            .expect("dbus-daemon prints its address")
            .expect("readable address");
        assert!(printed_address.starts_with("unix:"), "{printed_address}");

        let stderr = daemon.stderr.take().expect("log pipe");
        let (line_sender, log_lines) = mpsc::channel();
        // Ручная проверка: `FASTIPLAYER_TEST_ECHO_PROCESS_LOG=1 … -- --nocapture` печатает
        // журнал процесса целиком (например, для отчёта живого прогона окна).
        let echo = std::env::var_os("FASTIPLAYER_TEST_ECHO_PROCESS_LOG").is_some();
        std::thread::spawn(move || {
            for line in BufReader::new(stderr).lines().map_while(Result::ok) {
                if echo {
                    eprintln!("[process] {line}");
                }
                if line_sender.send(line).is_err() {
                    break;
                }
            }
        });
        Self {
            address,
            daemon,
            log_lines,
            directory,
        }
    }

    /// Ждёт строку лога с `needle` и возвращает её.
    fn wait_for_log_line(&self, needle: &str) -> String {
        self.wait_for_log_line_without(needle, &[])
    }

    /// Читает журнал `period` и падает, если за это время появилась строка с любой из
    /// `forbidden` (процесс жив и ничего такого не решил).
    fn expect_no_log_line_for(&self, period: Duration, forbidden: &[&str]) {
        let deadline = Instant::now() + period;
        loop {
            let left = deadline.saturating_duration_since(Instant::now());
            if left.is_zero() {
                return;
            }
            match self.log_lines.recv_timeout(left) {
                Ok(line) => {
                    if let Some(found) = forbidden.iter().find(|text| line.contains(**text)) {
                        panic!("unexpected log line ({found:?}) within {period:?}: {line}");
                    }
                }
                Err(mpsc::RecvTimeoutError::Timeout) => return,
                Err(mpsc::RecvTimeoutError::Disconnected) => {
                    panic!("the process log ended within {period:?}: the process is gone")
                }
            }
        }
    }

    /// То же, но строки, прочитанные по дороге, не должны содержать ни одной из
    /// `forbidden`: так тест видит, что процесс ни разу не отказал молча (отказ виден
    /// только в журнале, а старое медиа при этом продолжает играть).
    fn wait_for_log_line_without(&self, needle: &str, forbidden: &[&str]) -> String {
        let deadline = Instant::now() + WAIT;
        loop {
            let left = deadline.saturating_duration_since(Instant::now());
            match self.log_lines.recv_timeout(left) {
                Ok(line) if line.contains(needle) => return line,
                Ok(line) => {
                    if let Some(found) = forbidden.iter().find(|text| line.contains(**text)) {
                        panic!("forbidden log line ({found:?}) before {needle:?}: {line}");
                    }
                }
                Err(error) => panic!("no log line with {needle:?}: {error}"),
            }
        }
    }
}

impl Drop for PrivateBusWithBrowserSession {
    fn drop(&mut self) {
        let _ = self.daemon.kill();
        let _ = self.daemon.wait();
        let _ = std::fs::remove_dir_all(&self.directory);
    }
}

/// Как процесс браузерного режима показывает видео в тесте.
enum TestVideo {
    /// Без окна (тестовая переменная `FASTIPLAYER_TEST_BROWSER_VIDEO=headless`).
    Headless,
    /// Окно на частном виртуальном дисплее.
    Window(window::VirtualWaylandDisplay),
}

/// Режим показа — параметр сквозных тестов, которые обязаны проходить и без окна, и в
/// окне. Пути загрузки у режимов разные (безоконный плеер против пути открытия
/// `AppState` окна): смена поколения 03e проходила без окна и отвергалась в окне (04a).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum VideoMode {
    Headless,
    Window,
}

/// Процесс браузерного режима под тестом вместе с тем, на чём он показывает видео.
///
/// Порядок полей — порядок уничтожения: шина (и процесс) уходят раньше дисплея.
struct SessionProcessUnderTest {
    bus: PrivateBusWithBrowserSession,
    _video: TestVideo,
    mode: VideoMode,
}

impl SessionProcessUnderTest {
    /// Поднимает шину (и дисплей для окна). `None` — окна не будет: `kwin_wayland`
    /// нет, тест пропускается с сообщением (как `window.rs`).
    fn start(name: &str, mode: VideoMode) -> Option<Self> {
        Self::start_with_env(name, mode, &[])
    }

    /// То же с тестовыми переменными процесса.
    fn start_with_env(name: &str, mode: VideoMode, process_env: &[(&str, &str)]) -> Option<Self> {
        let video = match mode {
            VideoMode::Headless => TestVideo::Headless,
            VideoMode::Window => TestVideo::Window(window::VirtualWaylandDisplay::start(name)?),
        };
        let bus_name = match mode {
            VideoMode::Headless => format!("{name}-headless"),
            VideoMode::Window => format!("{name}-window"),
        };
        let bus =
            PrivateBusWithBrowserSession::start_with_env(&bus_name, None, &video, process_env);
        Some(Self {
            bus,
            _video: video,
            mode,
        })
    }

    /// Виртуальный дисплей окна (`None` — безоконный режим).
    fn display(&self) -> Option<&window::VirtualWaylandDisplay> {
        match &self._video {
            TestVideo::Window(display) => Some(display),
            TestVideo::Headless => None,
        }
    }

    /// Открывает сеанс, как экстеншен; в режиме окна ещё ждёт, пока окно поднимется.
    fn open_attached_session(&self) -> WebSocket<MaybeTlsStream<TcpStream>> {
        let socket = open_attached_session(&self.bus);
        if self.mode == VideoMode::Window {
            self.bus.wait_for_log_line("Browser window ready");
        }
        socket
    }
}

fn bus_config(address: &str, services: &Path) -> String {
    format!(
        r#"<!DOCTYPE busconfig PUBLIC "-//freedesktop//DTD D-Bus Bus Configuration 1.0//EN"
 "http://www.freedesktop.org/standards/dbus/1.0/busconfig.dtd">
<busconfig>
  <type>session</type>
  <listen>{address}</listen>
  <auth>EXTERNAL</auth>
  <servicedir>{services}</servicedir>
  <policy context="default">
    <allow send_destination="*" eavesdrop="true"/>
    <allow eavesdrop="true"/>
    <allow own="*"/>
  </policy>
</busconfig>
"#,
        services = services.display()
    )
}

/// Шаг записанного потока: текстовое сообщение, двоичная рамка или пауза записи.
enum StreamStep {
    Text(String),
    Binary(Vec<u8>),
    /// Страница ждала, пока часы плеера дойдут до этой позиции (мкс).
    AwaitClock(u64),
}

/// Читает шаги и ожидаемое число принятых байт записанного потока фикстур.
fn recorded_stream(name: &str) -> (Vec<StreamStep>, u64) {
    let directory = Path::new(FIXTURES).join("streams").join(name);
    let manifest: serde_json::Value = serde_json::from_slice(
        &std::fs::read(directory.join("stream.json")).expect("stream manifest"),
    )
    .expect("stream manifest is JSON");
    let steps = manifest["steps"]
        .as_array()
        .expect("steps array")
        .iter()
        .map(|step| {
            if let Some(position_us) = step["await_clock_us"].as_u64() {
                StreamStep::AwaitClock(position_us)
            } else if let Some(file) = step["text"].as_str() {
                let text = std::fs::read_to_string(directory.join(file)).expect("text step");
                StreamStep::Text(text.trim_end().to_owned())
            } else {
                let file = step["binary"].as_str().expect("step is text or binary");
                StreamStep::Binary(std::fs::read(directory.join(file)).expect("binary step"))
            }
        })
        .collect();
    let expected_bytes = manifest["expect"]["admitted"]
        .as_array()
        .expect("admitted list")
        .iter()
        .flat_map(|admitted| admitted["ranges"].as_array().expect("ranges").iter())
        .map(|range| {
            let start = range[0].as_u64().expect("range start");
            let end = range[1].as_u64().expect("range end");
            end - start
        })
        .sum();
    (steps, expected_bytes)
}

fn entry_call(client: &BrowserEntryClient, message: &str) -> PlayerControlMessage {
    let reply = client.call(message).expect("the browser session answers");
    decode_message(reply.as_bytes()).expect("reply is a player control message")
}

/// Будит процесс, открывает сеанс и подключается к нему, как экстеншен.
fn open_attached_session(
    bus: &PrivateBusWithBrowserSession,
) -> WebSocket<MaybeTlsStream<TcpStream>> {
    open_attached_session_with(bus, OPEN_SESSION)
}

/// То же с этим `open_session` (другой ключ — другая вкладка: замена сеанса).
fn open_attached_session_with(
    bus: &PrivateBusWithBrowserSession,
    open_session: &str,
) -> WebSocket<MaybeTlsStream<TcpStream>> {
    // Звонок будит незапущенный процесс; одно соединение = один звонящий.
    let client = BrowserEntryClient::connect(
        BrowserEntryBus::Address(bus.address.clone()),
        BrowserEntryClientConfig::default(),
    )
    .expect("D-Bus activation starts fastiplayer --browser-session");
    assert!(matches!(
        entry_call(&client, HELLO),
        PlayerControlMessage::HelloAccepted(_)
    ));
    let PlayerControlMessage::SessionGranted(grant) = entry_call(&client, open_session) else {
        panic!("open_session must be granted");
    };
    bus.wait_for_log_line("Browser session opened");
    // Посредник может умереть посреди сеанса: дальше он не нужен (решение 85).
    drop(client);
    let socket = attach(&grant);
    // Чтение не должно висеть вечно, если процесс замолчал.
    if let MaybeTlsStream::Plain(stream) = socket.get_ref() {
        stream
            .set_read_timeout(Some(WAIT))
            .expect("read timeout is set");
    }
    socket
}

/// Читает сообщения плеера странице, пока `stop` не скажет «хватит»; возвращает всё
/// прочитанное (включая последнее).
fn read_player_messages_until(
    socket: &mut WebSocket<MaybeTlsStream<TcpStream>>,
    what: &str,
    mut stop: impl FnMut(&PlayerSessionMessage) -> bool,
) -> Vec<PlayerSessionMessage> {
    let deadline = Instant::now() + WAIT;
    let mut messages = Vec::new();
    loop {
        assert!(Instant::now() < deadline, "the page never received {what}");
        match socket.read() {
            Ok(Message::Text(text)) => {
                let message: PlayerSessionMessage =
                    decode_message(text.as_bytes()).expect("player session message");
                let done = stop(&message);
                messages.push(message);
                if done {
                    return messages;
                }
            }
            Ok(_) => continue,
            Err(error) => panic!("waiting for {what}: {error}"),
        }
    }
}

/// Число после `field=` в строке журнала.
fn log_number(line: &str, field: &str) -> u64 {
    let needle = format!(" {field}=");
    let start = line
        .find(&needle)
        .unwrap_or_else(|| panic!("no {field} in: {line}"))
        + needle.len();
    line[start..]
        .split(|character: char| !character.is_ascii_digit())
        .next()
        .and_then(|digits| digits.parse().ok())
        .unwrap_or_else(|| panic!("{field} is not a number in: {line}"))
}

fn send_tab_gone_and_expect_close(socket: &mut WebSocket<MaybeTlsStream<TcpStream>>) {
    socket
        .send(Message::text(
            r#"{"type":"tab_gone","reason":"navigated_away"}"#,
        ))
        .expect("tab_gone is sent");
    assert_eq!(
        read_close_code(socket),
        CloseCode::from(SessionCloseReason::TabGone.close_code())
    );
}

/// Подключается к процессу браузерного режима и предъявляет пропуск.
fn attach(grant: &SessionGrant) -> WebSocket<MaybeTlsStream<TcpStream>> {
    let (mut socket, _response) =
        tungstenite::connect(format!("ws://127.0.0.1:{}/", grant.port.get()))
            .expect("loopback WebSocket connects");
    socket
        .send(Message::text(format!(
            r#"{{"type":"attach","session_key":"{}","token":"{}"}}"#,
            grant.session_key.as_str(),
            grant.token.as_str()
        )))
        .expect("attach is sent");
    let attached = socket.read().expect("attach is answered");
    assert_eq!(
        attached.into_text().expect("text frame").as_str(),
        r#"{"type":"attached"}"#
    );
    socket
}

fn read_close_code<Stream: std::io::Read + std::io::Write>(
    socket: &mut WebSocket<Stream>,
) -> CloseCode {
    loop {
        match socket.read() {
            Ok(Message::Close(Some(frame))) => return frame.code,
            Ok(_) => continue,
            Err(error) => panic!("expected a close frame, got {error}"),
        }
    }
}

/// Кадров в записанном ролике фикстуры (25 к/с, 0–360 мс; так же считает тест
/// «до кадра» `pushed-media-source/tests/recorded_streams_to_frame.rs`).
const FIXTURE_VIDEO_FRAMES: u64 = 10;

#[test]
fn bus_wakes_the_real_binary_which_plays_the_recorded_stream_to_the_end() {
    let bus = PrivateBusWithBrowserSession::start("plays");
    let mut socket = open_attached_session(&bus);

    // 4. Записанный поток целиком (включая `end_of_stream`).
    let (steps, expected_bytes) = recorded_stream("two-source-buffers-mid-box-cuts");
    for step in steps {
        let message = match step {
            StreamStep::Text(text) => Message::text(text),
            StreamStep::Binary(bytes) => Message::binary(bytes),
            StreamStep::AwaitClock(_) => panic!("this recording has no pauses"),
        };
        socket.send(message).expect("stream step is sent");
    }

    // Страница видит часы плеера до конца ролика и состояние буфера.
    let messages = read_player_messages_until(&mut socket, "the clock state ended", |message| {
        matches!(
            message,
            PlayerSessionMessage::Clock {
                state: ClockState::Ended,
                ..
            }
        )
    });
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
        clock_positions.last().is_some_and(|last| *last >= 360_000),
        "the clock never reached the last frame: {clock_positions:?}"
    );
    let buffer_states = messages
        .iter()
        .filter(|message| matches!(message, PlayerSessionMessage::BufferState { .. }))
        .count();
    assert!(buffer_states >= 1, "no buffer_state in {messages:?}");
    bus.wait_for_log_line("Browser playback ended");

    // 5. Вкладка ушла: сеанс закрыт, процесс подводит итог и выходит.
    send_tab_gone_and_expect_close(&mut socket);
    let summary = bus.wait_for_log_line("Browser playback summary kind=final");
    assert_eq!(
        log_number(&summary, "presented_frames"),
        FIXTURE_VIDEO_FRAMES,
        "{summary}"
    );
    assert_eq!(log_number(&summary, "position_regressions"), 0, "{summary}");
    assert!(log_number(&summary, "audio_played_ms") > 0, "{summary}");
    let totals = bus.wait_for_log_line("Browser session stopped");
    assert!(
        totals.contains(&format!("received_bytes={expected_bytes}")),
        "expected {expected_bytes} bytes in: {totals}"
    );
    bus.wait_for_log_line("Browser session process finished");
}

/// fMP4 фикстуры протокола с `avc1` в описании образца, переименованным в `encv`
/// (так выглядит зашифрованное видео: Vimeo, `results/00c.md`).
fn protected_video_mp4() -> Vec<u8> {
    let mut file = std::fs::read(Path::new(FIXTURES).join("media/video.mp4")).expect("video.mp4");
    let moov = file
        .windows(4)
        .position(|window| window == b"moov")
        .expect("moov box");
    let entry = file[moov..]
        .windows(4)
        .position(|window| window == b"avc1")
        .expect("avc1 sample entry")
        + moov;
    file[entry..entry + 4].copy_from_slice(b"encv");
    file
}

#[test]
fn protected_stream_is_rejected_to_the_page_and_the_process_keeps_the_session() {
    let bus = PrivateBusWithBrowserSession::start("rejects");
    let mut socket = open_attached_session(&bus);

    for text in [
        r#"{"type":"generation_started","generation":1}"#,
        r#"{"type":"source_buffer_added","generation":1,"source_buffer":0,"mime_type":"video/mp4; codecs=\"avc1.42c00a\""}"#,
    ] {
        socket.send(Message::text(text)).expect("message is sent");
    }
    let header = DataFrameHeader::SourceBufferBytes(SourceBufferBytesHeader {
        generation: StreamGeneration::FIRST,
        source_buffer: SourceBufferIndex::new(0).expect("source buffer"),
        sequence: AppendSequence(0),
    });
    let frame = encode_data_frame(&header, &protected_video_mp4()).expect("data frame");
    socket.send(Message::binary(frame)).expect("bytes are sent");

    let messages = read_player_messages_until(&mut socket, "stream_rejected", |message| {
        matches!(message, PlayerSessionMessage::StreamRejected { .. })
    });
    assert_eq!(
        messages.last(),
        Some(&PlayerSessionMessage::StreamRejected {
            generation: StreamGeneration::FIRST,
            reason: StreamRejection::ProtectedContent,
        })
    );
    // Отказ от поколения — не конец сеанса (следующее поколение может оказаться
    // играбельным): процесс жив и отвечает на `tab_gone` своим кодом 4002, а не
    // закрытием 4003 (нарушение) или 4006 (выключение).
    send_tab_gone_and_expect_close(&mut socket);
    let totals = bus.wait_for_log_line("Browser session stopped");
    assert!(totals.contains("received_frames=1"), "{totals}");
    bus.wait_for_log_line("Browser session process finished");
}

/// Живая проверка на настоящей сессионной шине (dbus-daemon или dbus-broker).
///
/// Перед запуском: `.service`-файл установлен (`scripts/install-desktop-entry.sh`
/// или кнопка «Connect to browsers» из сессии 06), процесс браузерного режима не
/// запущен. Если запущен обычный Fastiplayer, он встанет на паузу — так и задумано.
/// Запуск: `cargo test -p app-egui --test browser_session_process -- --ignored`.
#[test]
#[ignore = "needs the activation .service installed on the real session bus"]
fn live_session_bus_wakes_the_installed_browser_session() {
    let client = BrowserEntryClient::connect(
        BrowserEntryBus::Session,
        BrowserEntryClientConfig::default(),
    )
    .expect("the real session bus wakes fastiplayer --browser-session");
    assert!(matches!(
        entry_call(&client, HELLO),
        PlayerControlMessage::HelloAccepted(_)
    ));
    let PlayerControlMessage::SessionGranted(grant) = entry_call(&client, OPEN_SESSION) else {
        panic!("open_session must be granted");
    };
    let mut socket = attach(&grant);
    send_tab_gone_and_expect_close(&mut socket);
}

// Сквозные тесты 03c (файл `<video src>` и доигрывание) — отдельным модулем, чтобы этот
// файл не рос.
#[path = "browser_session_process/file_and_drain.rs"]
mod file_and_drain;

// Концы сеанса (04b): прощание при замене и закрытии окна, «вкладка ушла» в окне.
#[path = "browser_session_process/session_ends.rs"]
mod session_ends;

// Смена качества новым MediaSource (03e): три смены поколения подряд.
#[path = "browser_session_process/generation_switch.rs"]
mod generation_switch;

// «Помним, пока помнит страница» (04a): перемотка назад дальше окна «назад» — с полки.
#[path = "browser_session_process/seek_back_on_shelf.rs"]
mod seek_back_on_shelf;

// Окно браузерного режима (04a): то же, но с окном на виртуальном дисплее kwin.
#[path = "browser_session_process/window.rs"]
mod window;
