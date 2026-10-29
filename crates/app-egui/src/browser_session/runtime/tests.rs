//! Главный цикл с настоящими стойкой входа и сервером WebSocket на loopback.
//! Посредника изображает тест: кладёт звонки в ящик, как служба D-Bus.

use std::net::TcpStream;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::mpsc;
use std::time::Duration;

use browser_bridge_protocol::limits::MAX_JSON_MESSAGE_BYTES;
use browser_bridge_protocol::{
    PlayerControlMessage, PlayerSessionMessage, RefusalReason, SessionCloseReason, SessionGrant,
    decode_message,
};
use browser_bridge_session::{
    BrowserSessionServer, CountingByteSink, OsRandomTokenSource, SessionEndReason, SessionEventSink,
};
use desktop_integration::{
    BrowserEntryCaller, BrowserEntryReply, BrowserEntryRequest, BrowserEntrySink,
    MAX_BROWSER_ENTRY_MESSAGE_BYTES,
};
use tungstenite::protocol::frame::coding::CloseCode;
use tungstenite::stream::MaybeTlsStream;
use tungstenite::{Message, WebSocket};

use super::*;
use crate::browser_session::inbox::{InboxCapacity, ParkingWake};
use crate::browser_session::player_hello::player_hello;

/// Потолок ожидания событий из потоков сервера.
const WAIT: Duration = Duration::from_secs(20);

/// Настоящие фикстуры протокола: формы сообщений не угадываются в тестах.
const TAB_GONE: &str = include_str!(concat!(
    env!("CARGO_MANIFEST_DIR"),
    "/../browser-bridge-protocol/fixtures/session/valid/page-tab-gone.json"
));
const PAGE_PLAYBACK_PAUSE: &str = include_str!(concat!(
    env!("CARGO_MANIFEST_DIR"),
    "/../browser-bridge-protocol/fixtures/session/valid/page-page-playback-pause.json"
));

const HELLO: &str = r#"{"type":"hello","protocol":{"major":1,"minor":0},"software_version":"0.1.0","capabilities":[]}"#;

/// Подделка обычного плеера: считает просьбы «пауза» и не ходит на шину.
#[derive(Clone, Default)]
struct CountingPause {
    requests: Arc<AtomicUsize>,
}

impl NormalPlayerPause for CountingPause {
    fn request_pause(&mut self) {
        self.requests.fetch_add(1, Ordering::SeqCst);
    }

    fn finish(&mut self) {}
}

/// Цикл с настоящими сервером и стойкой; сервер живёт до конца теста.
struct Harness {
    runtime: BrowserSessionRuntime,
    inbox: Arc<BrowserSessionInbox>,
    wake: Arc<ParkingWake>,
    server: BrowserSessionServer,
    pause_requests: Arc<AtomicUsize>,
}

impl Harness {
    fn start(policy: BrowserSessionPolicy) -> Self {
        let wake = Arc::new(ParkingWake::default());
        let inbox = Arc::new(BrowserSessionInbox::new(
            InboxCapacity::default(),
            wake.clone(),
        ));
        let server = BrowserSessionServer::start(
            policy,
            Box::new(OsRandomTokenSource),
            Arc::new(CountingByteSink::new()),
            inbox.clone(),
        )
        .expect("loopback server starts");
        let desk = EntryDesk::new(
            player_hello().expect("valid player hello"),
            server.control(),
            &policy,
        );
        let pause = CountingPause::default();
        let pause_requests = pause.requests.clone();
        let runtime = BrowserSessionRuntime::new(desk, &policy, inbox.clone(), Box::new(pause));
        Self {
            runtime,
            inbox,
            wake,
            server,
            pause_requests,
        }
    }

    /// Звонок посредника: кладёт сообщение в ящик, проходит шаг цикла, возвращает ответ.
    fn call(&mut self, message: &str) -> PlayerControlMessage {
        let reply_receiver = self.deliver(message);
        let verdict = self.runtime.step(Instant::now());
        assert!(matches!(verdict, LifecycleVerdict::KeepRunning { .. }));
        decode_reply(&reply_receiver)
    }

    /// Кладёт звонок в ящик без шага цикла (чтобы собрать батч из нескольких событий).
    fn deliver(&self, message: &str) -> mpsc::Receiver<String> {
        let (reply_sender, reply_receiver) = mpsc::channel();
        let reply = BrowserEntryReply::from_callback(move |reply_text| {
            let _ = reply_sender.send(reply_text);
        });
        let request = BrowserEntryRequest {
            caller: BrowserEntryCaller::from_sender_name(":1.7".to_owned()),
            message: message.to_owned(),
        };
        self.inbox
            .deliver(BrowserEntryDelivery::new(request, reply))
            .expect("inbox accepts the call");
        reply_receiver
    }

    fn open(&mut self, request_id: u32, session_key: &str) -> SessionGrant {
        match self.call(&open_session(request_id, session_key)) {
            PlayerControlMessage::SessionGranted(grant) => grant,
            other => panic!("expected session_granted, got {other:?}"),
        }
    }

    fn pause_requests(&self) -> usize {
        self.pause_requests.load(Ordering::SeqCst)
    }

    /// Крутит цикл, пока машина не скажет «выходим» (или до потолка ожидания).
    fn run_until_exit(&mut self) -> ExitCause {
        let deadline = Instant::now() + WAIT;
        loop {
            match self.runtime.step(Instant::now()) {
                LifecycleVerdict::Exit(cause) => return cause,
                LifecycleVerdict::KeepRunning { wake_at } => {
                    assert!(
                        Instant::now() < deadline,
                        "the runtime never decided to exit"
                    );
                    let wait_until = wake_at.map_or(deadline, |wake_at| wake_at.min(deadline));
                    self.wake.wait(Some(wait_until));
                }
            }
        }
    }
}

fn decode_reply(reply_receiver: &mpsc::Receiver<String>) -> PlayerControlMessage {
    let reply_text = reply_receiver
        .recv_timeout(WAIT)
        .expect("every entry call gets exactly one reply");
    decode_message(reply_text.as_bytes()).expect("reply is a valid player control message")
}

fn open_session(request_id: u32, session_key: &str) -> String {
    format!(
        r#"{{"type":"open_session","request_id":{request_id},"session_key":"{session_key}","page":{{"title":"Episode 3","domain":"example.org"}},"intent":"window","hints":{{"duration":{{"kind":"unknown"}},"position":{{"kind":"unknown"}}}}}}"#
    )
}

fn attach(grant: &SessionGrant) -> WebSocket<MaybeTlsStream<TcpStream>> {
    let (mut socket, _response) =
        tungstenite::connect(format!("ws://127.0.0.1:{}/", grant.port.get()))
            .expect("loopback WebSocket connects");
    let attach = format!(
        r#"{{"type":"attach","session_key":"{}","token":"{}"}}"#,
        grant.session_key.as_str(),
        grant.token.as_str()
    );
    socket.send(Message::text(attach)).expect("attach is sent");
    let attached = socket.read().expect("server answers attach");
    let attached_text = attached.into_text().expect("attached is a text frame");
    assert_eq!(
        decode_message::<PlayerSessionMessage>(attached_text.as_bytes()),
        Ok(PlayerSessionMessage::Attached {})
    );
    socket
}

#[test]
fn hello_then_open_session_grants_this_servers_port_and_pauses_the_normal_player_once() {
    let mut harness = Harness::start(BrowserSessionPolicy::default());

    let accepted = harness.call(HELLO);
    assert!(
        matches!(accepted, PlayerControlMessage::HelloAccepted(ref hello) if hello.agreed_protocol == browser_bridge_protocol::CURRENT_PROTOCOL_VERSION),
        "{accepted:?}"
    );
    assert_eq!(
        harness.pause_requests(),
        0,
        "hello alone does not touch the normal player"
    );

    let grant = harness.open(1, "tab-1-video-1");
    assert_eq!(grant.port, harness.server.port());
    assert_eq!(grant.outcome, GrantOutcome::Started);
    assert_eq!(grant.session_key.as_str(), "tab-1-video-1");
    assert_eq!(harness.pause_requests(), 1);
}

#[test]
fn repeated_open_session_reuses_the_session_and_does_not_pause_again() {
    let mut harness = Harness::start(BrowserSessionPolicy::default());
    harness.call(HELLO);
    let first = harness.open(1, "tab-1-video-1");

    let repeated = harness.open(2, "tab-1-video-1");

    assert_eq!(repeated.outcome, GrantOutcome::ReusedExisting);
    assert!(
        repeated.token.matches(&first.token),
        "a retry before attach keeps the token"
    );
    assert_eq!(harness.pause_requests(), 1);
}

#[test]
fn video_from_another_tab_replaces_the_session_and_pauses_the_normal_player_again() {
    let mut harness = Harness::start(BrowserSessionPolicy::default());
    harness.call(HELLO);
    let first = harness.open(1, "tab-1-video-1");

    let replacement = harness.open(2, "tab-2-video-1");

    assert_eq!(replacement.outcome, GrantOutcome::ReplacedPrevious);
    assert!(!replacement.token.matches(&first.token));
    assert_eq!(harness.pause_requests(), 2);
}

#[test]
fn open_session_without_hello_is_refused_and_leaves_the_normal_player_alone() {
    let mut harness = Harness::start(BrowserSessionPolicy::default());

    let refused = harness.call(&open_session(5, "tab-1-video-1"));

    let PlayerControlMessage::Refused(refusal) = refused else {
        panic!("expected refused, got {refused:?}");
    };
    assert_eq!(refusal.reason, RefusalReason::HelloRequired {});
    assert_eq!(harness.pause_requests(), 0);
}

#[test]
fn process_exits_right_after_the_attached_session_ends() {
    let mut harness = Harness::start(BrowserSessionPolicy::default());
    harness.call(HELLO);
    let grant = harness.open(1, "tab-1-video-1");
    let mut socket = attach(&grant);

    socket
        .send(Message::text(TAB_GONE.trim_end()))
        .expect("tab_gone is sent");

    assert_eq!(
        harness.run_until_exit(),
        ExitCause::SessionEnded(SessionEndReason::ClosedByPlayer(
            SessionCloseReason::TabGone
        ))
    );
    // Страница видит честный код закрытия, а не обрыв.
    let close_code = loop {
        match socket.read() {
            Ok(Message::Close(Some(frame))) => break frame.code,
            Ok(_) => continue,
            Err(error) => panic!("expected a close frame, got {error}"),
        }
    };
    assert_eq!(
        close_code,
        CloseCode::from(SessionCloseReason::TabGone.close_code())
    );
}

#[test]
fn process_exits_when_nobody_opens_a_session_in_time() {
    let policy = BrowserSessionPolicy {
        idle_exit_after_start: Duration::from_millis(50),
        ..BrowserSessionPolicy::default()
    };
    let mut harness = Harness::start(policy);
    harness.call(HELLO);

    assert_eq!(harness.run_until_exit(), ExitCause::IdleWithoutSession);
    assert_eq!(harness.pause_requests(), 0);
}

#[test]
fn page_messages_are_counted_until_the_byte_source_arrives() {
    let mut harness = Harness::start(BrowserSessionPolicy::default());
    harness.call(HELLO);
    let grant = harness.open(1, "tab-1-video-1");
    let mut socket = attach(&grant);

    socket
        .send(Message::text(PAGE_PLAYBACK_PAUSE.trim_end()))
        .expect("page message is sent");
    socket
        .send(Message::text(TAB_GONE.trim_end()))
        .expect("tab_gone is sent");
    harness.run_until_exit();

    let finish = harness.runtime.finish(ExitCause::IdleWithoutSession);
    assert_eq!(
        finish.page_messages_received, 2,
        "page_playback and tab_gone"
    );
}

#[test]
fn dbus_entry_limit_matches_the_protocol_json_limit() {
    // desktop-integration не зависит от протокола и держит свою копию предела.
    assert_eq!(MAX_BROWSER_ENTRY_MESSAGE_BYTES, MAX_JSON_MESSAGE_BYTES);
}

#[test]
fn end_of_an_older_session_instance_with_the_same_key_does_not_stop_the_process() {
    // Гонка из ревью: срок возврата прежнего экземпляра K истёк (`Ended(K, старый)`),
    // а посредник тут же открыл K заново. Оба события приходят в одном батче;
    // процесс должен продолжать новый сеанс, а не выйти, выдав мёртвый пропуск.
    let mut harness = Harness::start(BrowserSessionPolicy::default());
    harness.call(HELLO);
    let reply = harness.deliver(&open_session(1, "tab-1-video-1"));
    harness
        .inbox
        .publish_lifecycle(SessionLifecycleEvent::Ended {
            session: SessionRef {
                key: browser_bridge_protocol::SessionKey::new("tab-1-video-1").expect("valid key"),
                instance: browser_bridge_session::SessionInstance(u64::MAX),
            },
            reason: SessionEndReason::ReconnectGraceExpired,
        });

    let verdict = harness.runtime.step(Instant::now());

    assert!(
        matches!(verdict, LifecycleVerdict::KeepRunning { .. }),
        "{verdict:?}"
    );
    assert!(matches!(
        decode_reply(&reply),
        PlayerControlMessage::SessionGranted(_)
    ));
    assert_eq!(harness.pause_requests(), 1);
}
