//! Тесты переводчика: что уходит в дверь и что получает браузер.

use std::collections::VecDeque;

use browser_bridge_protocol::{ControlRequestId, PlayerControlMessage};

use super::*;

const HELLO: &str = r#"{"type":"hello","protocol":{"major":1,"minor":0},"software_version":"0.1.0","capabilities":[]}"#;
const OPEN_SESSION: &str = r#"{"type":"open_session","request_id":7,"session_key":"tab-1-video-1","page":{"title":"Episode","domain":"example.org"},"intent":"window","hints":{"duration":{"kind":"unknown"},"position":{"kind":"unknown"}}}"#;
/// Настоящий ответ плеера из общих фикстур протокола (без перевода строки в конце).
fn hello_accepted() -> &'static str {
    include_str!("../../../browser-bridge-protocol/fixtures/control/valid/hello-accepted.json")
        .trim_end()
}

/// Дверь-заглушка: запоминает звонки и отвечает заготовленными ответами.
#[derive(Default)]
struct ScriptedDoor {
    rung_with: Vec<String>,
    answers: VecDeque<Result<String, DoorFailure>>,
}

impl ScriptedDoor {
    fn answering(answers: impl IntoIterator<Item = Result<String, DoorFailure>>) -> Self {
        Self {
            rung_with: Vec::new(),
            answers: answers.into_iter().collect(),
        }
    }
}

impl PlayerDoor for ScriptedDoor {
    fn ring(&mut self, message_json: &str) -> Result<String, DoorFailure> {
        self.rung_with.push(message_json.to_owned());
        self.answers
            .pop_front()
            .expect("the test scripted an answer for every ring")
    }
}

fn decoded(reply: &RelayedReply) -> PlayerControlMessage {
    decode_message(reply.json.as_bytes()).expect("the browser always gets a valid reply")
}

#[test]
fn hello_is_forwarded_verbatim_and_the_player_reply_passes_through() {
    let mut relay = Relay::new(ScriptedDoor::answering([Ok(hello_accepted().to_owned())]));
    let reply = relay.handle(HELLO.as_bytes()).unwrap();
    assert_eq!(relay.door.rung_with, vec![HELLO.to_owned()]);
    assert_eq!(reply.json, hello_accepted());
    assert_eq!(reply.request, RequestKind::Hello);
    assert_eq!(
        reply.reply,
        ReplyKind::FromPlayer(PlayerReplyKind::HelloAccepted)
    );
}

#[test]
fn player_refusal_passes_through_as_a_player_reply() {
    let hello_required = r#"{"type":"refused","in_reply_to":{"kind":"open_session","request_id":7},"reason":{"kind":"hello_required"}}"#;
    let mut relay = Relay::new(ScriptedDoor::answering([Ok(hello_required.to_owned())]));
    let reply = relay.handle(OPEN_SESSION.as_bytes()).unwrap();
    assert_eq!(reply.json, hello_required);
    assert_eq!(
        reply.reply,
        ReplyKind::FromPlayer(PlayerReplyKind::Refused(RefusalReason::HelloRequired {}))
    );
}

#[test]
fn garbage_is_refused_as_unparsed_without_ringing() {
    for (garbage, kind) in [
        (&b"not json"[..], MessageErrorKind::Syntax),
        (b"", MessageErrorKind::Syntax),
        (
            br#"{"type":"attach","session_key":"k","token":"t"}"#,
            MessageErrorKind::UnknownType,
        ),
        (br#"[1,2]"#, MessageErrorKind::NotAnObject),
        (&[0xff, 0xfe][..], MessageErrorKind::NotUtf8),
    ] {
        let mut relay = Relay::new(ScriptedDoor::default());
        let reply = relay.handle(garbage).unwrap();
        assert!(
            relay.door.rung_with.is_empty(),
            "{garbage:?} must not reach the door"
        );
        assert_eq!(reply.request, RequestKind::Unparsed);
        assert_eq!(
            decoded(&reply),
            PlayerControlMessage::Refused(ControlRefusal {
                in_reply_to: ReplyTarget::UnparsedMessage {},
                reason: RefusalReason::MalformedMessage { error: kind },
            }),
            "{garbage:?}"
        );
    }
}

#[test]
fn hello_with_an_unknown_field_is_refused_strictly() {
    let with_extra = r#"{"type":"hello","protocol":{"major":1,"minor":0},"software_version":"0.1.0","capabilities":[],"extension_id":"x"}"#;
    let mut relay = Relay::new(ScriptedDoor::default());
    let reply = relay.handle(with_extra.as_bytes()).unwrap();
    assert!(relay.door.rung_with.is_empty());
    assert_eq!(
        reply.reply,
        ReplyKind::RefusedByHost(HostRefusalCause::MalformedMessage(
            MessageErrorKind::UnknownField
        ))
    );
}

#[test]
fn door_failures_become_refusals_addressed_to_the_request() {
    let cases = [
        (
            DoorFailure::PlayerUnavailable,
            RefusalReason::PlayerUnavailable {},
        ),
        (
            DoorFailure::UnsupportedPlatform,
            RefusalReason::PlayerUnavailable {},
        ),
        (
            DoorFailure::PlayerNotResponding,
            RefusalReason::PlayerNotResponding {},
        ),
        (
            DoorFailure::PlayerShuttingDown,
            RefusalReason::PlayerShuttingDown {},
        ),
        (
            DoorFailure::MessageRefusedByTransport,
            RefusalReason::MalformedMessage {
                error: MessageErrorKind::TooLarge,
            },
        ),
    ];
    for (failure, reason) in cases {
        let mut relay = Relay::new(ScriptedDoor::answering([Err(failure), Err(failure)]));
        let hello_reply = relay.handle(HELLO.as_bytes()).unwrap();
        assert_eq!(
            decoded(&hello_reply),
            PlayerControlMessage::Refused(ControlRefusal {
                in_reply_to: ReplyTarget::Hello {},
                reason,
            }),
            "{failure:?}"
        );
        let open_reply = relay.handle(OPEN_SESSION.as_bytes()).unwrap();
        assert_eq!(
            decoded(&open_reply),
            PlayerControlMessage::Refused(ControlRefusal {
                in_reply_to: ReplyTarget::OpenSession {
                    request_id: ControlRequestId(7),
                },
                reason,
            }),
            "{failure:?}"
        );
        assert_eq!(
            open_reply.reply,
            ReplyKind::RefusedByHost(HostRefusalCause::Door(failure))
        );
    }
}

#[test]
fn an_invalid_player_reply_never_reaches_the_browser() {
    for broken in [
        "",
        "{}",
        r#"{"type":"attached"}"#,
        r#"{"type":"hello_accepted"}"#,
    ] {
        let mut relay = Relay::new(ScriptedDoor::answering([Ok(broken.to_owned())]));
        let reply = relay.handle(HELLO.as_bytes()).unwrap();
        assert_ne!(reply.json, broken);
        assert_eq!(
            decoded(&reply),
            PlayerControlMessage::Refused(ControlRefusal {
                in_reply_to: ReplyTarget::Hello {},
                reason: RefusalReason::PlayerUnavailable {},
            }),
            "{broken:?}"
        );
        assert!(matches!(
            reply.reply,
            ReplyKind::RefusedByHost(HostRefusalCause::InvalidPlayerReply(_))
        ));
    }
}

#[test]
fn every_refusal_reason_has_a_log_name() {
    // Имена журнала совпадают с именами протокола (snake_case в JSON).
    let reasons = [
        RefusalReason::PlayerUnavailable {},
        RefusalReason::PlayerNotResponding {},
        RefusalReason::PlayerShuttingDown {},
        RefusalReason::ProtectedContent {},
        RefusalReason::UnsupportedCodec {},
        RefusalReason::MalformedMessage {
            error: MessageErrorKind::Syntax,
        },
        RefusalReason::HelloRequired {},
    ];
    for reason in reasons {
        let json = encode_message(&PlayerControlMessage::Refused(ControlRefusal {
            in_reply_to: ReplyTarget::Hello {},
            reason,
        }))
        .unwrap();
        let expected = format!(
            r#""reason":{{"kind":"{}""#,
            refusal_reason_log_name(&reason)
        );
        assert!(json.contains(&expected), "{json} vs {expected}");
    }
}
