//! Тесты цикла посредника: порядок «вызывающий → рамки → ответы» и коды выхода.

use std::cell::Cell;
use std::ffi::OsString;
use std::io::Cursor;

use browser_bridge_protocol::extension_identity::{
    BrowserFamily, CHROMIUM_DEVELOPMENT_EXTENSION_ID,
};
use browser_bridge_protocol::{PlayerControlMessage, decode_message};

use super::*;
use crate::door::DoorFailure;

/// Журнал в память.
#[derive(Default)]
struct RecordingLog {
    events: Vec<HostEvent>,
}

impl HostLog for RecordingLog {
    fn record(&mut self, event: &HostEvent) {
        self.events.push(event.clone());
    }
}

/// Дверь, на которую никто не отвечает: «плеер недоступен».
struct UnavailableDoor;

impl PlayerDoor for UnavailableDoor {
    fn ring(&mut self, _message_json: &str) -> Result<String, DoorFailure> {
        Err(DoorFailure::PlayerUnavailable)
    }
}

fn our_chromium_arguments() -> Vec<OsString> {
    vec![OsString::from(format!(
        "chrome-extension://{CHROMIUM_DEVELOPMENT_EXTENSION_ID}/"
    ))]
}

fn frame(body: &[u8]) -> Vec<u8> {
    let mut bytes = u32::try_from(body.len()).unwrap().to_ne_bytes().to_vec();
    bytes.extend_from_slice(body);
    bytes
}

/// Разрезает вывод посредника на ответы.
fn replies(output: &[u8]) -> Vec<PlayerControlMessage> {
    let mut rest = output;
    let mut messages = Vec::new();
    while !rest.is_empty() {
        let (prefix, after) = rest.split_first_chunk::<4>().expect("whole prefix");
        let length = usize::try_from(u32::from_ne_bytes(*prefix)).unwrap();
        let (body, after) = after.split_at(length);
        messages.push(decode_message(body).expect("valid reply"));
        rest = after;
    }
    messages
}

#[test]
fn a_foreign_caller_exits_before_the_door_is_opened() {
    let door_opened = Cell::new(false);
    let mut log = RecordingLog::default();
    let mut output = Vec::new();
    let exit = run_host(
        &[OsString::from(
            "chrome-extension://aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa/",
        )],
        Cursor::new(frame(b"{}")),
        &mut output,
        || {
            door_opened.set(true);
            UnavailableDoor
        },
        &mut log,
    );
    let rejection = CallerRejection::ForeignExtension {
        browser: BrowserFamily::Chromium,
    };
    assert_eq!(exit, HostExit::ForeignCaller(rejection));
    assert_eq!(exit.exit_code(), 2);
    assert!(
        !door_opened.get(),
        "a foreign caller must not reach the bus"
    );
    assert!(output.is_empty());
    assert_eq!(log.events, vec![HostEvent::CallerRejected(rejection)]);
}

#[test]
fn every_message_gets_one_reply_until_the_input_closes() {
    let mut input = frame(b"garbage");
    input.extend(frame(
        br#"{"type":"hello","protocol":{"major":1,"minor":0},"software_version":"0.1.0","capabilities":[]}"#,
    ));
    let mut log = RecordingLog::default();
    let mut output = Vec::new();
    let exit = run_host(
        &our_chromium_arguments(),
        Cursor::new(input),
        &mut output,
        || UnavailableDoor,
        &mut log,
    );
    assert_eq!(exit, HostExit::InputClosed);
    assert_eq!(exit.exit_code(), 0);
    let replies = replies(&output);
    assert_eq!(replies.len(), 2);
    assert!(matches!(
        &replies[1],
        PlayerControlMessage::Refused(refusal)
            if refusal.reason == browser_bridge_protocol::RefusalReason::PlayerUnavailable {}
    ));
    assert_eq!(
        log.events.first(),
        Some(&HostEvent::CallerAccepted {
            browser: BrowserFamily::Chromium
        })
    );
    assert_eq!(log.events.last(), Some(&HostEvent::InputClosed));
}

#[test]
fn a_broken_frame_closes_the_port_after_answering_the_previous_ones() {
    let mut input = frame(b"{}");
    input.extend([1, 2]);
    let mut output = Vec::new();
    let exit = run_host(
        &our_chromium_arguments(),
        Cursor::new(input),
        &mut output,
        || UnavailableDoor,
        &mut RecordingLog::default(),
    );
    assert_eq!(
        exit,
        HostExit::BrokenFrame(FrameReadError::TruncatedLength { received: 2 })
    );
    assert_eq!(exit.exit_code(), 3);
    assert_eq!(replies(&output).len(), 1);
}

#[test]
fn an_input_closed_without_messages_never_opens_the_door() {
    // Браузер сразу закрыл порт: посредник выходит, шину не трогает (дверь
    // открывается лениво, но и сам объект двери не нужен до первого звонка).
    let mut rings = 0;
    struct CountingDoor<'counter>(&'counter mut i32);
    impl PlayerDoor for CountingDoor<'_> {
        fn ring(&mut self, _message_json: &str) -> Result<String, DoorFailure> {
            *self.0 += 1;
            Err(DoorFailure::PlayerUnavailable)
        }
    }
    let exit = run_host(
        &our_chromium_arguments(),
        Cursor::new(Vec::new()),
        Vec::new(),
        || CountingDoor(&mut rings),
        &mut RecordingLog::default(),
    );
    assert_eq!(exit, HostExit::InputClosed);
    assert_eq!(rings, 0);
}

/// Вывод, который браузер уже закрыл.
struct ClosedOutput;

impl std::io::Write for ClosedOutput {
    fn write(&mut self, _bytes: &[u8]) -> std::io::Result<usize> {
        Err(std::io::Error::from(std::io::ErrorKind::BrokenPipe))
    }

    fn flush(&mut self) -> std::io::Result<()> {
        Ok(())
    }
}

#[test]
fn a_closed_output_ends_the_host_without_panic() {
    let exit = run_host(
        &our_chromium_arguments(),
        Cursor::new(frame(b"{}")),
        ClosedOutput,
        || UnavailableDoor,
        &mut RecordingLog::default(),
    );
    assert_eq!(
        exit,
        HostExit::ReplyNotWritten(FrameWriteError::Write {
            kind: std::io::ErrorKind::BrokenPipe
        })
    );
    assert_eq!(exit.exit_code(), 4);
}
