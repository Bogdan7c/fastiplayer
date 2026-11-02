//! Настоящий бинарь `fastiplayer-browser-host`, запущенный так, как его запускает
//! браузер, но без плеера: что посредник делает с шиной, входом и stderr.
//!
//! «Шина» здесь — Unix-сокет, который только слушает: если посредник к нему
//! подключился, это видно по очереди подключений. Сквозные тесты с настоящим
//! плеером — `app-egui/tests/browser_session_process/native_host.rs`.

#![cfg(unix)]
#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::panic,
    reason = "integration-тест: падение с понятным сообщением и есть результат"
)]

use std::io::{ErrorKind, Read, Write};
use std::os::unix::net::UnixListener;
use std::path::PathBuf;
use std::process::{Child, Command, Stdio};
use std::time::{Duration, Instant};

use browser_bridge_protocol::extension_identity::{
    CHROMIUM_DEVELOPMENT_EXTENSION_ID, FIREFOX_EXTENSION_ID,
};
use browser_bridge_protocol::{
    ControlRefusal, PlayerControlMessage, RefusalReason, ReplyTarget, decode_message,
};

const HELLO: &str = r#"{"type":"hello","protocol":{"major":1,"minor":0},"software_version":"0.1.0","capabilities":[]}"#;

/// Сколько ждём выхода посредника.
const EXIT_WAIT: Duration = Duration::from_secs(10);

/// Слушающий сокет вместо шины в своём временном каталоге.
struct ListeningBus {
    directory: PathBuf,
    listener: UnixListener,
}

impl ListeningBus {
    fn new(name: &str) -> Self {
        let directory = std::env::temp_dir().join(format!(
            "fastiplayer-browser-host-binary-{}-{name}",
            std::process::id()
        ));
        std::fs::create_dir_all(&directory).unwrap();
        let listener = UnixListener::bind(directory.join("bus")).unwrap();
        listener.set_nonblocking(true).unwrap();
        Self {
            directory,
            listener,
        }
    }

    fn address(&self) -> String {
        format!("unix:path={}", self.directory.join("bus").display())
    }

    fn was_contacted(&self) -> bool {
        match self.listener.accept() {
            Ok(_) => true,
            Err(error) if error.kind() == ErrorKind::WouldBlock => false,
            Err(error) => panic!("listener failed: {error}"),
        }
    }
}

impl Drop for ListeningBus {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.directory);
    }
}

fn spawn_host(arguments: &[&str], bus_address: &str) -> Child {
    Command::new(env!("CARGO_BIN_EXE_fastiplayer-browser-host"))
        .args(arguments)
        .env("DBUS_SESSION_BUS_ADDRESS", bus_address)
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .expect("host starts")
}

fn chromium_origin() -> String {
    format!("chrome-extension://{CHROMIUM_DEVELOPMENT_EXTENSION_ID}/")
}

fn frame(body: &str) -> Vec<u8> {
    let mut bytes = u32::try_from(body.len()).unwrap().to_ne_bytes().to_vec();
    bytes.extend_from_slice(body.as_bytes());
    bytes
}

/// Ждёт выхода и возвращает код (`None` — убит сигналом).
fn wait_for_exit(child: &mut Child) -> Option<i32> {
    let deadline = Instant::now() + EXIT_WAIT;
    loop {
        if let Some(status) = child.try_wait().unwrap() {
            return status.code();
        }
        assert!(Instant::now() < deadline, "the host did not exit");
        std::thread::sleep(Duration::from_millis(10));
    }
}

fn read_all(stream: Option<impl Read>) -> Vec<u8> {
    let mut bytes = Vec::new();
    stream.unwrap().read_to_end(&mut bytes).unwrap();
    bytes
}

#[test]
fn a_foreign_extension_exits_with_code_2_without_touching_the_bus() {
    let bus = ListeningBus::new("foreign");
    let mut host = spawn_host(
        &["chrome-extension://aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa/"],
        &bus.address(),
    );
    // Браузер успел бы написать сообщение — посредник его не читает.
    let _ignored_broken_pipe = host.stdin.take().unwrap().write_all(&frame(HELLO));
    assert_eq!(wait_for_exit(&mut host), Some(2));
    assert!(!bus.was_contacted(), "a foreign caller reached the bus");
    let stderr = String::from_utf8(read_all(host.stderr.take())).unwrap();
    assert!(stderr.contains("caller rejected"), "{stderr}");
    assert!(read_all(host.stdout.take()).is_empty());
}

#[test]
fn a_closed_port_ends_the_host_with_code_0_without_touching_the_bus() {
    // Так выглядит закрытие браузера: только EOF, никто не убивает (разведка 00).
    let bus = ListeningBus::new("eof");
    let mut host = spawn_host(
        &["/tmp/manifest.json", FIREFOX_EXTENSION_ID],
        &bus.address(),
    );
    drop(host.stdin.take());
    assert_eq!(wait_for_exit(&mut host), Some(0));
    assert!(!bus.was_contacted());
    let stderr = String::from_utf8(read_all(host.stderr.take())).unwrap();
    assert!(
        stderr.contains("caller accepted browser=firefox"),
        "{stderr}"
    );
    assert!(stderr.contains("input closed"), "{stderr}");
}

#[test]
fn without_a_player_hello_is_refused_as_unavailable() {
    // Шины по этому адресу нет вовсе: честный отказ, посредник жив до EOF.
    let directory = std::env::temp_dir().join(format!(
        "fastiplayer-browser-host-binary-{}-nobus",
        std::process::id()
    ));
    let address = format!("unix:path={}", directory.join("missing").display());
    let mut host = spawn_host(&[&chromium_origin()], &address);
    let mut stdin = host.stdin.take().unwrap();
    stdin.write_all(&frame(HELLO)).unwrap();
    drop(stdin);
    assert_eq!(wait_for_exit(&mut host), Some(0));
    let output = read_all(host.stdout.take());
    let (prefix, body) = output.split_first_chunk::<4>().expect("one reply");
    assert_eq!(
        usize::try_from(u32::from_ne_bytes(*prefix)).unwrap(),
        body.len()
    );
    assert_eq!(
        decode_message::<PlayerControlMessage>(body).unwrap(),
        PlayerControlMessage::Refused(ControlRefusal {
            in_reply_to: ReplyTarget::Hello {},
            reason: RefusalReason::PlayerUnavailable {},
        })
    );
    let stderr = String::from_utf8(read_all(host.stderr.take())).unwrap();
    let expected_line = format!(
        "answered request=hello bytes={} reply=host:refused:player_unavailable",
        HELLO.len()
    );
    assert!(stderr.contains(&expected_line), "{stderr}");
}

#[test]
fn a_closed_stderr_does_not_change_the_answer() {
    // Браузер может не читать stderr: запись в закрытую трубу не должна ронять
    // посредника (ловушка `eprintln!` из сессии 03e).
    let bus = ListeningBus::new("closed-stderr");
    let mut host = spawn_host(&[&chromium_origin()], &bus.address());
    drop(host.stderr.take());
    let mut stdin = host.stdin.take().unwrap();
    stdin.write_all(&frame("not json")).unwrap();
    drop(stdin);
    assert_eq!(wait_for_exit(&mut host), Some(0));
    let output = read_all(host.stdout.take());
    let body = &output[4..];
    assert!(matches!(
        decode_message::<PlayerControlMessage>(body).unwrap(),
        PlayerControlMessage::Refused(ControlRefusal {
            in_reply_to: ReplyTarget::UnparsedMessage {},
            ..
        })
    ));
    assert!(
        !bus.was_contacted(),
        "a malformed message must not reach the bus"
    );
}

#[test]
fn an_oversized_frame_closes_the_port_with_code_3() {
    let bus = ListeningBus::new("oversized");
    let mut host = spawn_host(&[&chromium_origin()], &bus.address());
    let mut stdin = host.stdin.take().unwrap();
    // 64 МиБ — предел Chrome; наш предел — 128 КиБ. Тело не посылаем: посредник
    // обязан выйти по одному префиксу, не дожидаясь байтов.
    stdin
        .write_all(&(64_u32 * 1024 * 1024).to_ne_bytes())
        .unwrap();
    assert_eq!(wait_for_exit(&mut host), Some(3));
    drop(stdin);
    let stderr = String::from_utf8(read_all(host.stderr.take())).unwrap();
    assert!(
        stderr.contains("broken frame, closing the port"),
        "{stderr}"
    );
    assert!(!bus.was_contacted());
}
