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
use crate::browser_session::intake_notice::IntakeNotice;
use crate::browser_session::player_hello::player_hello;
use crate::browser_session::session_media::DrainEnd;

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

/// Медиа-часть-подделка: запоминает, что ей сообщил цикл, и просит проснуться к
/// заданному сроку.
#[derive(Default)]
struct RecordingMedia {
    opened: Vec<SessionRef>,
    ended: Vec<SessionRef>,
    notices: Vec<IntakeNotice>,
    bytes_appended: Vec<SessionRef>,
    drives: usize,
    wake_at: Option<Instant>,
    /// Сколько раз цикл спросил «доиграно?».
    drain_questions: usize,
    /// Что отвечать на «доиграно?» (`None` — «ещё играем»).
    drain_answer: Option<DrainProgress>,
}

impl SessionMediaPort for RecordingMedia {
    fn on_session_opened(&mut self, session: &SessionRef) {
        self.opened.push(session.clone());
    }

    fn on_session_ended(&mut self, session: &SessionRef) {
        self.ended.push(session.clone());
    }

    fn on_intake(&mut self, notices: Vec<IntakeNotice>, bytes_appended: Option<SessionRef>) {
        self.notices.extend(notices);
        self.bytes_appended.extend(bytes_appended);
    }

    fn drive(&mut self, _now: Instant) -> Option<Instant> {
        self.drives += 1;
        self.wake_at
    }

    fn playback_drain(&mut self, now: Instant) -> DrainProgress {
        self.drain_questions += 1;
        self.drain_answer.unwrap_or(DrainProgress::Draining {
            wake_at: now + Duration::from_millis(250),
        })
    }
}

/// Цикл с настоящими сервером и стойкой; сервер живёт до конца теста.
struct Harness {
    runtime: BrowserSessionRuntime,
    media: RecordingMedia,
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
            media: RecordingMedia::default(),
            inbox,
            wake,
            server,
            pause_requests,
        }
    }

    /// Звонок посредника: кладёт сообщение в ящик, проходит шаг цикла, возвращает ответ.
    fn call(&mut self, message: &str) -> PlayerControlMessage {
        let reply_receiver = self.deliver(message);
        let verdict = self.runtime.step(Instant::now(), &mut self.media);
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

    /// Крутит цикл, пока `done` не станет истиной (или до потолка ожидания); возвращает
    /// последнее решение машины.
    fn step_until(&mut self, mut done: impl FnMut(&Self) -> bool) -> LifecycleVerdict {
        let deadline = Instant::now() + WAIT;
        loop {
            let verdict = self.runtime.step(Instant::now(), &mut self.media);
            if done(self) || matches!(verdict, LifecycleVerdict::Exit(_)) {
                return verdict;
            }
            assert!(Instant::now() < deadline, "the awaited state never came");
            self.wake
                .wait(Some(Instant::now() + Duration::from_millis(20)));
        }
    }

    /// Крутит цикл, пока машина не скажет «выходим» (или до потолка ожидания).
    fn run_until_exit(&mut self) -> ExitCause {
        let deadline = Instant::now() + WAIT;
        loop {
            match self.runtime.step(Instant::now(), &mut self.media) {
                LifecycleVerdict::Exit(cause) => return cause,
                LifecycleVerdict::AwaitPlaybackDrain => {
                    panic!("`step` must not hand the draining state to its caller")
                }
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
fn process_exits_after_tab_gone_once_the_media_has_nothing_left_to_play() {
    let mut harness = Harness::start(BrowserSessionPolicy::default());
    // Медиа-часть отвечает «доиграно» сразу (ничего не загружено).
    harness.media.drain_answer = Some(DrainProgress::Drained(DrainEnd::NothingToPlay));
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

/// `tab_gone` не выключает процесс: пока медиа-часть отвечает «ещё играем», цикл жив
/// и спрашивает её снова; как только «доиграно» — выход по причине `tab_gone`.
#[test]
fn tab_gone_keeps_the_process_alive_while_playing_and_exits_after_the_media_drained() {
    let mut harness = Harness::start(BrowserSessionPolicy::default());
    harness.call(HELLO);
    let grant = harness.open(1, "tab-1-video-1");
    let mut socket = attach(&grant);
    socket
        .send(Message::text(TAB_GONE.trim_end()))
        .expect("tab_gone is sent");

    // Сеанс закончился, но плеер ещё играет: цикл спрашивает «доиграно?» и живёт.
    let verdict = harness.step_until(|harness| harness.media.drain_questions >= 3);
    assert!(
        matches!(verdict, LifecycleVerdict::KeepRunning { wake_at: Some(_) }),
        "the process must keep running while the media is draining: {verdict:?}"
    );
    assert_eq!(harness.media.ended.len(), 1, "the media learned of the end");

    // Плеер доиграл — следующий же проход выходит, причина прежняя.
    harness.media.drain_answer = Some(DrainProgress::Drained(DrainEnd::PlayedToEnd));
    assert_eq!(
        harness.run_until_exit(),
        ExitCause::SessionEnded(SessionEndReason::ClosedByPlayer(
            SessionCloseReason::TabGone
        ))
    );
}

/// Пока доигрываем, срок пробуждения цикла — срок медиа-части (часов странице нет).
#[test]
fn draining_wakes_the_loop_at_the_media_deadline() {
    let mut harness = Harness::start(BrowserSessionPolicy::default());
    harness.call(HELLO);
    let grant = harness.open(1, "tab-1-video-1");
    let mut socket = attach(&grant);
    socket
        .send(Message::text(TAB_GONE.trim_end()))
        .expect("tab_gone is sent");
    harness.step_until(|harness| harness.media.drain_questions >= 1);
    let asked_at = Instant::now();
    harness.media.drain_answer = Some(DrainProgress::Draining {
        wake_at: asked_at + Duration::from_secs(3),
    });

    let verdict = harness.runtime.step(asked_at, &mut harness.media);

    assert_eq!(
        verdict,
        LifecycleVerdict::KeepRunning {
            wake_at: Some(asked_at + Duration::from_secs(3))
        }
    );
}

/// Новый сеанс во время доигрывания заменяет доигрываемый: цикл перестаёт спрашивать
/// «доиграно?» и живёт дальше как с обычным активным сеансом.
#[test]
fn a_new_session_during_the_drain_forgets_the_drain() {
    let mut harness = Harness::start(BrowserSessionPolicy::default());
    harness.call(HELLO);
    let grant = harness.open(1, "tab-1-video-1");
    let mut socket = attach(&grant);
    socket
        .send(Message::text(TAB_GONE.trim_end()))
        .expect("tab_gone is sent");
    harness.step_until(|harness| harness.media.drain_questions >= 1);

    harness.open(2, "tab-2-video-1");
    let questions_after_open = harness.media.drain_questions;
    // Даже если бы медиа-часть сказала «доиграно», новый сеанс уже не доигрывается.
    harness.media.drain_answer = Some(DrainProgress::Drained(DrainEnd::PlayedToEnd));
    for _ in 0..3 {
        let verdict = harness.runtime.step(Instant::now(), &mut harness.media);
        assert!(
            matches!(verdict, LifecycleVerdict::KeepRunning { .. }),
            "the new session keeps the process alive: {verdict:?}"
        );
    }
    assert_eq!(harness.media.drain_questions, questions_after_open);
    assert_eq!(harness.media.opened.len(), 2);
}

/// Экстеншен пропал и не вернулся за срок возврата — тоже доигрываем.
#[test]
fn a_vanished_extension_drains_the_playback_before_the_process_exits() {
    let policy = BrowserSessionPolicy {
        reconnect_grace: Duration::from_millis(100),
        ..BrowserSessionPolicy::default()
    };
    let mut harness = Harness::start(policy);
    harness.call(HELLO);
    let grant = harness.open(1, "tab-1-video-1");
    let socket = attach(&grant);
    // Обрыв без кадра закрытия и без `tab_gone`.
    drop(socket);

    let verdict = harness.step_until(|harness| harness.media.drain_questions >= 2);
    assert!(matches!(verdict, LifecycleVerdict::KeepRunning { .. }));

    harness.media.drain_answer = Some(DrainProgress::Drained(DrainEnd::PlayedToEnd));
    assert_eq!(
        harness.run_until_exit(),
        ExitCause::SessionEnded(SessionEndReason::ReconnectGraceExpired)
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

/// Сообщение страницы перед `tab_gone` не мешает циклу закончить сеанс по
/// `tab_gone`: ящик принимает его как обычное сообщение и не путает с событием жизни.
#[test]
fn page_message_before_tab_gone_still_ends_the_session_by_tab_gone() {
    let mut harness = Harness::start(BrowserSessionPolicy::default());
    harness.media.drain_answer = Some(DrainProgress::Drained(DrainEnd::NothingToPlay));
    harness.call(HELLO);
    let grant = harness.open(1, "tab-1-video-1");
    let mut socket = attach(&grant);

    socket
        .send(Message::text(PAGE_PLAYBACK_PAUSE.trim_end()))
        .expect("page message is sent");
    socket
        .send(Message::text(TAB_GONE.trim_end()))
        .expect("tab_gone is sent");

    assert_eq!(
        harness.run_until_exit(),
        ExitCause::SessionEnded(SessionEndReason::ClosedByPlayer(
            SessionCloseReason::TabGone
        ))
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

    let verdict = harness.runtime.step(Instant::now(), &mut harness.media);

    assert!(
        matches!(verdict, LifecycleVerdict::KeepRunning { .. }),
        "{verdict:?}"
    );
    assert!(matches!(
        decode_reply(&reply),
        PlayerControlMessage::SessionGranted(_)
    ));
    assert_eq!(harness.pause_requests(), 1);
    // Медиа-часть узнаёт о конце экземпляра (забыть отложенное его странице) и
    // сама сверяет, тот ли это экземпляр.
    let ended: Vec<u64> = harness
        .media
        .ended
        .iter()
        .map(|session| session.instance.0)
        .collect();
    assert_eq!(ended, vec![u64::MAX]);
}

#[test]
fn opened_sessions_and_stream_notices_reach_the_media_part_in_order() {
    let mut harness = Harness::start(BrowserSessionPolicy::default());
    harness.call(HELLO);
    let first = harness.open(1, "tab-1-video-1");
    let second = harness.open(2, "tab-2-video-1");
    // Повтор того же сеанса — то же видео, но медиа-часть тоже узнаёт о нём
    // (она сама решает, что экземпляр тот же).
    harness.open(3, "tab-2-video-1");
    assert_eq!(
        harness
            .media
            .opened
            .iter()
            .map(|session| session.key.as_str().to_owned())
            .collect::<Vec<_>>(),
        vec![
            first.session_key.as_str(),
            second.session_key.as_str(),
            second.session_key.as_str()
        ]
    );
    assert_ne!(harness.media.opened[0], harness.media.opened[1]);
    assert_eq!(harness.media.opened[1], harness.media.opened[2]);

    let session = harness.media.opened[1].clone();
    harness.inbox.post_intake_notices([
        IntakeNotice::BytesAppended {
            session: session.clone(),
        },
        IntakeNotice::CloseSession {
            session: session.clone(),
            reason: SessionCloseReason::ProtocolViolation,
        },
    ]);
    let drives_before = harness.media.drives;
    harness.runtime.step(Instant::now(), &mut harness.media);
    assert_eq!(harness.media.drives, drives_before + 1);
    assert_eq!(harness.media.bytes_appended, vec![session.clone()]);
    assert!(matches!(
        harness.media.notices.as_slice(),
        [IntakeNotice::CloseSession { session: closed, reason: SessionCloseReason::ProtocolViolation }]
            if *closed == session
    ));
}

#[test]
fn the_loop_wakes_for_the_media_part_when_its_deadline_is_the_earliest() {
    let mut harness = Harness::start(BrowserSessionPolicy::default());
    let media_deadline = Instant::now() + Duration::from_millis(250);
    harness.media.wake_at = Some(media_deadline);
    let verdict = harness.runtime.step(Instant::now(), &mut harness.media);
    // Срок «никто не открыл сеанс» — секунды; медиа просит раньше и побеждает.
    assert_eq!(
        verdict,
        LifecycleVerdict::KeepRunning {
            wake_at: Some(media_deadline)
        }
    );
}
