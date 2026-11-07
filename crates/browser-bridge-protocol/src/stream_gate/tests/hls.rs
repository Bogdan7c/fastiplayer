//! HLS-поколение (протокол 1.1) и версия сеанса у сторожа.

use super::*;
use crate::resource::{ResourceFetchFailure, ResourceFetchOutcome, ResourceHandle};
use crate::version::ProtocolVersion;

fn hls_added(number: u32) -> PageSessionMessage {
    PageSessionMessage::HlsSourceAdded {
        generation: generation(number),
        root: ResourceHandle(0),
    }
}

fn resource_finished() -> PageSessionMessage {
    PageSessionMessage::ResourceFetchFinished {
        request_id: RangeRequestId(3),
        outcome: ResourceFetchOutcome::Failed {
            failure: ResourceFetchFailure::Refused {},
        },
    }
}

/// Сторож с объявленным HLS-поколением на текущей версии crate (1.1).
fn gate_with_hls_generation(number: u32) -> StreamGate {
    let mut gate = StreamGate::new();
    for step in 1..=number {
        assert_eq!(gate.apply_message(&started(step)), Ok(GateDecision::Accept));
    }
    assert_eq!(
        gate.apply_message(&hls_added(number)),
        Ok(GateDecision::Accept)
    );
    gate
}

/// Все MSE-сообщения про поколение 1 и SourceBuffer 0.
fn media_source_only_messages() -> Vec<PageSessionMessage> {
    let Ok(offset_us) = crate::time::MediaTimeOffset::from_micros(5) else {
        panic!("тестовое смещение должно проходить проверку");
    };
    let mime_type = match MimeType::new("video/mp4") {
        Ok(mime_type) => mime_type,
        Err(violation) => panic!("MIME: {violation}"),
    };
    vec![
        duration_changed(1),
        PageSessionMessage::EndOfStream {
            generation: generation(1),
        },
        PageSessionMessage::SourceBufferAborted {
            generation: generation(1),
            source_buffer: source_buffer(0),
        },
        PageSessionMessage::SourceBufferTypeChanged {
            generation: generation(1),
            source_buffer: source_buffer(0),
            mime_type,
        },
        PageSessionMessage::TimestampOffsetChanged {
            generation: generation(1),
            source_buffer: source_buffer(0),
            offset_us,
        },
    ]
}

#[test]
fn hls_source_marks_the_generation_as_hls_and_repeat_is_a_violation() {
    let mut gate = gate_with_hls_generation(1);
    let after_first = gate.clone();
    assert_eq!(
        gate.apply_message(&hls_added(1)),
        Err(StreamViolation::HlsSourceAlreadyAnnounced)
    );
    assert_eq!(gate, after_first, "нарушение не меняет состояние");
    assert_eq!(gate.current_generation(), Some(generation(1)));
}

#[test]
fn hls_source_is_only_accepted_in_an_undecided_generation() {
    // После SourceBuffer: поколение уже MSE.
    let mut gate = StreamGate::new();
    assert_eq!(gate.apply_message(&started(1)), Ok(GateDecision::Accept));
    assert_eq!(gate.apply_message(&added(1, 0)), Ok(GateDecision::Accept));
    let before = gate.clone();
    assert_eq!(
        gate.apply_message(&hls_added(1)),
        Err(StreamViolation::HlsSourceAfterSourceBuffer)
    );
    assert_eq!(gate, before);
    // MSE-поколение после отказа работает как раньше.
    assert_eq!(
        gate.admit_source_buffer_bytes(&bytes(1, 0, 0)),
        Ok(GateDecision::Accept)
    );

    // После файла: поколение уже файл.
    let mut gate = gate_with_file_generation(1);
    let before = gate.clone();
    assert_eq!(
        gate.apply_message(&hls_added(1)),
        Err(StreamViolation::HlsSourceInFileGeneration)
    );
    assert_eq!(gate, before);
}

#[test]
fn file_source_and_source_buffers_in_an_hls_generation_are_violations() {
    let mut gate = gate_with_hls_generation(1);
    let before = gate.clone();
    assert_eq!(
        gate.apply_message(&file_added(1)),
        Err(StreamViolation::FileSourceInHlsGeneration)
    );
    assert_eq!(
        gate.apply_message(&added(1, 0)),
        Err(StreamViolation::SourceBufferInHlsGeneration)
    );
    assert_eq!(gate, before, "отказы не меняют вид поколения");
}

#[test]
fn media_source_messages_and_bytes_in_an_hls_generation_are_violations() {
    let mut gate = gate_with_hls_generation(1);
    let before = gate.clone();
    for message in &media_source_only_messages() {
        assert_eq!(
            gate.apply_message(message),
            Err(StreamViolation::MediaSourceMessageInHlsGeneration),
            "{message:?}"
        );
    }
    assert_eq!(
        gate.admit_source_buffer_bytes(&bytes(1, 0, 0)),
        Err(StreamViolation::MediaSourceMessageInHlsGeneration)
    );
    assert_eq!(gate, before);
}

#[test]
fn hls_generation_allows_resource_results_and_other_non_mse_messages() {
    let mut gate = gate_with_hls_generation(1);
    let Ok(target_us) = MediaTime::from_micros(1_000_000) else {
        panic!("тестовое время должно проходить проверку");
    };
    let allowed = [
        resource_finished(),
        PageSessionMessage::SeekStarted {
            target_us,
            cause: SeekCause::PageSeek {},
        },
        PageSessionMessage::PagePlayback {
            request: PagePlaybackRequest::Play {},
        },
        // Хвост запроса файла прошлого поколения: номера общие, решает плеер.
        PageSessionMessage::RangeFetchFinished {
            request_id: RangeRequestId(1),
            outcome: RangeFetchOutcome::Completed {
                resource_size: ResourceSize::Unknown {},
            },
        },
        PageSessionMessage::TabGone {
            reason: TabGoneReason::Closed,
        },
    ];
    for message in &allowed {
        assert_eq!(
            gate.apply_message(message),
            Ok(GateDecision::Accept),
            "{message:?}"
        );
    }
    // Следующее поколение решается заново: может быть и MSE.
    assert_eq!(gate.apply_message(&started(2)), Ok(GateDecision::Accept));
    assert_eq!(gate.apply_message(&added(2, 0)), Ok(GateDecision::Accept));
    assert_eq!(
        gate.admit_source_buffer_bytes(&bytes(2, 0, 0)),
        Ok(GateDecision::Accept)
    );
}

#[test]
fn hls_source_follows_generation_rules_stale_and_unannounced() {
    let mut gate = StreamGate::new();
    assert_eq!(
        gate.apply_message(&hls_added(1)),
        Err(StreamViolation::GenerationNotAnnounced)
    );
    let mut gate = gate_with_hls_generation(1);
    assert_eq!(
        gate.apply_message(&hls_added(2)),
        Err(StreamViolation::GenerationNotAnnounced),
        "из будущего"
    );
    assert_eq!(gate.apply_message(&started(2)), Ok(GateDecision::Accept));
    let before = gate.clone();
    assert_eq!(
        gate.apply_message(&hls_added(1)),
        Ok(GateDecision::DropStale)
    );
    assert_eq!(gate, before, "устаревшее не меняет состояние");
    assert_eq!(gate.apply_message(&hls_added(2)), Ok(GateDecision::Accept));
}

#[test]
fn messages_of_protocol_1_1_are_violations_when_the_session_agreed_on_1_0() {
    let mut gate = StreamGate::for_agreed_version(ProtocolVersion::V1_0);
    assert_eq!(gate.agreed_version(), ProtocolVersion::V1_0);
    assert_eq!(gate.apply_message(&started(1)), Ok(GateDecision::Accept));
    let before = gate.clone();
    let expected = Err(StreamViolation::MessageNotInAgreedVersion {
        agreed: ProtocolVersion::V1_0,
        introduced_in: ProtocolVersion::V1_1,
    });
    assert_eq!(gate.apply_message(&hls_added(1)), expected);
    assert_eq!(gate.apply_message(&resource_finished()), expected);
    assert_eq!(gate, before, "отвергнутое не решает вид поколения");
    // Версия проверяется раньше «устарело»: сообщения не из договора нет вовсе.
    assert_eq!(gate.apply_message(&started(2)), Ok(GateDecision::Accept));
    assert_eq!(gate.apply_message(&hls_added(1)), expected);
}

#[test]
fn protocol_1_0_session_keeps_every_earlier_rule() {
    // MSE и файл на 1.0 работают как до версии 1.1.
    let mut gate = StreamGate::for_agreed_version(ProtocolVersion::V1_0);
    assert_eq!(gate.apply_message(&started(1)), Ok(GateDecision::Accept));
    assert_eq!(gate.apply_message(&added(1, 0)), Ok(GateDecision::Accept));
    assert_eq!(
        gate.admit_source_buffer_bytes(&bytes(1, 0, 0)),
        Ok(GateDecision::Accept)
    );
    assert_eq!(gate.apply_message(&started(2)), Ok(GateDecision::Accept));
    assert_eq!(gate.apply_message(&file_added(2)), Ok(GateDecision::Accept));
    assert_eq!(
        gate.apply_message(&added(2, 0)),
        Err(StreamViolation::SourceBufferInFileGeneration)
    );
}

#[test]
fn a_1_1_session_accepts_hls_and_resource_results() {
    let mut gate = StreamGate::for_agreed_version(ProtocolVersion::V1_1);
    assert_eq!(gate.apply_message(&started(1)), Ok(GateDecision::Accept));
    assert_eq!(gate.apply_message(&hls_added(1)), Ok(GateDecision::Accept));
    assert_eq!(
        gate.apply_message(&resource_finished()),
        Ok(GateDecision::Accept)
    );
    // Сторож по умолчанию говорит на версии crate.
    assert_eq!(StreamGate::new().agreed_version(), ProtocolVersion::V1_1);
}
