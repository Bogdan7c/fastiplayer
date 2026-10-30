//! Журнал переходника не несёт текста от страницы (инвариант 7): только класс
//! ошибки и «поддержан ли тип», без описания ошибки и без строки MIME.

use super::*;
use crate::browser_session::test_log_capture::with_captured_log;

/// Метка, которую страница «подсунула» в свои данные.
const MARKER: &str = "page-secret-marker";

#[test]
fn a_stream_failure_is_logged_by_class_without_its_text() {
    let (mut translator, _log, failure) = fake_translator();
    run(
        &mut translator,
        [started(1, 1), added(1, 1, 0, "video/mp4")],
    );
    *failure.0.lock().expect("failure") = Some(PushedStreamError::Demux {
        lane: LaneId::new(0),
        reason: format!("box named {MARKER} is broken"),
    });

    let (notices, journal) = with_captured_log(|| run(&mut translator, [bytes(1, 1, 0, b"bytes")]));

    assert_eq!(
        seen(&notices),
        vec![Seen::Reject(1, 1, StreamRejection::UnparsableMedia)]
    );
    assert!(journal.contains("Pushed stream failed"), "{journal}");
    assert!(journal.contains("UnparsableMedia"), "{journal}");
    assert!(!journal.contains(MARKER), "{journal}");
}

#[test]
fn an_unsupported_mime_is_logged_without_the_mime_text() {
    let (mut translator, _log, _failure) = fake_translator();
    let mime = format!("video/x-{MARKER}");

    let (notices, journal) =
        with_captured_log(|| run(&mut translator, [started(1, 1), added(1, 1, 0, &mime)]));

    assert_eq!(
        seen(&notices),
        vec![
            Seen::NewGeneration(1, 1),
            Seen::Reject(1, 1, StreamRejection::UnsupportedCodec),
        ]
    );
    assert!(
        journal.contains("Unsupported SourceBuffer type"),
        "{journal}"
    );
    assert!(!journal.contains(MARKER), "{journal}");
}
