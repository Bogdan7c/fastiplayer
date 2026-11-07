//! Отладочный отвод провода: видит сырое, до разбора и сторожа поколений, и
//! ничего не меняет в работе сервера.

use std::sync::{Arc, Mutex};
use std::time::Instant;

use browser_bridge_protocol::CURRENT_PROTOCOL_VERSION;
use browser_bridge_session::SessionWireTap;
use browser_bridge_session::{BrowserSessionServer, SessionByteSink, SessionEventSink, SessionRef};

use super::support::{
    RecordingByteSink, RecordingEvents, SequenceTokens, Watched, attach, data_frame_for,
    open_request, read_close_code, send_binary, send_text, test_policy,
};

/// Что отвод увидел, по порядку.
#[derive(Debug, Clone, PartialEq, Eq)]
enum Seen {
    PageText(Vec<u8>),
    PageBinary(Vec<u8>),
    PlayerText(String),
    Closed,
}

#[derive(Default)]
struct TapLog {
    seen: Mutex<Vec<Seen>>,
}

impl SessionWireTap for TapLog {
    fn page_text(&self, _session: &SessionRef, bytes: &[u8]) {
        self.seen
            .lock()
            .unwrap()
            .push(Seen::PageText(bytes.to_vec()));
    }
    fn page_binary(&self, _session: &SessionRef, bytes: &[u8]) {
        self.seen
            .lock()
            .unwrap()
            .push(Seen::PageBinary(bytes.to_vec()));
    }
    fn player_text(&self, _session: &SessionRef, json: &str) {
        self.seen
            .lock()
            .unwrap()
            .push(Seen::PlayerText(json.to_owned()));
    }
    fn connection_closed(&self, _session: &SessionRef) {
        self.seen.lock().unwrap().push(Seen::Closed);
    }
}

#[test]
fn tap_sees_raw_wire_including_stale_frames_that_the_gate_drops() {
    let bytes = Arc::new(RecordingByteSink::default());
    let events = Arc::new(RecordingEvents {
        seen: Watched::new(),
        always_full: false,
    });
    let tap = Arc::new(TapLog::default());
    let server = BrowserSessionServer::start_with_wire_tap(
        test_policy(),
        Box::new(SequenceTokens(0)),
        Arc::clone(&bytes) as Arc<dyn SessionByteSink>,
        Arc::clone(&events) as Arc<dyn SessionEventSink>,
        Some(Arc::clone(&tap) as Arc<dyn SessionWireTap>),
    )
    .unwrap();
    let opened = server
        .control()
        .open_session(
            &open_request("tapped"),
            CURRENT_PROTOCOL_VERSION,
            Instant::now(),
        )
        .unwrap();
    let mut socket = attach(server.port().get(), "tapped", &opened.token);

    // Поколение 2 вытесняет 1; рамка поколения 1 после этого устарела: сторож её
    // выбросит, а отвод обязан её увидеть.
    let first = r#"{"type":"generation_started","generation":1}"#;
    let second = r#"{"type":"generation_started","generation":2}"#;
    send_text(&mut socket, first);
    send_text(&mut socket, second);
    let stale_frame = data_frame_for(1, 0);
    send_binary(&mut socket, stale_frame.clone());
    let tab_gone = r#"{"type":"tab_gone","reason":"closed"}"#;
    send_text(&mut socket, tab_gone);
    read_close_code(&mut socket);
    let report = server.shutdown();

    let seen = tap.seen.lock().unwrap().clone();
    // Первые четыре записи — ровно то, что отправила страница, байт в байт.
    assert_eq!(
        &seen[..4],
        &[
            Seen::PageText(first.as_bytes().to_vec()),
            Seen::PageText(second.as_bytes().to_vec()),
            Seen::PageBinary(stale_frame),
            Seen::PageText(tab_gone.as_bytes().to_vec()),
        ]
    );
    assert_eq!(seen.last(), Some(&Seen::Closed));
    // Сервер своё дело сделал как без отвода: устаревшая рамка выброшена, байты не дошли.
    assert_eq!(report.dropped_stale_frames, 1);
    assert_eq!(bytes.received.get().frames, 0);
}
