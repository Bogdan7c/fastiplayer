//! Противодавление (сессия 09b2): пока приёмник байтов отвечает «места нет», поток
//! соединения не читает сокет — ни одна рамка не доходит до приёмника; когда место
//! появляется, всё отправленное доходит в порядке провода. Просьба закрыть сеанс
//! исполняется и во время удержания (поток соединения не спит на сокете).

use std::sync::Arc;
use std::time::{Duration, Instant};

use browser_bridge_protocol::CURRENT_PROTOCOL_VERSION;
use browser_bridge_protocol::{SessionCloseReason, StreamGeneration};
use browser_bridge_session::{
    BrowserSessionServer, ChunkSource, IntakeRoom, SessionByteSink, SessionChunk, SessionRef,
};

use crate::support::{
    RecordingEvents, SequenceTokens, Watched, attach, data_frame_for, open_request,
    read_close_code, send_binary, send_text, test_policy,
};

/// Что приёмник видел и держит ли он сейчас поток соединения.
#[derive(Clone, Default)]
struct HoldState {
    /// «Места нет», пока `true`.
    holding: bool,
    /// Номера принятых рамок в порядке прихода.
    accepted_sequences: Vec<u64>,
}

/// Приёмник, которым управляет тест: держит поток соединения, пока его не отпустят.
#[derive(Default)]
struct HoldingSink {
    state: Watched<HoldState>,
}

impl SessionByteSink for HoldingSink {
    fn accept(&self, chunk: SessionChunk<'_>) {
        let ChunkSource::SourceBuffer(buffer) = chunk.source else {
            panic!("в этом тесте только байты SourceBuffer");
        };
        self.state
            .update(|state| state.accepted_sequences.push(buffer.sequence.0));
    }

    fn session_ended(&self, _session: &SessionRef) {}

    fn wait_for_room(&self, _session: &SessionRef, patience: Duration) -> IntakeRoom {
        // Ждём отпускания не дольше терпения, как настоящая очередь.
        let deadline = Instant::now() + patience;
        loop {
            if !self.state.get().holding {
                return IntakeRoom::Available;
            }
            if Instant::now() >= deadline {
                return IntakeRoom::Exhausted;
            }
            std::thread::sleep(Duration::from_millis(1));
        }
    }
}

impl Default for Watched<HoldState> {
    fn default() -> Self {
        Self::new()
    }
}

/// Рамки SourceBuffer 0 поколения 1 с номерами подряд.
fn frames(count: u64) -> Vec<Vec<u8>> {
    (0..count)
        .map(|sequence| {
            let mut frame = data_frame_for(StreamGeneration::FIRST.get(), 0);
            frame[16..24].copy_from_slice(&sequence.to_le_bytes());
            frame
        })
        .collect()
}

/// Сервер с удерживающим приёмником и подключённое соединение.
fn attached_with_holding_sink() -> (
    BrowserSessionServer,
    Arc<HoldingSink>,
    tungstenite::WebSocket<std::net::TcpStream>,
) {
    let sink = Arc::new(HoldingSink::default());
    let events = Arc::new(RecordingEvents {
        seen: Watched::new(),
        always_full: false,
    });
    let server = BrowserSessionServer::start(
        test_policy(),
        Box::new(SequenceTokens(0)),
        Arc::clone(&sink) as Arc<dyn SessionByteSink>,
        events,
    )
    .unwrap();
    let opened = server
        .control()
        .open_session(
            &open_request("held"),
            CURRENT_PROTOCOL_VERSION,
            Instant::now(),
        )
        .unwrap();
    let socket = attach(server.port().get(), "held", &opened.token);
    (server, sink, socket)
}

/// Отправляет поколение и `count` рамок.
fn send_generation_and_frames(
    socket: &mut tungstenite::WebSocket<std::net::TcpStream>,
    count: u64,
) {
    send_text(socket, r#"{"type":"generation_started","generation":1}"#);
    send_text(
        socket,
        r#"{"type":"source_buffer_added","generation":1,"source_buffer":0,"mime_type":"video/mp4; codecs=\"avc1.42c00a\""}"#,
    );
    for frame in frames(count) {
        send_binary(socket, frame);
    }
}

#[test]
fn frames_wait_on_the_wire_while_the_sink_has_no_room_and_arrive_in_order_after() {
    let (server, sink, mut socket) = attached_with_holding_sink();
    sink.state.update(|state| state.holding = true);
    send_generation_and_frames(&mut socket, 5);

    // Пока места нет, ни одна рамка не доходит: поток соединения сокет не читает.
    let quiet_until = Instant::now() + Duration::from_millis(300);
    while Instant::now() < quiet_until {
        assert_eq!(
            sink.state.get().accepted_sequences,
            Vec::<u64>::new(),
            "рамка дошла до приёмника во время удержания"
        );
        std::thread::sleep(Duration::from_millis(10));
    }

    sink.state.update(|state| state.holding = false);
    let state = sink.state.wait_for(
        "все рамки после отпускания",
        |state| state.accepted_sequences.len() == 5,
    );
    assert_eq!(state.accepted_sequences, [0, 1, 2, 3, 4]);
    server.shutdown();
}

#[test]
fn a_close_request_is_honoured_while_the_sink_holds_the_connection() {
    let (server, sink, mut socket) = attached_with_holding_sink();
    sink.state.update(|state| state.holding = true);
    send_generation_and_frames(&mut socket, 3);
    std::thread::sleep(Duration::from_millis(100));

    // Плеер закрывает сеанс: удержание не мешает — кадр закрытия приходит с кодом.
    let session = browser_bridge_session::SessionRef {
        key: browser_bridge_protocol::SessionKey::new("held").unwrap(),
        instance: browser_bridge_session::SessionInstance(1),
    };
    let outcome = server
        .control()
        .close_session(&session, SessionCloseReason::PlayerClosed);
    assert_eq!(outcome, browser_bridge_session::CloseOutcome::Closed);
    assert_eq!(
        read_close_code(&mut socket),
        SessionCloseReason::PlayerClosed.close_code()
    );
    // Удержанные рамки так и не дошли: сеанс кончился раньше.
    assert_eq!(sink.state.get().accepted_sequences, Vec::<u64>::new());
    server.shutdown();
}
