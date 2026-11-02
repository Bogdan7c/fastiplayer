//! Посредник native messaging «звонок в дверь» (мост, сессия 05): НАСТОЯЩИЙ бинарь
//! `fastiplayer-browser-host` против НАСТОЯЩЕГО `fastiplayer --browser-session`.
//!
//! Тест играет роль браузера: запускает посредника как браузер (аргументы вызывающего,
//! stdin/stdout — рамки «4 байта длины в порядке байт машины + JSON»), а частная шина
//! D-Bus будит процесс плеера по `.service`-файлу. Доказано:
//! - браузер → посредник → D-Bus → активация → `hello_accepted` / `session_granted`, и
//!   выданный пропуск открывает сеанс, который играет записанный поток до кадров на
//!   показе (`native_host_wakes_the_player_and_the_granted_session_plays_to_render`);
//! - посредник нужен только для звонка (решение 85): закрытый вход или убитый
//!   посредник сеанс не трогают (`host_killed_mid_session_does_not_stop_playback`);
//! - два посредника (Chromium и Firefox) не путают ответы, а вторая вкладка заменяет
//!   первую (`two_hosts_last_session_replaces_the_first_and_replies_do_not_mix`);
//! - плеер ушёл между звонками: посредник честно отдаёт `hello_required`, не падает и
//!   после нового `hello` снова даёт пропуск (`player_gone_between_calls_…`);
//! - чужой экстеншен не будит плеер вовсе, код выхода 2 (`foreign_extension_…`);
//! - битый JSON получает отказ, а порт остаётся открытым (`malformed_frame_…`).

use std::io::{Read, Write};
use std::process::{ChildStdin, ChildStdout, ExitStatus};
use std::sync::OnceLock;
use std::thread::JoinHandle;

use browser_bridge_protocol::extension_identity::{
    CHROMIUM_DEVELOPMENT_EXTENSION_ID, FIREFOX_EXTENSION_ID,
};
use browser_bridge_protocol::{ControlRefusal, GrantOutcome, MessageErrorKind, RefusalReason};
use browser_bridge_protocol::{ReplyTarget, SessionCloseReason as CloseReason};

use super::*;

/// Сколько ждём выхода посредника после закрытия входа (на деле — миллисекунды).
const HOST_EXIT_WAIT: Duration = Duration::from_secs(10);

/// Сколько тест следит, не проснулся ли плеер от чужого экстеншена.
const NO_WAKE_OBSERVATION: Duration = Duration::from_secs(1);

// ---- бинарь посредника -----------------------------------------------------------

/// Путь к `fastiplayer-browser-host`, собранному в том же профиле, что и этот тест.
///
/// `CARGO_BIN_EXE_<имя>` cargo задаёт только для бинарей СВОЕГО пакета, а посредник —
/// отдельный крейт `browser-bridge-host`; зависимость от бинаря между пакетами
/// (artifact dependencies) ещё нестабильна. Поэтому тест сам просит cargo собрать
/// бинарь (к началу тестов блокировка сборки уже отпущена, вложенный `cargo build`
/// не зависает) и берёт путь из `compiler-artifact` в JSON-выводе — не угадывая
/// каталог `target`. Профиль берётся тот же, что у бинаря плеера: его каталог
/// (`debug`, `release` или имя своего профиля) — родитель `CARGO_BIN_EXE_fastiplayer`.
fn native_host_binary() -> &'static Path {
    static BINARY: OnceLock<PathBuf> = OnceLock::new();
    BINARY.get_or_init(|| {
        let player_binary = Path::new(env!("CARGO_BIN_EXE_fastiplayer"));
        let profile = player_binary
            .parent()
            .and_then(Path::file_name)
            .and_then(|name| name.to_str())
            .expect("the player binary lies in target/<profile>/");
        let mut build = Command::new(env!("CARGO"));
        build
            .current_dir(concat!(env!("CARGO_MANIFEST_DIR"), "/../.."))
            .args([
                "build",
                "-p",
                "browser-bridge-host",
                "--bin",
                "fastiplayer-browser-host",
                "--message-format=json-render-diagnostics",
            ]);
        match profile {
            "debug" => {}
            "release" => {
                build.arg("--release");
            }
            custom => {
                build.args(["--profile", custom]);
            }
        }
        let output = build.output().expect("cargo is runnable");
        assert!(
            output.status.success(),
            "building the native host failed:\n{}",
            String::from_utf8_lossy(&output.stderr)
        );
        String::from_utf8_lossy(&output.stdout)
            .lines()
            .filter_map(|line| serde_json::from_str::<serde_json::Value>(line).ok())
            .filter(|message| message["reason"] == "compiler-artifact")
            .filter(|message| message["target"]["name"] == "fastiplayer-browser-host")
            .find_map(|message| message["executable"].as_str().map(PathBuf::from))
            .expect("cargo reported the native host executable")
    })
}

// ---- посредник под тестом, как его видит браузер ----------------------------------

/// Аргумент Chromium: `chrome-extension://<ID>/`.
fn chromium_argument() -> Vec<String> {
    vec![format!(
        "chrome-extension://{CHROMIUM_DEVELOPMENT_EXTENSION_ID}/"
    )]
}

/// Аргументы Firefox: путь к файлу подключения и ID экстеншена.
fn firefox_arguments() -> Vec<String> {
    vec![
        "/nonexistent/fastiplayer_browser_host.json".to_owned(),
        FIREFOX_EXTENSION_ID.to_owned(),
    ]
}

/// Запущенный посредник: вход, читатель ответов и сборщик stderr.
struct NativeHost {
    child: Child,
    stdin: Option<ChildStdin>,
    /// Целые рамки ответов; канал закрывается, когда stdout посредника закрыт.
    replies: mpsc::Receiver<Vec<u8>>,
    stderr_reader: Option<JoinHandle<String>>,
}

impl NativeHost {
    /// Запускает посредника так, как это делает браузер, на частной шине `bus`.
    fn spawn(bus: &PrivateBusWithBrowserSession, arguments: &[String]) -> Self {
        let mut child = Command::new(native_host_binary())
            .args(arguments)
            .env("DBUS_SESSION_BUS_ADDRESS", &bus.address)
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .spawn()
            .expect("the native host starts");
        let stdin = child.stdin.take().expect("host stdin");
        let stdout = child.stdout.take().expect("host stdout");
        let mut stderr = child.stderr.take().expect("host stderr");
        let (reply_sender, replies) = mpsc::channel();
        std::thread::spawn(move || read_reply_frames(stdout, &reply_sender));
        // stderr вычитывается всегда: иначе заполненная труба остановила бы посредника.
        let stderr_reader = std::thread::spawn(move || {
            let mut text = String::new();
            let _ = stderr.read_to_string(&mut text);
            text
        });
        Self {
            child,
            stdin: Some(stdin),
            replies,
            stderr_reader: Some(stderr_reader),
        }
    }

    /// Пишет рамку: 4 байта длины в порядке байт машины и JSON.
    fn send_frame(&mut self, json: &str) -> std::io::Result<()> {
        let length = u32::try_from(json.len()).expect("test message fits u32");
        let mut frame = length.to_ne_bytes().to_vec();
        frame.extend_from_slice(json.as_bytes());
        let stdin = self.stdin.as_mut().expect("host stdin is open");
        stdin.write_all(&frame)?;
        stdin.flush()
    }

    /// Читает следующую рамку ответа и разбирает её как ответ плеера.
    fn read_reply(&self) -> PlayerControlMessage {
        let frame = self
            .replies
            .recv_timeout(WAIT)
            .expect("the host answered in time");
        decode_message(&frame).expect("the host reply is a player control message")
    }

    /// Одно сообщение браузера → один ответ.
    fn ask(&mut self, json: &str) -> PlayerControlMessage {
        self.send_frame(json).expect("the host takes the frame");
        self.read_reply()
    }

    /// `hello`, на который обязан прийти `hello_accepted`.
    fn say_hello(&mut self) {
        assert!(
            matches!(self.ask(HELLO), PlayerControlMessage::HelloAccepted(_)),
            "hello must be accepted"
        );
    }

    /// `open_session`, на который обязан прийти пропуск.
    fn open_session(&mut self, request_id: u32, session_key: &str) -> SessionGrant {
        match self.ask(&open_session_json(request_id, session_key)) {
            PlayerControlMessage::SessionGranted(grant) => grant,
            other => panic!("open_session must be granted, got {other:?}"),
        }
    }

    /// Посредник ещё жив (процесс не завершён).
    fn assert_running(&mut self) {
        assert!(
            self.child.try_wait().expect("host state").is_none(),
            "the native host must still be running"
        );
    }

    /// Браузер закрывает порт: вход посредника закрыт, ждём выхода.
    fn close_input_and_wait(&mut self) -> ExitStatus {
        self.stdin = None;
        self.wait_for_exit()
    }

    /// Ждёт завершения процесса посредника.
    fn wait_for_exit(&mut self) -> ExitStatus {
        let deadline = Instant::now() + HOST_EXIT_WAIT;
        loop {
            if let Some(status) = self.child.try_wait().expect("host state") {
                return status;
            }
            assert!(
                Instant::now() < deadline,
                "the native host did not exit within {HOST_EXIT_WAIT:?}"
            );
            std::thread::sleep(Duration::from_millis(20));
        }
    }

    /// Журнал stderr посредника целиком; вызывать после выхода процесса.
    fn stderr_after_exit(&mut self) -> String {
        self.stderr_reader
            .take()
            .expect("stderr is read once")
            .join()
            .expect("stderr reader thread")
    }
}

impl Drop for NativeHost {
    fn drop(&mut self) {
        // Тест упал или закончил раньше: посредник не должен остаться сиротой.
        let _ = self.child.kill();
        let _ = self.child.wait();
    }
}

/// Читает рамки stdout посредника и отдаёт их по одной; конец потока закрывает канал.
fn read_reply_frames(mut stdout: ChildStdout, sender: &mpsc::Sender<Vec<u8>>) {
    loop {
        let mut prefix = [0_u8; 4];
        if stdout.read_exact(&mut prefix).is_err() {
            return;
        }
        let length = usize::try_from(u32::from_ne_bytes(prefix)).expect("length fits usize");
        let mut body = vec![0_u8; length];
        if stdout.read_exact(&mut body).is_err() || sender.send(body).is_err() {
            return;
        }
    }
}

/// `open_session` вкладки с этим номером запроса и ключом сеанса.
fn open_session_json(request_id: u32, session_key: &str) -> String {
    format!(
        r#"{{"type":"open_session","request_id":{request_id},"session_key":"{session_key}","page":{{"title":"Episode {request_id}","domain":"example.org"}},"intent":"window","hints":{{"duration":{{"kind":"unknown"}},"position":{{"kind":"unknown"}}}}}}"#
    )
}

/// Подключается к сеансу по пропуску; чтение не висит вечно, если процесс замолчал.
fn attach_with_timeout(grant: &SessionGrant) -> WebSocket<MaybeTlsStream<TcpStream>> {
    let socket = attach(grant);
    if let MaybeTlsStream::Plain(stream) = socket.get_ref() {
        stream
            .set_read_timeout(Some(WAIT))
            .expect("read timeout is set");
    }
    socket
}

fn send_step(socket: &mut WebSocket<MaybeTlsStream<TcpStream>>, step: StreamStep) {
    let message = match step {
        StreamStep::Text(text) => Message::text(text),
        StreamStep::Binary(bytes) => Message::binary(bytes),
        StreamStep::AwaitClock(_) => panic!("this recording has no pauses"),
    };
    socket.send(message).expect("stream step is sent");
}

/// Страница досмотрела поток: часы дошли до `ended`, вкладка ушла, итог процесса —
/// все кадры дошли до показа, позиция не откатывалась, процесс вышел сам.
fn finish_playback_and_check_it_reached_render(
    bus: &PrivateBusWithBrowserSession,
    socket: &mut WebSocket<MaybeTlsStream<TcpStream>>,
) {
    read_player_messages_until(socket, "the clock state ended", |message| {
        matches!(
            message,
            PlayerSessionMessage::Clock {
                state: ClockState::Ended,
                ..
            }
        )
    });
    send_tab_gone_and_expect_close(socket);
    let summary = bus.wait_for_log_line("Browser playback summary kind=final");
    assert_eq!(
        log_number(&summary, "presented_frames"),
        FIXTURE_VIDEO_FRAMES,
        "{summary}"
    );
    assert_eq!(log_number(&summary, "position_regressions"), 0, "{summary}");
    bus.wait_for_log_line("Browser session process finished");
}

// ---- тесты -------------------------------------------------------------------------

#[test]
fn native_host_wakes_the_player_and_the_granted_session_plays_to_render() {
    let bus = PrivateBusWithBrowserSession::start("native-host-plays");
    let mut host = NativeHost::spawn(&bus, &chromium_argument());

    // Первый же звонок будит ещё не запущенный плеер через активацию D-Bus.
    host.say_hello();
    let grant = host.open_session(1, "tab-12-video-3");
    assert_eq!(grant.outcome, GrantOutcome::Started);
    bus.wait_for_log_line("Browser session opened");

    // Браузер закрыл порт: посредник выходит штатно и сообщает об этом в stderr.
    let status = host.close_input_and_wait();
    assert_eq!(status.code(), Some(0), "closed input is a normal end");
    assert!(
        host.stderr_after_exit().contains("input closed"),
        "the host explains why it exited"
    );

    // Посредник ушёл, а пропуск работает: поток играет до кадров на показе.
    let mut socket = attach_with_timeout(&grant);
    let (steps, _bytes) = recorded_stream("two-source-buffers-mid-box-cuts");
    for step in steps {
        send_step(&mut socket, step);
    }
    finish_playback_and_check_it_reached_render(&bus, &mut socket);
}

#[test]
fn host_killed_mid_session_does_not_stop_playback() {
    let bus = PrivateBusWithBrowserSession::start("native-host-killed");
    let mut host = NativeHost::spawn(&bus, &chromium_argument());
    host.say_hello();
    let grant = host.open_session(1, "tab-12-video-3");
    bus.wait_for_log_line("Browser session opened");
    let mut socket = attach_with_timeout(&grant);

    // Половина потока уходит при живом посреднике, затем его убивают (SIGKILL).
    let (mut steps, _bytes) = recorded_stream("two-source-buffers-mid-box-cuts");
    let second_half = steps.split_off(steps.len() / 2);
    assert!(
        !steps.is_empty() && !second_half.is_empty(),
        "the recording must be splittable"
    );
    for step in steps {
        send_step(&mut socket, step);
    }
    host.child.kill().expect("the host is killed");
    let status = host.wait_for_exit();
    assert_eq!(status.code(), None, "the host died from a signal");

    // Вторая половина доходит без посредника: сеанс живёт на прямом WebSocket.
    for step in second_half {
        send_step(&mut socket, step);
    }
    finish_playback_and_check_it_reached_render(&bus, &mut socket);
}

#[test]
fn two_hosts_last_session_replaces_the_first_and_replies_do_not_mix() {
    let bus = PrivateBusWithBrowserSession::start("native-host-two");
    let mut chromium_host = NativeHost::spawn(&bus, &chromium_argument());
    let mut firefox_host = NativeHost::spawn(&bus, &firefox_arguments());
    chromium_host.say_hello();
    firefox_host.say_hello();

    let first_grant = chromium_host.open_session(1, "tab-1-video-1");
    assert_eq!(first_grant.outcome, GrantOutcome::Started);
    bus.wait_for_log_line("Browser session opened");
    let mut first_tab = attach_with_timeout(&first_grant);

    // Вторая вкладка: ответ приходит ЕЙ (свой номер запроса и ключ), а не первой.
    let second_grant = firefox_host.open_session(2, "tab-2-video-1");
    assert_eq!(second_grant.outcome, GrantOutcome::ReplacedPrevious);
    assert_eq!(second_grant.session_key.as_str(), "tab-2-video-1");
    assert_eq!(first_grant.session_key.as_str(), "tab-1-video-1");
    assert_eq!(
        serde_json::to_value(first_grant.request_id).expect("request id"),
        serde_json::json!(1)
    );
    assert_eq!(
        serde_json::to_value(second_grant.request_id).expect("request id"),
        serde_json::json!(2)
    );
    assert_ne!(first_grant.token.as_str(), second_grant.token.as_str());

    // Первая вкладка закрыта кодом замены, вторая подключается.
    assert_eq!(
        read_close_code(&mut first_tab),
        CloseCode::from(CloseReason::ReplacedByAnotherTab.close_code())
    );
    bus.wait_for_log_line("Browser session opened");
    let mut second_tab = attach_with_timeout(&second_grant);

    for host in [&mut chromium_host, &mut firefox_host] {
        assert_eq!(host.close_input_and_wait().code(), Some(0));
    }
    send_tab_gone_and_expect_close(&mut second_tab);
    bus.wait_for_log_line("Browser session process finished");
}

#[test]
fn player_gone_between_calls_host_answers_honestly_and_recovers() {
    let bus = PrivateBusWithBrowserSession::start("native-host-player-gone");
    let mut host = NativeHost::spawn(&bus, &chromium_argument());
    host.say_hello();
    let grant = host.open_session(1, "tab-12-video-3");
    bus.wait_for_log_line("Browser session opened");
    let mut socket = attach_with_timeout(&grant);

    // Вкладка ушла, процесс плеера вышел; посредник того же порта остаётся.
    send_tab_gone_and_expect_close(&mut socket);
    bus.wait_for_log_line("Browser session process finished");
    host.assert_running();

    // Шина будит НОВЫЙ процесс, а он этого звонящего не знает: честное «нужен hello».
    let PlayerControlMessage::Refused(ControlRefusal {
        in_reply_to,
        reason,
    }) = host.ask(&open_session_json(2, "tab-12-video-4"))
    else {
        panic!("a fresh player process does not know this caller");
    };
    assert!(matches!(in_reply_to, ReplyTarget::OpenSession { .. }));
    assert_eq!(reason, RefusalReason::HelloRequired {});
    host.assert_running();

    // Экстеншен повторяет hello (так и задумано) — сеанс снова выдаётся.
    host.say_hello();
    let regrant = host.open_session(3, "tab-12-video-4");
    assert_eq!(regrant.outcome, GrantOutcome::Started);
    assert_eq!(regrant.session_key.as_str(), "tab-12-video-4");
    bus.wait_for_log_line("Browser session opened");
    let mut second_socket = attach_with_timeout(&regrant);
    send_tab_gone_and_expect_close(&mut second_socket);

    assert_eq!(host.close_input_and_wait().code(), Some(0));
}

#[test]
fn foreign_extension_is_refused_without_waking_the_player() {
    let bus = PrivateBusWithBrowserSession::start("native-host-foreign");
    let foreign = vec!["chrome-extension://aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa/".to_owned()];
    let mut host = NativeHost::spawn(&bus, &foreign);

    // Посредник выходит, не читая вход; запись в закрытую трубу может не удаться.
    let _ = host.send_frame(HELLO);
    let status = host.wait_for_exit();
    assert_eq!(status.code(), Some(2), "foreign caller exits with code 2");
    assert!(
        host.stderr_after_exit().contains("caller rejected"),
        "the host explains the refusal"
    );
    assert!(
        host.replies
            .recv_timeout(Duration::from_millis(200))
            .is_err(),
        "a foreign caller gets no reply frame"
    );
    // До шины дело не дошло: процесс плеера не запускался.
    bus.expect_no_log_line_for(NO_WAKE_OBSERVATION, &["Browser session"]);
}

#[test]
fn malformed_frame_gets_a_refusal_and_the_port_stays_open() {
    let bus = PrivateBusWithBrowserSession::start("native-host-malformed");
    let mut host = NativeHost::spawn(&bus, &chromium_argument());

    let PlayerControlMessage::Refused(ControlRefusal {
        in_reply_to,
        reason,
    }) = host.ask(r#"{"type":"nope"}"#)
    else {
        panic!("a malformed message must be refused");
    };
    assert_eq!(in_reply_to, ReplyTarget::UnparsedMessage {});
    assert_eq!(
        reason,
        RefusalReason::MalformedMessage {
            error: MessageErrorKind::UnknownType
        }
    );

    // Порт открыт, посредник жив и дальше работает: hello доходит до плеера.
    host.assert_running();
    host.say_hello();
    assert_eq!(host.close_input_and_wait().code(), Some(0));
}
