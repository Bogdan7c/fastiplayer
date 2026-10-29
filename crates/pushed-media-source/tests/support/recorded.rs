//! Записанные потоки протокола моста → источник, через того же сторожа, что и сервер 02.
//!
//! Переходник протокола в процессе браузерного режима — сессия 03b. Здесь его
//! минимальная тестовая копия: ровно столько, сколько нужно, чтобы байты
//! записанного потока дошли до источника в том же порядке и с той же отбраковкой
//! устаревших поколений, что и вживую.

use std::path::Path;

use browser_bridge_protocol::{
    DataFrameHeader, GateDecision, MediaDuration, PageSessionMessage, StreamGate,
    decode_data_frame, decode_message,
};
use pushed_media_source::{
    LaneId, PushedContainer, PushedDuration, PushedSourcePolicy, PushedStreamOpener,
    PushedStreamWriter, TimestampOffset, pushed_stream,
};

use super::media::protocol_fixtures_dir;

/// Шаг записанного потока.
enum Step {
    /// JSON-сообщение страницы.
    Text(Vec<u8>),
    /// Двоичная рамка с байтами.
    Binary(Vec<u8>),
}

/// Последнее поколение записанного потока, доставленное до конца.
pub struct ReplayedStream {
    /// Сторона приёма (живёт, пока жив поток).
    pub writer: PushedStreamWriter,
    /// Сторона открытия.
    pub opener: PushedStreamOpener,
}

/// Читает шаги записанного потока `fixtures/streams/<name>/stream.json`.
fn load_steps(name: &str) -> Vec<Step> {
    let directory = protocol_fixtures_dir().join("streams").join(name);
    let manifest: serde_json::Value =
        serde_json::from_slice(&std::fs::read(directory.join("stream.json")).expect("manifest"))
            .expect("manifest JSON");
    let read = |file: &serde_json::Value| -> Vec<u8> {
        std::fs::read(Path::new(&directory).join(file.as_str().expect("file name"))).expect("step")
    };
    manifest["steps"]
        .as_array()
        .expect("steps")
        .iter()
        .map(|step| match (step.get("text"), step.get("binary")) {
            (Some(file), None) => Step::Text(read(file)),
            (None, Some(file)) => Step::Binary(read(file)),
            _ => panic!("a step is either text or binary"),
        })
        .collect()
}

/// Проигрывает записанный поток в источник; каждое новое поколение — новый поток.
pub fn replay(name: &str) -> ReplayedStream {
    let mut gate = StreamGate::new();
    let mut current: Option<ReplayedStream> = None;
    for step in load_steps(name) {
        match step {
            Step::Text(json) => {
                let message: PageSessionMessage = decode_message(&json).expect("valid message");
                if gate.apply_message(&message).expect("no violation") == GateDecision::DropStale {
                    continue;
                }
                if let PageSessionMessage::GenerationStarted { .. } = message {
                    let policy = PushedSourcePolicy::recommended().expect("policy");
                    let (writer, opener) = pushed_stream(policy).expect("stream");
                    current = Some(ReplayedStream { writer, opener });
                    continue;
                }
                let stream = current.as_mut().expect("generation announced first");
                apply_message(&mut stream.writer, message);
            }
            Step::Binary(frame) => {
                let frame = decode_data_frame(&frame).expect("valid frame");
                let DataFrameHeader::SourceBufferBytes(header) = frame.header else {
                    panic!("recorded streams carry only source buffer bytes");
                };
                if gate
                    .admit_source_buffer_bytes(&header)
                    .expect("no violation")
                    == GateDecision::DropStale
                {
                    continue;
                }
                let stream = current.as_mut().expect("generation announced first");
                stream
                    .writer
                    .append(LaneId::new(header.source_buffer.get()), frame.payload)
                    .expect("bytes accepted");
            }
        }
    }
    current.expect("at least one generation")
}

/// Сообщение MSE → вызов стороны приёма.
pub fn apply_message(writer: &mut PushedStreamWriter, message: PageSessionMessage) {
    match message {
        PageSessionMessage::SourceBufferAdded {
            source_buffer,
            mime_type,
            ..
        } => {
            let container = PushedContainer::from_mime_type(mime_type.as_str()).expect("container");
            writer
                .add_lane(LaneId::new(source_buffer.get()), container)
                .expect("lane");
        }
        PageSessionMessage::SourceBufferTypeChanged {
            source_buffer,
            mime_type,
            ..
        } => {
            let container = PushedContainer::from_mime_type(mime_type.as_str()).expect("container");
            writer
                .change_container(LaneId::new(source_buffer.get()), container)
                .expect("change type");
        }
        PageSessionMessage::TimestampOffsetChanged {
            source_buffer,
            offset_us,
            ..
        } => writer
            .set_timestamp_offset(
                LaneId::new(source_buffer.get()),
                TimestampOffset::from_micros(offset_us.as_micros()),
            )
            .expect("offset"),
        PageSessionMessage::SourceBufferAborted { source_buffer, .. } => {
            writer
                .abort(LaneId::new(source_buffer.get()))
                .expect("abort");
        }
        PageSessionMessage::DurationChanged { duration, .. } => {
            writer.set_duration(match duration {
                MediaDuration::Unknown {} => PushedDuration::Unknown,
                MediaDuration::Live {} => PushedDuration::Live,
                MediaDuration::Finite { duration_us } => PushedDuration::Finite(
                    std::time::Duration::from_micros(duration_us.as_micros()),
                ),
            });
        }
        PageSessionMessage::EndOfStream { .. } => {
            writer.end_of_stream().expect("end of stream");
        }
        _ => {}
    }
}
