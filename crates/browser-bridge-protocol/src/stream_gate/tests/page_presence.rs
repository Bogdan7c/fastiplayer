//! Присутствие страницы у сторожа (протокол 1.4, решения 240–241): уход, возврат, отказ.

use super::*;
use crate::session::page_reopen::PageReopenFailure;
use crate::version::ProtocolVersion;

fn page_left() -> PageSessionMessage {
    PageSessionMessage::PageLeft {
        reason: TabGoneReason::Closed,
    }
}

fn page_returned() -> PageSessionMessage {
    PageSessionMessage::PageReturned {}
}

fn page_reopen_failed() -> PageSessionMessage {
    PageSessionMessage::PageReopenFailed {
        reason: PageReopenFailure::NoBrowserWindow,
    }
}

fn media_time(micros: u64) -> MediaTime {
    match MediaTime::from_micros(micros) {
        Ok(time) => time,
        Err(violation) => panic!("время {micros}: {violation}"),
    }
}

/// Сторож 1.4 с поколением 1 и SourceBuffer 0/1, где страница ушла.
fn gate_with_page_left() -> StreamGate {
    let mut gate = gate_with_generation(1);
    assert_eq!(gate.apply_message(&page_left()), Ok(GateDecision::Accept));
    gate
}

/// То же, но страница уже вернулась.
fn gate_with_page_returned() -> StreamGate {
    let mut gate = gate_with_page_left();
    assert_eq!(
        gate.apply_message(&page_returned()),
        Ok(GateDecision::Accept)
    );
    gate
}

/// Сторож, где вернуть страницу не вышло.
fn gate_with_reopen_failed() -> StreamGate {
    let mut gate = gate_with_page_left();
    assert_eq!(
        gate.apply_message(&page_reopen_failed()),
        Ok(GateDecision::Accept)
    );
    gate
}

#[test]
fn page_left_is_accepted_and_the_new_messages_are_unbound_from_the_stream() {
    // Без единого поколения тоже можно уйти: страница могла закрыться до `generation_started`.
    let mut gate = StreamGate::new();
    assert_eq!(gate.apply_message(&page_left()), Ok(GateDecision::Accept));
    assert_eq!(gate.current_generation(), None);
    assert_eq!(
        gate.apply_message(&page_returned()),
        Ok(GateDecision::Accept)
    );
    // Первое поколение после возврата — по-прежнему 1.
    assert_eq!(gate.apply_message(&started(1)), Ok(GateDecision::Accept));
    assert_eq!(gate.current_generation(), Some(generation(1)));
}

#[test]
fn page_returned_or_reopen_failed_while_the_page_is_present_is_a_violation() {
    for message in [page_returned(), page_reopen_failed()] {
        let mut gate = gate_with_generation(1);
        let before = gate.clone();
        assert_eq!(
            gate.apply_message(&message),
            Err(StreamViolation::PageIsPresent),
            "{message:?}"
        );
        assert_eq!(gate, before, "отказ не меняет сторожа");
    }
}

#[test]
fn page_left_twice_is_a_violation_because_the_page_is_already_away() {
    let mut gate = gate_with_page_left();
    assert_eq!(
        gate.apply_message(&page_left()),
        Err(StreamViolation::PageIsAway)
    );
}

#[test]
fn while_the_page_is_away_only_return_failure_and_tab_gone_pass() {
    let mut gate = gate_with_page_left();
    let before = gate.clone();
    // tab_gone: экстеншен сдался — принят, состояние не меняется (сервер кончит сеанс сам).
    assert_eq!(
        gate.apply_message(&PageSessionMessage::TabGone {
            reason: TabGoneReason::NavigatedAway
        }),
        Ok(GateDecision::Accept)
    );
    assert_eq!(gate, before);
    // Всё остальное — нарушение: и новое поколение, и сообщение про старое, и не про поток.
    for message in [
        started(2),
        added(1, 0),
        duration_changed(1),
        PageSessionMessage::SeekStarted {
            target_us: media_time(42_000_000),
            cause: SeekCause::PageSeek {},
        },
    ] {
        assert_eq!(
            gate.apply_message(&message),
            Err(StreamViolation::PageIsAway),
            "{message:?}"
        );
    }
    assert_eq!(gate, before, "отказы не меняют сторожа");
}

#[test]
fn bytes_are_refused_while_the_page_is_away_even_for_the_old_generation() {
    let mut gate = gate_with_page_left();
    let before = gate.clone();
    assert_eq!(
        gate.admit_source_buffer_bytes(&bytes(1, 0, 0)),
        Err(StreamViolation::PageIsAway)
    );
    assert_eq!(
        gate.admit_source_buffer_bytes(&bytes(7, 0, 0)),
        Err(StreamViolation::PageIsAway)
    );
    assert_eq!(gate, before);
}

#[test]
fn range_bytes_pass_only_while_the_page_is_present() {
    let mut gate = gate_with_generation(1);
    assert_eq!(gate.admit_range_bytes(), Ok(GateDecision::Accept));
    assert_eq!(gate.apply_message(&page_left()), Ok(GateDecision::Accept));
    assert_eq!(gate.admit_range_bytes(), Err(StreamViolation::PageIsAway));
    assert_eq!(
        gate.apply_message(&page_returned()),
        Ok(GateDecision::Accept)
    );
    assert_eq!(gate.admit_range_bytes(), Ok(GateDecision::Accept));
    let failed = gate_with_reopen_failed();
    assert_eq!(failed.admit_range_bytes(), Err(StreamViolation::PageIsAway));
    // Проверка рамок вида 2 состояние не меняет.
    let before = failed.clone();
    assert_eq!(failed.admit_range_bytes(), Err(StreamViolation::PageIsAway));
    assert_eq!(failed, before);
}

#[test]
fn the_generation_that_left_with_the_page_stays_stale_after_the_return() {
    let mut gate = gate_with_page_returned();
    // Байты и сообщения поколения 1 — устаревшие, хотя оно ещё «текущее» по номеру.
    assert_eq!(
        gate.admit_source_buffer_bytes(&bytes(1, 0, 0)),
        Ok(GateDecision::DropStale)
    );
    assert_eq!(
        gate.apply_message(&duration_changed(1)),
        Ok(GateDecision::DropStale)
    );
    assert_eq!(
        gate.apply_message(&added(1, 2)),
        Ok(GateDecision::DropStale)
    );
    // Счётчик рамок старого поколения не двигался: DropStale ничего не меняет.
    let before = gate.clone();
    let _ = gate.admit_source_buffer_bytes(&bytes(1, 0, 0));
    assert_eq!(gate, before);
}

#[test]
fn after_the_return_the_next_generation_is_current_plus_one_and_works_normally() {
    let mut gate = gate_with_page_returned();
    // Через одно поколение — по-прежнему нарушение порядка.
    assert_eq!(
        gate.apply_message(&started(3)),
        Err(StreamViolation::GenerationOutOfOrder)
    );
    assert_eq!(gate.apply_message(&started(2)), Ok(GateDecision::Accept));
    assert_eq!(gate.apply_message(&added(2, 0)), Ok(GateDecision::Accept));
    assert_eq!(
        gate.admit_source_buffer_bytes(&bytes(2, 0, 0)),
        Ok(GateDecision::Accept)
    );
    // Старое поколение остаётся устаревшим и теперь по обычному правилу.
    assert_eq!(
        gate.admit_source_buffer_bytes(&bytes(1, 0, 0)),
        Ok(GateDecision::DropStale)
    );
}

#[test]
fn a_second_departure_retires_the_second_generation_too() {
    let mut gate = gate_with_page_returned();
    assert_eq!(gate.apply_message(&started(2)), Ok(GateDecision::Accept));
    assert_eq!(gate.apply_message(&added(2, 0)), Ok(GateDecision::Accept));
    assert_eq!(gate.apply_message(&page_left()), Ok(GateDecision::Accept));
    assert_eq!(
        gate.apply_message(&page_returned()),
        Ok(GateDecision::Accept)
    );
    assert_eq!(
        gate.admit_source_buffer_bytes(&bytes(2, 0, 0)),
        Ok(GateDecision::DropStale)
    );
    assert_eq!(gate.apply_message(&started(3)), Ok(GateDecision::Accept));
}

#[test]
fn after_reopen_failed_every_message_and_every_byte_frame_is_refused() {
    let mut gate = gate_with_reopen_failed();
    let before = gate.clone();
    for message in [
        page_left(),
        page_returned(),
        page_reopen_failed(),
        PageSessionMessage::TabGone {
            reason: TabGoneReason::Closed,
        },
        started(2),
        duration_changed(1),
    ] {
        assert_eq!(
            gate.apply_message(&message),
            Err(StreamViolation::PageIsAway),
            "{message:?}"
        );
    }
    assert_eq!(
        gate.admit_source_buffer_bytes(&bytes(1, 0, 0)),
        Err(StreamViolation::PageIsAway)
    );
    assert_eq!(gate, before);
}

#[test]
fn the_new_messages_are_not_part_of_an_agreed_1_3_session() {
    // «Договорились — значит строго»: экстеншен 1.3 шлёт `tab_gone`, а не `page_left`.
    for message in [page_left(), page_returned(), page_reopen_failed()] {
        let mut gate = StreamGate::for_agreed_version(ProtocolVersion::V1_3);
        assert_eq!(gate.apply_message(&started(1)), Ok(GateDecision::Accept));
        let before = gate.clone();
        assert_eq!(
            gate.apply_message(&message),
            Err(StreamViolation::MessageNotInAgreedVersion {
                agreed: ProtocolVersion::V1_3,
                introduced_in: ProtocolVersion::V1_4,
            }),
            "{message:?}"
        );
        assert_eq!(gate, before);
    }
}
