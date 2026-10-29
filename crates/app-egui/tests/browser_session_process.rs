//! Сквозной тест процесса браузерного режима (мост в браузер, сессия 02).
//!
//! Настоящий бинарь `fastiplayer` будит частная шина D-Bus по `.service`-файлу —
//! так же, как его разбудит посредник экстеншена. Тест играет роль посредника и
//! background экстеншена:
//! 1. звонок на незапущенное имя → шина запускает `fastiplayer --browser-session`;
//! 2. `hello` → `hello_accepted`, `open_session` → порт и пропуск;
//! 3. WebSocket на 127.0.0.1: `attach` → `attached`;
//! 4. записанный поток из фикстур протокола (fMP4, два SourceBuffer, разрезы посреди
//!    боксов) → `tab_gone` → код закрытия 4002;
//! 5. процесс выходит сам и пишет, сколько байт дошло до приёмника — ровно размер
//!    исходных медиафайлов.
//!
//! Требует `dbus-daemon` (как и прочие тесты шины в проекте). Настоящий рабочий стол
//! не трогается: у частной шины свой каталог служб, свой адрес и нет других служб.

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

use browser_bridge_protocol::{
    PlayerControlMessage, SessionCloseReason, SessionGrant, decode_message,
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
    fn start() -> Self {
        let directory = std::env::temp_dir().join(format!(
            "fastiplayer-browser-session-e2e-{}",
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

        let mut daemon = Command::new("dbus-daemon")
            .arg(format!("--config-file={}", config.display()))
            .args(["--nofork", "--print-address=1"])
            // Запущенный шиной процесс наследует окружение демона: и он, и его
            // клиент паузы обычного плеера пойдут на эту частную шину.
            .env("DBUS_SESSION_BUS_ADDRESS", &address)
            .env("RUST_LOG", "info")
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
        std::thread::spawn(move || {
            for line in BufReader::new(stderr).lines().map_while(Result::ok) {
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
        let deadline = Instant::now() + WAIT;
        loop {
            let left = deadline.saturating_duration_since(Instant::now());
            match self.log_lines.recv_timeout(left) {
                Ok(line) if line.contains(needle) => return line,
                Ok(_) => continue,
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

/// Шаг записанного потока: текстовое сообщение или двоичная рамка.
enum StreamStep {
    Text(String),
    Binary(Vec<u8>),
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
            if let Some(file) = step["text"].as_str() {
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

#[test]
fn bus_wakes_the_real_binary_and_recorded_bytes_reach_its_receiver() {
    let bus = PrivateBusWithBrowserSession::start();

    // 1–2. Звонок будит незапущенный процесс; одно соединение = один звонящий.
    let client = BrowserEntryClient::connect(
        BrowserEntryBus::Address(bus.address.clone()),
        BrowserEntryClientConfig::default(),
    )
    .expect("D-Bus activation starts fastiplayer --browser-session");
    assert!(matches!(
        entry_call(&client, HELLO),
        PlayerControlMessage::HelloAccepted(_)
    ));
    let PlayerControlMessage::SessionGranted(grant) = entry_call(&client, OPEN_SESSION) else {
        panic!("open_session must be granted");
    };
    bus.wait_for_log_line("Browser session opened");

    // Посредник может умереть посреди сеанса: дальше он не нужен (решение 85).
    drop(client);

    // 3. WebSocket напрямую в процесс браузерного режима.
    let mut socket = attach(&grant);

    // 4. Записанный поток и уход вкладки.
    let (steps, expected_bytes) = recorded_stream("two-source-buffers-mid-box-cuts");
    for step in steps {
        let message = match step {
            StreamStep::Text(text) => Message::text(text),
            StreamStep::Binary(bytes) => Message::binary(bytes),
        };
        socket.send(message).expect("stream step is sent");
    }
    socket
        .send(Message::text(
            r#"{"type":"tab_gone","reason":"navigated_away"}"#,
        ))
        .expect("tab_gone is sent");
    assert_eq!(
        read_close_code(&mut socket),
        CloseCode::from(SessionCloseReason::TabGone.close_code())
    );

    // 5. Процесс сам выходит и отчитывается о принятых байтах.
    let totals = bus.wait_for_log_line("Browser session stopped");
    assert!(
        totals.contains(&format!("received_bytes={expected_bytes}")),
        "expected {expected_bytes} bytes in: {totals}"
    );
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
    socket
        .send(Message::text(
            r#"{"type":"tab_gone","reason":"navigated_away"}"#,
        ))
        .expect("tab_gone is sent");
    assert_eq!(
        read_close_code(&mut socket),
        CloseCode::from(SessionCloseReason::TabGone.close_code())
    );
}
