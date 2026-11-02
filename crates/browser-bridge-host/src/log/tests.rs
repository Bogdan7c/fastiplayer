//! Тесты журнала: в тексте только виды и размеры, никаких данных страницы.

use browser_bridge_protocol::extension_identity::BrowserFamily;
use browser_bridge_protocol::{MessageErrorKind, RefusalReason};

use super::*;
use crate::door::DoorFailure;

#[test]
fn answered_messages_name_request_size_and_reply() {
    let text = format_event(&HostEvent::MessageAnswered {
        request: RequestKind::OpenSession,
        request_bytes: 212,
        reply: ReplyKind::FromPlayer(PlayerReplyKind::SessionGranted),
    });
    assert_eq!(
        text,
        "answered request=open_session bytes=212 reply=player:session_granted"
    );
}

#[test]
fn refusals_name_their_cause() {
    let cases = [
        (
            ReplyKind::FromPlayer(PlayerReplyKind::Refused(RefusalReason::HelloRequired {})),
            "player:refused:hello_required",
        ),
        (
            ReplyKind::RefusedByHost(HostRefusalCause::Door(DoorFailure::PlayerNotResponding)),
            "host:refused:player_not_responding",
        ),
        (
            ReplyKind::RefusedByHost(HostRefusalCause::MalformedMessage(
                MessageErrorKind::UnknownField,
            )),
            "host:refused:malformed_message:UnknownField",
        ),
        (
            ReplyKind::RefusedByHost(HostRefusalCause::InvalidPlayerReply(
                MessageErrorKind::MissingField,
            )),
            "host:refused:invalid_player_reply:MissingField",
        ),
    ];
    for (reply, expected) in cases {
        let text = format_event(&HostEvent::MessageAnswered {
            request: RequestKind::Hello,
            request_bytes: 1,
            reply,
        });
        assert!(text.ends_with(&format!("reply={expected}")), "{text}");
    }
}

#[test]
fn caller_events_name_only_the_browser_family() {
    assert_eq!(
        format_event(&HostEvent::CallerAccepted {
            browser: BrowserFamily::Firefox
        }),
        "caller accepted browser=firefox"
    );
    let rejected = format_event(&HostEvent::CallerRejected(
        CallerRejection::ForeignExtension {
            browser: BrowserFamily::Chromium,
        },
    ));
    assert!(rejected.starts_with("caller rejected:"), "{rejected}");
    // ID чужого экстеншена в причину не попадает — его там просто нет.
    assert!(!rejected.contains("chrome-extension://"), "{rejected}");
}

#[test]
fn frame_problems_explain_the_exit() {
    let text = format_event(&HostEvent::FrameRejected(FrameReadError::TooLarge {
        declared: 200_000,
        limit: 131_072,
    }));
    assert_eq!(
        text,
        "broken frame, closing the port: message declares 200000 bytes, the limit is 131072"
    );
}
