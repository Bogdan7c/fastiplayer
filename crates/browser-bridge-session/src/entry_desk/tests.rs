use std::sync::Arc;

use browser_bridge_protocol::{BridgeSoftware, ControlRequestId, SessionToken};

use super::*;
use crate::server::BrowserSessionServer;
use crate::sink::CountingByteSink;
use crate::test_support::{RecordingEvents, SequenceTokens};
use crate::token::SessionTokenSource;

const HELLO_EXTENSION: &str =
    include_str!("../../../browser-bridge-protocol/fixtures/control/valid/hello-extension.json");
const HELLO_ACCEPTED: &str =
    include_str!("../../../browser-bridge-protocol/fixtures/control/valid/hello-accepted.json");
const HELLO_MAJOR_2: &str = include_str!(
    "../../../browser-bridge-protocol/fixtures/control/valid/hello-extension-major-2.json"
);
const OPEN_WINDOW: &str = include_str!(
    "../../../browser-bridge-protocol/fixtures/control/valid/open-session-window.json"
);
const REFUSED_MALFORMED: &str =
    include_str!("../../../browser-bridge-protocol/fixtures/control/valid/refused-malformed.json");
const UNKNOWN_FIELD: &str =
    include_str!("../../../browser-bridge-protocol/fixtures/control/invalid/unknown-field.json");

fn caller(name: &str) -> EntryCaller {
    EntryCaller::new(name).unwrap()
}

/// Hello плеера из фикстуры hello-accepted (версия 1.2, две возможности).
fn fixture_player_hello() -> Hello {
    match decode_message::<PlayerControlMessage>(HELLO_ACCEPTED.as_bytes()).unwrap() {
        PlayerControlMessage::HelloAccepted(accepted) => accepted.player,
        other => panic!("ожидали hello_accepted, пришло {other:?}"),
    }
}

fn start_server(tokens: Box<dyn SessionTokenSource>) -> BrowserSessionServer {
    BrowserSessionServer::start(
        BrowserSessionPolicy::default(),
        tokens,
        Arc::new(CountingByteSink::new()),
        Arc::new(RecordingEvents::default()),
    )
    .unwrap()
}

fn desk_for(server: &BrowserSessionServer, policy: &BrowserSessionPolicy) -> EntryDesk {
    EntryDesk::new(fixture_player_hello(), server.control(), policy)
}

fn decode_reply(outcome: &EntryOutcome) -> PlayerControlMessage {
    decode_message::<PlayerControlMessage>(outcome.reply_json.as_bytes()).unwrap()
}

fn refusal_of(outcome: &EntryOutcome) -> ControlRefusal {
    match decode_reply(outcome) {
        PlayerControlMessage::Refused(refusal) => refusal,
        other => panic!("ожидали refused, пришло {other:?}"),
    }
}

#[test]
fn hello_gets_the_exact_accepted_reply_with_the_players_own_hello() {
    let server = start_server(Box::new(SequenceTokens::new()));
    let mut desk = desk_for(&server, &BrowserSessionPolicy::default());
    let outcome = desk.handle(&caller(":1.7"), HELLO_EXTENSION.as_bytes(), Instant::now());
    assert_eq!(outcome.reply_json, HELLO_ACCEPTED);
    assert_eq!(outcome.event, None);
}

#[test]
fn open_session_without_hello_is_refused_with_its_request_id() {
    let server = start_server(Box::new(SequenceTokens::new()));
    let mut desk = desk_for(&server, &BrowserSessionPolicy::default());
    let outcome = desk.handle(&caller(":1.7"), OPEN_WINDOW.as_bytes(), Instant::now());
    assert_eq!(
        refusal_of(&outcome),
        ControlRefusal {
            in_reply_to: ReplyTarget::OpenSession {
                request_id: ControlRequestId(1)
            },
            reason: RefusalReason::HelloRequired {},
        }
    );
    assert_eq!(outcome.event, None);
}

#[test]
fn open_session_after_hello_grants_port_and_token_and_reports_the_event() {
    let server = start_server(Box::new(SequenceTokens::new()));
    let mut desk = desk_for(&server, &BrowserSessionPolicy::default());
    let who = caller(":1.7");
    desk.handle(&who, HELLO_EXTENSION.as_bytes(), Instant::now());
    let outcome = desk.handle(&who, OPEN_WINDOW.as_bytes(), Instant::now());

    let PlayerControlMessage::SessionGranted(grant) = decode_reply(&outcome) else {
        panic!("ожидали session_granted: {}", outcome.reply_json);
    };
    assert_eq!(grant.request_id, ControlRequestId(1));
    assert_eq!(grant.session_key.as_str(), "tab-12-video-3");
    assert_eq!(grant.port, server.port());
    assert_eq!(grant.outcome, GrantOutcome::Started);
    assert!(
        grant
            .token
            .matches(&SessionToken::new(format!("{:032}", 1)).unwrap())
    );

    let Some(EntryEvent::SessionOpened {
        key,
        page,
        intent,
        outcome: event_outcome,
        ..
    }) = outcome.event
    else {
        panic!("ожидали событие SessionOpened");
    };
    assert_eq!(key.as_str(), "tab-12-video-3");
    assert_eq!(page.domain.as_str(), "uaserials.com");
    assert_eq!(intent, SessionIntent::Window);
    assert_eq!(event_outcome, GrantOutcome::Started);
}

#[test]
fn repeated_open_session_returns_the_same_token_and_marks_reuse() {
    let server = start_server(Box::new(SequenceTokens::new()));
    let mut desk = desk_for(&server, &BrowserSessionPolicy::default());
    let who = caller(":1.7");
    desk.handle(&who, HELLO_EXTENSION.as_bytes(), Instant::now());
    let first = desk.handle(&who, OPEN_WINDOW.as_bytes(), Instant::now());
    let second = desk.handle(&who, OPEN_WINDOW.as_bytes(), Instant::now());
    let (PlayerControlMessage::SessionGranted(first), PlayerControlMessage::SessionGranted(second)) =
        (decode_reply(&first), decode_reply(&second))
    else {
        panic!("оба ответа должны быть session_granted");
    };
    assert!(first.token.matches(&second.token));
    assert_eq!(second.outcome, GrantOutcome::ReusedExisting);
}

#[test]
fn malformed_message_gets_the_exact_malformed_refusal_without_message_text() {
    let server = start_server(Box::new(SequenceTokens::new()));
    let mut desk = desk_for(&server, &BrowserSessionPolicy::default());
    let outcome = desk.handle(&caller(":1.7"), UNKNOWN_FIELD.as_bytes(), Instant::now());
    assert_eq!(outcome.reply_json, REFUSED_MALFORMED);
    assert_eq!(outcome.event, None);
}

#[test]
fn incompatible_hello_is_refused_and_the_caller_stays_unknown() {
    let server = start_server(Box::new(SequenceTokens::new()));
    let mut desk = desk_for(&server, &BrowserSessionPolicy::default());
    let who = caller(":1.7");
    // Сначала договорились, потом тот же звонящий прислал несовместимый hello.
    desk.handle(&who, HELLO_EXTENSION.as_bytes(), Instant::now());
    let refused = desk.handle(&who, HELLO_MAJOR_2.as_bytes(), Instant::now());
    assert_eq!(
        refusal_of(&refused),
        ControlRefusal {
            in_reply_to: ReplyTarget::Hello {},
            // У плеера major 1 < 2 у экстеншена: обновлять надо плеер.
            reason: RefusalReason::IncompatibleVersion {
                update: BridgeSoftware::Player
            },
        }
    );
    let after = desk.handle(&who, OPEN_WINDOW.as_bytes(), Instant::now());
    assert_eq!(refusal_of(&after).reason, RefusalReason::HelloRequired {});
}

#[test]
fn callers_are_remembered_independently_and_the_oldest_is_forgotten_at_capacity() {
    let server = start_server(Box::new(SequenceTokens::new()));
    let policy = BrowserSessionPolicy {
        max_remembered_callers: 2,
        ..BrowserSessionPolicy::default()
    };
    let mut desk = desk_for(&server, &policy);
    for name in [":1.1", ":1.2", ":1.3"] {
        desk.handle(&caller(name), HELLO_EXTENSION.as_bytes(), Instant::now());
    }
    let forgotten = desk.handle(&caller(":1.1"), OPEN_WINDOW.as_bytes(), Instant::now());
    assert_eq!(
        refusal_of(&forgotten).reason,
        RefusalReason::HelloRequired {}
    );
    for name in [":1.2", ":1.3"] {
        let kept = desk.handle(&caller(name), OPEN_WINDOW.as_bytes(), Instant::now());
        assert!(
            matches!(decode_reply(&kept), PlayerControlMessage::SessionGranted(_)),
            "{name}"
        );
    }
}

#[test]
fn open_session_during_shutdown_is_refused_as_shutting_down() {
    let server = start_server(Box::new(SequenceTokens::new()));
    let mut desk = desk_for(&server, &BrowserSessionPolicy::default());
    let who = caller(":1.7");
    desk.handle(&who, HELLO_EXTENSION.as_bytes(), Instant::now());
    server.shutdown();
    let outcome = desk.handle(&who, OPEN_WINDOW.as_bytes(), Instant::now());
    assert_eq!(
        refusal_of(&outcome).reason,
        RefusalReason::PlayerShuttingDown {}
    );
    assert_eq!(outcome.event, None);
}

#[test]
fn token_source_failure_is_refused_as_unavailable_and_opens_nothing() {
    let server = start_server(Box::new(SequenceTokens::broken()));
    let mut desk = desk_for(&server, &BrowserSessionPolicy::default());
    let who = caller(":1.7");
    desk.handle(&who, HELLO_EXTENSION.as_bytes(), Instant::now());
    let outcome = desk.handle(&who, OPEN_WINDOW.as_bytes(), Instant::now());
    assert_eq!(
        refusal_of(&outcome).reason,
        RefusalReason::PlayerUnavailable {}
    );
    assert_eq!(outcome.event, None);
}

#[test]
fn fallback_reply_equals_the_encoded_unavailable_refusal() {
    let encoded = refuse(
        ReplyTarget::UnparsedMessage {},
        RefusalReason::PlayerUnavailable {},
    );
    assert_eq!(encoded.reply_json, FALLBACK_REPLY_JSON);
}

#[test]
fn caller_ids_are_validated() {
    assert_eq!(EntryCaller::new("").unwrap_err(), EntryCallerError::Empty);
    assert_eq!(
        EntryCaller::new("a".repeat(256)).unwrap_err(),
        EntryCallerError::TooLong
    );
    assert_eq!(
        EntryCaller::new(":1.\n7").unwrap_err(),
        EntryCallerError::ControlCharacter
    );
    assert_eq!(EntryCaller::new(":1.7").unwrap().as_str(), ":1.7");
}

#[test]
fn debug_output_of_entry_outcome_and_event_hides_token_title_and_domain() {
    let server = start_server(Box::new(SequenceTokens::new()));
    let mut desk = desk_for(&server, &BrowserSessionPolicy::default());
    let who = caller(":1.7");
    desk.handle(&who, HELLO_EXTENSION.as_bytes(), Instant::now());
    let outcome = desk.handle(&who, OPEN_WINDOW.as_bytes(), Instant::now());
    let printed = format!("{outcome:?}");
    for secret in ["00000000000000000000000000000001", "uaserials.com", "Шрек"] {
        assert!(
            !printed.contains(secret),
            "Debug выдал {secret:?}: {printed}"
        );
    }
    assert!(
        printed.contains("tab-12-video-3"),
        "ключ сеанса в Debug остаётся: {printed}"
    );
}

#[test]
fn opened_event_carries_the_registry_instance_and_reuse_keeps_it() {
    let server = start_server(Box::new(SequenceTokens::new()));
    let mut desk = desk_for(&server, &BrowserSessionPolicy::default());
    let who = caller(":1.7");
    desk.handle(&who, HELLO_EXTENSION.as_bytes(), Instant::now());
    let instance_of = |outcome: EntryOutcome| match outcome.event {
        Some(EntryEvent::SessionOpened { instance, .. }) => instance,
        None => panic!("нет события"),
    };
    let first = instance_of(desk.handle(&who, OPEN_WINDOW.as_bytes(), Instant::now()));
    let repeat = instance_of(desk.handle(&who, OPEN_WINDOW.as_bytes(), Instant::now()));
    assert_eq!(first, repeat);
}

/// Приложение узнаёт вытесненный экземпляр прямо из события открытия: по нему
/// оно прощается со старой вкладкой (`bid_farewell_to_replaced`, решение 127).
#[test]
fn opened_event_names_the_instance_it_replaced() {
    let server = start_server(Box::new(SequenceTokens::new()));
    let mut desk = desk_for(&server, &BrowserSessionPolicy::default());
    let who = caller(":1.7");
    desk.handle(&who, HELLO_EXTENSION.as_bytes(), Instant::now());
    let opened_of = |outcome: EntryOutcome| match outcome.event {
        Some(EntryEvent::SessionOpened {
            key,
            instance,
            replaced,
            ..
        }) => (SessionRef { key, instance }, replaced),
        None => panic!("нет события"),
    };
    let (first, first_replaced) =
        opened_of(desk.handle(&who, OPEN_WINDOW.as_bytes(), Instant::now()));
    assert_eq!(first_replaced, None, "первое открытие никого не вытесняет");
    let (_repeat, repeat_replaced) =
        opened_of(desk.handle(&who, OPEN_WINDOW.as_bytes(), Instant::now()));
    assert_eq!(repeat_replaced, None, "повтор того же сеанса — не замена");
    let other_tab = OPEN_WINDOW.replace("tab-12-video-3", "tab-13-video-1");
    let (second, second_replaced) =
        opened_of(desk.handle(&who, other_tab.as_bytes(), Instant::now()));
    assert_ne!(second, first);
    assert_eq!(second_replaced, Some(first));
}
