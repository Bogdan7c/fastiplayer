//! Обещание порядка (сессия 03b): сообщения страницы и байты одного соединения
//! приходят в приёмники из одного потока, без перекрытия, в порядке кадров на проводе.
//!
//! Один записывающий объект реализует ОБА приёмника и складывает всё в одну очередь —
//! так же поступит переходник 03b. Тест сверяет очередь с отправленной
//! последовательностью кадров целиком: любая перестановка «сообщение до байтов» /
//! «байты до сообщения» ломает равенство.

use std::sync::{Arc, Mutex};
use std::thread::ThreadId;
use std::time::Instant;

use browser_bridge_protocol::{
    AppendSequence, DataFrameHeader, PageSessionMessage, SessionCloseReason,
    SourceBufferBytesHeader, SourceBufferIndex, StreamGeneration, decode_message,
    encode_data_frame,
};
use browser_bridge_session::{
    BrowserSessionServer, ChunkSource, EventSinkFull, SessionByteSink, SessionChunk,
    SessionEventSink, SessionLifecycleEvent, SessionRef,
};

use crate::support::{
    SequenceTokens, attach, open_request, read_close_code, send_binary, send_text, test_policy,
};

/// Что приёмник увидел (или что тест ждёт увидеть) — один элемент единой очереди.
#[derive(Debug, Clone, PartialEq, Eq)]
enum Observed {
    /// Сообщение страницы целиком.
    PageMessage(PageSessionMessage),
    /// Кусок байтов SourceBuffer: где, какой по счёту и что внутри.
    SourceBufferBytes {
        source_buffer: u16,
        sequence: u64,
        payload: Vec<u8>,
    },
}

/// Запись одного вызова приёмника данных: что и из какого потока.
struct DataCall {
    observed: Observed,
    thread: ThreadId,
}

/// Один объект на оба трейта, одна очередь под замком (как у переходника 03b).
#[derive(Default)]
struct OrderedQueueSink {
    data_calls: Mutex<Vec<DataCall>>,
}

impl OrderedQueueSink {
    fn push(&self, observed: Observed) {
        self.data_calls.lock().unwrap().push(DataCall {
            observed,
            thread: std::thread::current().id(),
        });
    }
}

impl SessionByteSink for OrderedQueueSink {
    fn accept(&self, chunk: SessionChunk<'_>) {
        match chunk.source {
            ChunkSource::SourceBuffer(buffer) => self.push(Observed::SourceBufferBytes {
                source_buffer: buffer.source_buffer.get(),
                sequence: buffer.sequence.0,
                payload: buffer.payload.to_vec(),
            }),
            ChunkSource::Range(_) => panic!("в этом тесте нет запросов диапазонов"),
        }
    }

    fn session_ended(&self, _session: &SessionRef) {}
}

impl SessionEventSink for OrderedQueueSink {
    fn publish_page_message(
        &self,
        _session: &SessionRef,
        message: PageSessionMessage,
    ) -> Result<(), EventSinkFull> {
        self.push(Observed::PageMessage(message));
        Ok(())
    }

    fn publish_lifecycle(&self, _event: SessionLifecycleEvent) {}
}

/// Кадр на провод и то, что из него должен увидеть приёмник.
enum WireStep {
    Text(&'static str),
    Bytes {
        source_buffer: u16,
        sequence: u64,
        payload: Vec<u8>,
    },
}

impl WireStep {
    fn bytes(source_buffer: u16, sequence: u64, tag: u8) -> Self {
        // Груз различим по кадрам: перепутанные куски одного SourceBuffer тоже видны.
        Self::Bytes {
            source_buffer,
            sequence,
            payload: vec![tag; 64],
        }
    }

    fn expected(&self) -> Observed {
        match self {
            Self::Text(json) => Observed::PageMessage(decode_message(json.as_bytes()).unwrap()),
            Self::Bytes {
                source_buffer,
                sequence,
                payload,
            } => Observed::SourceBufferBytes {
                source_buffer: *source_buffer,
                sequence: *sequence,
                payload: payload.clone(),
            },
        }
    }

    fn send(&self, socket: &mut tungstenite::WebSocket<std::net::TcpStream>) {
        match self {
            Self::Text(json) => send_text(socket, json),
            Self::Bytes {
                source_buffer,
                sequence,
                payload,
            } => {
                let header = DataFrameHeader::SourceBufferBytes(SourceBufferBytesHeader {
                    generation: StreamGeneration::new(1).unwrap(),
                    source_buffer: SourceBufferIndex::new(*source_buffer).unwrap(),
                    sequence: AppendSequence(*sequence),
                });
                send_binary(socket, encode_data_frame(&header, payload).unwrap());
            }
        }
    }
}

/// Перемешанные сообщения MSE и байты двух SourceBuffer, проходящие сторожа потока:
/// поколение с 1, номера рамок каждого SourceBuffer подряд с 0.
fn interleaved_script() -> Vec<WireStep> {
    vec![
        WireStep::Text(r#"{"type":"generation_started","generation":1}"#),
        WireStep::Text(
            r#"{"type":"source_buffer_added","generation":1,"source_buffer":0,"mime_type":"video/mp4; codecs=\"avc1.42c00a\""}"#,
        ),
        WireStep::Text(
            r#"{"type":"source_buffer_added","generation":1,"source_buffer":1,"mime_type":"audio/mp4; codecs=\"mp4a.40.2\""}"#,
        ),
        WireStep::bytes(0, 0, 0xA0),
        WireStep::bytes(1, 0, 0xB0),
        WireStep::Text(
            r#"{"type":"timestamp_offset_changed","generation":1,"source_buffer":1,"offset_us":-21333}"#,
        ),
        WireStep::bytes(1, 1, 0xB1),
        WireStep::bytes(0, 1, 0xA1),
        WireStep::Text(r#"{"type":"source_buffer_aborted","generation":1,"source_buffer":0}"#),
        WireStep::bytes(0, 2, 0xA2),
        WireStep::Text(
            r#"{"type":"timestamp_offset_changed","generation":1,"source_buffer":0,"offset_us":500000}"#,
        ),
        WireStep::bytes(0, 3, 0xA3),
        WireStep::bytes(1, 2, 0xB2),
        WireStep::Text(r#"{"type":"end_of_stream","generation":1}"#),
        // Конец: `tab_gone` тоже сообщение страницы и идёт последним в той же очереди.
        WireStep::Text(r#"{"type":"tab_gone","reason":"closed"}"#),
    ]
}

#[test]
fn page_messages_and_bytes_reach_one_queue_in_wire_order_from_one_thread() {
    let sink = Arc::new(OrderedQueueSink::default());
    let server = BrowserSessionServer::start(
        test_policy(),
        Box::new(SequenceTokens(0)),
        Arc::clone(&sink) as Arc<dyn SessionByteSink>,
        Arc::clone(&sink) as Arc<dyn SessionEventSink>,
    )
    .unwrap();
    let opened = server
        .control()
        .open_session(&open_request("ordered"), Instant::now())
        .unwrap();
    let mut socket = attach(server.port().get(), "ordered", &opened.token);

    let script = interleaved_script();
    for step in &script {
        step.send(&mut socket);
    }
    // 4002 приходит после обработки `tab_gone`, а он — последний кадр: к этому моменту
    // сервер выдал приёмникам всё отправленное раньше.
    assert_eq!(
        read_close_code(&mut socket),
        SessionCloseReason::TabGone.close_code()
    );
    let report = server.shutdown();

    let data_calls = sink.data_calls.lock().unwrap();
    let observed: Vec<Observed> = data_calls
        .iter()
        .map(|call| call.observed.clone())
        .collect();
    let expected: Vec<Observed> = script.iter().map(WireStep::expected).collect();
    assert_eq!(
        observed, expected,
        "единая очередь сообщений и байтов не совпала с порядком на проводе"
    );

    let connection_thread = data_calls[0].thread;
    assert!(
        data_calls
            .iter()
            .all(|call| call.thread == connection_thread),
        "сообщения и байты одного соединения пришли из разных потоков"
    );
    assert_ne!(
        connection_thread,
        std::thread::current().id(),
        "выдача должна идти из потока соединения сервера, а не из потока теста"
    );
    assert_eq!(report.dropped_page_messages, 0);
    assert_eq!(report.dropped_stale_frames, 0);
    assert_eq!(report.delivered_chunks, 7);
}
