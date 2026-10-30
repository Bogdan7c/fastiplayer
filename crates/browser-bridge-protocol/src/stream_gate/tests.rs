use super::*;
use crate::session::page::{
    PagePlaybackRequest, RangeFetchOutcome, ResourceSize, SeekCause, TabGoneReason,
};
use crate::stream::RangeRequestId;
use crate::text::MimeType;
use crate::time::MediaDuration;
use crate::time::MediaTime;

fn generation(number: u32) -> StreamGeneration {
    match StreamGeneration::new(number) {
        Ok(generation) => generation,
        Err(violation) => panic!("поколение {number}: {violation}"),
    }
}

fn source_buffer(index: u16) -> SourceBufferIndex {
    match SourceBufferIndex::new(index) {
        Ok(index) => index,
        Err(violation) => panic!("SourceBuffer {index}: {violation}"),
    }
}

fn started(number: u32) -> PageSessionMessage {
    PageSessionMessage::GenerationStarted {
        generation: generation(number),
    }
}

fn added(number: u32, index: u16) -> PageSessionMessage {
    let Ok(mime_type) = MimeType::new("video/mp4; codecs=\"avc1.64001f\"") else {
        panic!("тестовый MIME должен проходить проверку");
    };
    PageSessionMessage::SourceBufferAdded {
        generation: generation(number),
        source_buffer: source_buffer(index),
        mime_type,
    }
}

fn bytes(number: u32, index: u16, sequence: u64) -> SourceBufferBytesHeader {
    SourceBufferBytesHeader {
        generation: generation(number),
        source_buffer: source_buffer(index),
        sequence: AppendSequence(sequence),
    }
}

/// Сторож с объявленным поколением и SourceBuffer 0 и 1.
fn gate_with_generation(number: u32) -> StreamGate {
    let mut gate = StreamGate::new();
    for step in 1..=number {
        assert_eq!(gate.apply_message(&started(step)), Ok(GateDecision::Accept));
    }
    assert_eq!(
        gate.apply_message(&added(number, 0)),
        Ok(GateDecision::Accept)
    );
    assert_eq!(
        gate.apply_message(&added(number, 1)),
        Ok(GateDecision::Accept)
    );
    gate
}

#[test]
fn bytes_of_announced_source_buffers_are_accepted_in_sequence_per_buffer() {
    let mut gate = gate_with_generation(1);
    // Номера у каждого SourceBuffer свои, и рамки двух буферов перемежаются.
    for (index, sequence) in [(0, 0), (1, 0), (0, 1), (0, 2), (1, 1)] {
        assert_eq!(
            gate.admit_source_buffer_bytes(&bytes(1, index, sequence)),
            Ok(GateDecision::Accept),
            "SB {index} #{sequence}"
        );
    }
    assert_eq!(gate.current_generation(), Some(generation(1)));
}

#[test]
fn stale_generation_bytes_and_messages_are_dropped_without_touching_current_state() {
    let mut gate = gate_with_generation(1);
    assert_eq!(
        gate.admit_source_buffer_bytes(&bytes(1, 0, 0)),
        Ok(GateDecision::Accept)
    );
    assert_eq!(gate.apply_message(&started(2)), Ok(GateDecision::Accept));
    assert_eq!(gate.apply_message(&added(2, 0)), Ok(GateDecision::Accept));
    let before = gate.clone();

    // Хвост рекламы: рамка и сообщения поколения 1 после начала поколения 2.
    assert_eq!(
        gate.admit_source_buffer_bytes(&bytes(1, 0, 1)),
        Ok(GateDecision::DropStale)
    );
    assert_eq!(
        gate.apply_message(&added(1, 3)),
        Ok(GateDecision::DropStale)
    );
    assert_eq!(
        gate.apply_message(&PageSessionMessage::EndOfStream {
            generation: generation(1)
        }),
        Ok(GateDecision::DropStale)
    );
    assert_eq!(gate, before, "устаревшее не должно менять состояние");

    // Поколение 2 начинает свои номера с 0, независимо от поколения 1.
    assert_eq!(
        gate.admit_source_buffer_bytes(&bytes(2, 0, 0)),
        Ok(GateDecision::Accept)
    );
}

#[test]
fn generations_must_start_at_one_and_grow_by_exactly_one() {
    let mut gate = StreamGate::new();
    assert_eq!(
        gate.apply_message(&started(2)),
        Err(StreamViolation::GenerationOutOfOrder)
    );
    assert_eq!(gate.apply_message(&started(1)), Ok(GateDecision::Accept));
    assert_eq!(
        gate.apply_message(&started(1)),
        Err(StreamViolation::GenerationOutOfOrder),
        "повтор"
    );
    assert_eq!(
        gate.apply_message(&started(3)),
        Err(StreamViolation::GenerationOutOfOrder),
        "пропуск"
    );
    assert_eq!(gate.current_generation(), Some(generation(1)));
}

#[test]
fn anything_about_an_unannounced_generation_is_a_violation() {
    let mut gate = StreamGate::new();
    assert_eq!(
        gate.admit_source_buffer_bytes(&bytes(1, 0, 0)),
        Err(StreamViolation::GenerationNotAnnounced)
    );
    assert_eq!(
        gate.apply_message(&added(1, 0)),
        Err(StreamViolation::GenerationNotAnnounced)
    );
    let mut gate = gate_with_generation(1);
    assert_eq!(
        gate.admit_source_buffer_bytes(&bytes(2, 0, 0)),
        Err(StreamViolation::GenerationNotAnnounced),
        "рамка из будущего"
    );
}

#[test]
fn source_buffer_must_be_announced_once_before_its_bytes_and_messages() {
    let mut gate = gate_with_generation(1);
    assert_eq!(
        gate.admit_source_buffer_bytes(&bytes(1, 2, 0)),
        Err(StreamViolation::SourceBufferNotAnnounced)
    );
    assert_eq!(
        gate.apply_message(&PageSessionMessage::SourceBufferAborted {
            generation: generation(1),
            source_buffer: source_buffer(2),
        }),
        Err(StreamViolation::SourceBufferNotAnnounced)
    );
    assert_eq!(
        gate.apply_message(&added(1, 0)),
        Err(StreamViolation::SourceBufferAlreadyAnnounced)
    );
}

#[test]
fn lost_or_repeated_frame_is_reported_with_expected_and_received_numbers() {
    let mut gate = gate_with_generation(1);
    assert_eq!(
        gate.admit_source_buffer_bytes(&bytes(1, 0, 0)),
        Ok(GateDecision::Accept)
    );
    assert_eq!(
        gate.admit_source_buffer_bytes(&bytes(1, 0, 2)),
        Err(StreamViolation::SequenceGap {
            expected: AppendSequence(1),
            received: AppendSequence(2),
        })
    );
    assert_eq!(
        gate.admit_source_buffer_bytes(&bytes(1, 0, 0)),
        Err(StreamViolation::SequenceGap {
            expected: AppendSequence(1),
            received: AppendSequence(0),
        }),
        "повтор"
    );
    // Ошибка не сдвинула ожидание: правильная рамка всё ещё принимается.
    assert_eq!(
        gate.admit_source_buffer_bytes(&bytes(1, 0, 1)),
        Ok(GateDecision::Accept)
    );
}

#[test]
fn seek_and_other_unbound_messages_pass_even_before_any_generation() {
    let mut gate = StreamGate::new();
    let Ok(target_us) = MediaTime::from_micros(42_000_000) else {
        panic!("тестовое время должно проходить проверку");
    };
    let seek = PageSessionMessage::SeekStarted {
        target_us,
        cause: SeekCause::PageSeek {},
    };
    assert_eq!(gate.apply_message(&seek), Ok(GateDecision::Accept));
    assert_eq!(
        gate,
        StreamGate::new(),
        "перемотка не трогает номера потока"
    );
}

fn file_added(number: u32) -> PageSessionMessage {
    PageSessionMessage::FileSourceAdded {
        generation: generation(number),
    }
}

/// Сторож с объявленным поколением-файлом.
fn gate_with_file_generation(number: u32) -> StreamGate {
    let mut gate = StreamGate::new();
    for step in 1..=number {
        assert_eq!(gate.apply_message(&started(step)), Ok(GateDecision::Accept));
    }
    assert_eq!(
        gate.apply_message(&file_added(number)),
        Ok(GateDecision::Accept)
    );
    gate
}

fn duration_changed(number: u32) -> PageSessionMessage {
    let Ok(duration_us) = MediaTime::from_micros(1_000_000) else {
        panic!("тестовая длительность должна проходить проверку");
    };
    PageSessionMessage::DurationChanged {
        generation: generation(number),
        duration: MediaDuration::Finite { duration_us },
    }
}

#[test]
fn file_source_marks_the_generation_as_file_and_repeat_is_a_violation() {
    let mut gate = gate_with_file_generation(1);
    let after_first = gate.clone();
    assert_eq!(
        gate.apply_message(&file_added(1)),
        Err(StreamViolation::FileSourceAlreadyAnnounced)
    );
    assert_eq!(gate, after_first, "нарушение не меняет состояние");
}

#[test]
fn file_source_after_source_buffer_is_a_violation() {
    let mut gate = StreamGate::new();
    assert_eq!(gate.apply_message(&started(1)), Ok(GateDecision::Accept));
    assert_eq!(gate.apply_message(&added(1, 0)), Ok(GateDecision::Accept));
    let before = gate.clone();
    assert_eq!(
        gate.apply_message(&file_added(1)),
        Err(StreamViolation::FileSourceAfterSourceBuffer)
    );
    assert_eq!(gate, before);
    // MSE-поколение после отказа работает как раньше.
    assert_eq!(
        gate.admit_source_buffer_bytes(&bytes(1, 0, 0)),
        Ok(GateDecision::Accept)
    );
}

#[test]
fn source_buffer_in_file_generation_is_a_violation() {
    let mut gate = gate_with_file_generation(1);
    assert_eq!(
        gate.apply_message(&added(1, 0)),
        Err(StreamViolation::SourceBufferInFileGeneration)
    );
}

#[test]
fn media_source_messages_and_bytes_in_file_generation_are_violations() {
    let mut gate = gate_with_file_generation(1);
    let before = gate.clone();
    let Ok(offset_us) = crate::time::MediaTimeOffset::from_micros(5) else {
        panic!("тестовое смещение должно проходить проверку");
    };
    let mime_type = match MimeType::new("video/mp4") {
        Ok(mime_type) => mime_type,
        Err(violation) => panic!("MIME: {violation}"),
    };
    let mse_only = [
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
    ];
    for message in &mse_only {
        assert_eq!(
            gate.apply_message(message),
            Err(StreamViolation::MediaSourceMessageInFileGeneration),
            "{message:?}"
        );
    }
    assert_eq!(
        gate.admit_source_buffer_bytes(&bytes(1, 0, 0)),
        Err(StreamViolation::MediaSourceMessageInFileGeneration)
    );
    assert_eq!(gate, before);
}

#[test]
fn file_generation_still_allows_non_mse_messages() {
    let mut gate = gate_with_file_generation(1);
    let Ok(target_us) = MediaTime::from_micros(1_000_000) else {
        panic!("тестовое время должно проходить проверку");
    };
    let allowed = [
        PageSessionMessage::SeekStarted {
            target_us,
            cause: SeekCause::PageSeek {},
        },
        PageSessionMessage::PagePlayback {
            request: PagePlaybackRequest::Pause {},
        },
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
    // Следующее поколение может быть и MSE: содержимое решается заново.
    assert_eq!(gate.apply_message(&started(2)), Ok(GateDecision::Accept));
    assert_eq!(gate.apply_message(&added(2, 0)), Ok(GateDecision::Accept));
    assert_eq!(
        gate.admit_source_buffer_bytes(&bytes(2, 0, 0)),
        Ok(GateDecision::Accept)
    );
}

#[test]
fn file_source_follows_generation_rules_stale_and_unannounced() {
    let mut gate = StreamGate::new();
    assert_eq!(
        gate.apply_message(&file_added(1)),
        Err(StreamViolation::GenerationNotAnnounced)
    );
    let mut gate = gate_with_file_generation(1);
    assert_eq!(
        gate.apply_message(&file_added(2)),
        Err(StreamViolation::GenerationNotAnnounced),
        "из будущего"
    );
    assert_eq!(gate.apply_message(&started(2)), Ok(GateDecision::Accept));
    let before = gate.clone();
    assert_eq!(
        gate.apply_message(&file_added(1)),
        Ok(GateDecision::DropStale)
    );
    assert_eq!(gate, before, "устаревшее не меняет состояние");
    // Поколение 2 решается независимо от файла в поколении 1.
    assert_eq!(gate.apply_message(&file_added(2)), Ok(GateDecision::Accept));
}

#[test]
fn media_source_generation_rules_are_unchanged() {
    let mut gate = gate_with_generation(1);
    assert_eq!(
        gate.apply_message(&duration_changed(1)),
        Ok(GateDecision::Accept)
    );
    assert_eq!(
        gate.apply_message(&PageSessionMessage::EndOfStream {
            generation: generation(1)
        }),
        Ok(GateDecision::Accept)
    );
    // Недообъявленный SourceBuffer — по-прежнему SourceBufferNotAnnounced.
    assert_eq!(
        gate.apply_message(&PageSessionMessage::SourceBufferAborted {
            generation: generation(1),
            source_buffer: source_buffer(2),
        }),
        Err(StreamViolation::SourceBufferNotAnnounced)
    );
    // Поколение без объявлений: длительность допустима до решения о содержимом.
    let mut undecided = StreamGate::new();
    assert_eq!(
        undecided.apply_message(&started(1)),
        Ok(GateDecision::Accept)
    );
    assert_eq!(
        undecided.apply_message(&duration_changed(1)),
        Ok(GateDecision::Accept)
    );
}
