//! Тесты сверки рамок и итогов страницы (чистая логика, без потоков).

use std::time::{Duration, Instant};

use super::error::PushedFileError;
use super::ids::{RangeRequestId, RangeRequestIds};
use super::retry::RangeRetryPolicy;
use super::state::{FileState, ReadPlan, ResourceSize};
use super::writer::{RangeBytesVerdict, RangeFailure, RangeOutcome, ResourceSizeReport};

/// Предел запроса в тестах.
const MAX_REQUEST: u32 = 1024;
/// Пауза повтора в тестах.
const RETRY_PAUSE: Duration = Duration::from_millis(50);

/// Файл с прошлым: номера 1..=3 «уже выдавал прежний файл сеанса».
fn fresh_state(retry: RangeRetryPolicy) -> (FileState, RangeRequestIds) {
    let ids = RangeRequestIds::new();
    for _ in 0..3 {
        ids.issue().expect("previous file id");
    }
    (FileState::new(ids.next_unissued(), retry), ids)
}

fn retry_policy() -> RangeRetryPolicy {
    RangeRetryPolicy::with_budget(Duration::from_secs(10)).with_delays(RETRY_PAUSE, RETRY_PAUSE)
}

/// Файл с первым запросом `[0, length)`; возвращает его номер.
fn started(length: u32) -> (FileState, RangeRequestIds, RangeRequestId) {
    let (mut state, ids) = fresh_state(retry_policy());
    state.begin_initial_read(&ids, length).expect("initial");
    let id = state.take_requests()[0].id;
    (state, ids, id)
}

fn completed(size: ResourceSizeReport) -> RangeOutcome {
    RangeOutcome::Completed {
        resource_size: size,
    }
}

fn read_all(state: &mut FileState, position: u64, capacity: usize) -> Vec<u8> {
    let mut output = vec![0; capacity];
    let count = state.copy_available(position, &mut output);
    output.truncate(count);
    output
}

#[test]
fn first_request_starts_at_zero_with_the_initial_length() {
    let (mut state, ids) = fresh_state(retry_policy());
    state.begin_initial_read(&ids, 64).expect("initial");
    let requests = state.take_requests();
    assert_eq!(requests.len(), 1);
    assert_eq!((requests[0].offset, requests[0].length.get()), (0, 64));
    assert_eq!(requests[0].id.get(), 4, "numbers continue across files");
    assert!(state.take_requests().is_empty(), "a request is taken once");
}

#[test]
fn contiguous_frames_are_accepted_and_readable_in_order() {
    let (mut state, _ids, id) = started(10);
    assert_eq!(
        state.accept_bytes(id, 0, b"abc"),
        Ok(RangeBytesVerdict::Accepted)
    );
    assert_eq!(
        state.accept_bytes(id, 3, b"defghij"),
        Ok(RangeBytesVerdict::Accepted)
    );
    assert_eq!(read_all(&mut state, 0, 100), b"abcdefghij");
    assert!(state.failure().is_none());
}

#[test]
fn frame_with_a_gap_or_overlap_is_a_violation_and_the_first_error_wins() {
    for wrong_offset in [4_u64, 2] {
        let (mut state, _ids, id) = started(10);
        state.accept_bytes(id, 0, b"abc").expect("first frame");
        let error = state
            .accept_bytes(id, wrong_offset, b"x")
            .expect_err("violation");
        assert_eq!(
            error,
            PushedFileError::NonContiguousBytes {
                id: id.get(),
                expected: 3,
                actual: wrong_offset
            }
        );
        // Следующее нарушение другого рода не подменяет первую причину.
        assert_eq!(
            state.accept_bytes(id, 3, &[0; 100]),
            Err(error.clone()),
            "the first error is sticky"
        );
        assert_eq!(state.failure(), Some(&error));
    }
}

#[test]
fn bytes_past_the_end_of_the_request_are_a_violation() {
    let (mut state, _ids, id) = started(4);
    let error = state.accept_bytes(id, 0, b"12345").expect_err("too long");
    assert_eq!(
        error,
        PushedFileError::BytesBeyondRequest {
            id: id.get(),
            requested_end: 4
        }
    );
    assert!(read_all(&mut state, 0, 10).is_empty(), "nothing leaked in");
}

#[test]
fn a_number_nobody_issued_is_a_violation() {
    let (mut state, _ids, id) = started(4);
    let foreign = RangeRequestId::new(id.get() + 1);
    assert_eq!(
        state.accept_bytes(foreign, 0, b"x"),
        Err(PushedFileError::UnissuedRequest { id: foreign.get() })
    );
    let (mut state, _ids, id) = started(4);
    assert_eq!(
        state.finish_request(
            RangeRequestId::new(id.get() + 7),
            completed(ResourceSizeReport::Unknown),
            Instant::now()
        ),
        Err(PushedFileError::UnissuedRequest { id: id.get() + 7 })
    );
}

#[test]
fn numbers_of_a_previous_file_are_stale_not_an_attack() {
    let (mut state, _ids, _id) = started(4);
    for previous in 1..=3 {
        let stale = RangeRequestId::new(previous);
        assert_eq!(
            state.accept_bytes(stale, 999, b"late"),
            Ok(RangeBytesVerdict::StaleDropped)
        );
        assert_eq!(
            state.finish_request(
                stale,
                completed(ResourceSizeReport::Known(1)),
                Instant::now()
            ),
            Ok(())
        );
    }
    assert!(
        state.failure().is_none(),
        "late frames of a past file are harmless"
    );
    assert!(read_all(&mut state, 0, 10).is_empty());
}

#[test]
fn bytes_of_an_abandoned_request_are_dropped_unvalidated_and_never_buffered() {
    let (mut state, ids, first) = started(100);
    state.accept_bytes(first, 0, &[1; 10]).expect("first frame");
    // Перемотка: читатель встал на 500 — прежний запрос брошен, просим с 500.
    let plan = state
        .plan_read(&ids, 500, 50, MAX_REQUEST, Instant::now())
        .expect("plan");
    assert_eq!(plan, ReadPlan::RequestQueued);
    let second = state.take_requests()[0];
    assert_eq!((second.offset, second.length.get()), (500, 50));
    // Запоздавшая рамка брошенного запроса — даже с кривым смещением — не нарушение.
    assert_eq!(
        state.accept_bytes(first, 77, &[9; 5]),
        Ok(RangeBytesVerdict::StaleDropped)
    );
    assert!(state.failure().is_none());
    state
        .accept_bytes(second.id, 500, &[2; 50])
        .expect("second request");
    assert_eq!(read_all(&mut state, 500, 100), vec![2; 50]);
}

#[test]
fn a_request_dropped_before_it_was_sent_is_never_sent() {
    let (mut state, ids) = fresh_state(retry_policy());
    state.begin_initial_read(&ids, 64).expect("initial");
    // Перемотка до отправки: первый запрос заменён вторым.
    state
        .plan_read(&ids, 5000, 10, MAX_REQUEST, Instant::now())
        .expect("plan");
    let requests = state.take_requests();
    assert_eq!(requests.len(), 1, "only the live request is sent");
    assert_eq!(requests[0].offset, 5000);
}

#[test]
fn short_completion_is_honest_only_at_the_end_of_the_file() {
    // Запросили 10, файл 6 байт: недобор у конца файла.
    let (mut state, _ids, id) = started(10);
    state.accept_bytes(id, 0, b"abcdef").expect("bytes");
    state
        .finish_request(id, completed(ResourceSizeReport::Known(6)), Instant::now())
        .expect("honest end");
    assert_eq!(read_all(&mut state, 0, 100), b"abcdef");

    // Размер 20, а страница закончила на 6: недобор не у конца.
    let (mut state, _ids, id) = started(10);
    state.accept_bytes(id, 0, b"abcdef").expect("bytes");
    assert_eq!(
        state.finish_request(id, completed(ResourceSizeReport::Known(20)), Instant::now()),
        Err(PushedFileError::ShortRangeResponse {
            id: id.get(),
            ended_at: 6,
            requested_end: 10
        })
    );
}

#[test]
fn unknown_size_short_completion_marks_the_end_of_the_file() {
    let (mut state, ids, id) = started(10);
    state.accept_bytes(id, 0, b"abcdef").expect("bytes");
    state
        .finish_request(id, completed(ResourceSizeReport::Unknown), Instant::now())
        .expect("completed");
    assert_eq!(read_all(&mut state, 0, 100), b"abcdef");
    assert_eq!(
        state.plan_read(&ids, 6, 10, MAX_REQUEST, Instant::now()),
        Ok(ReadPlan::EndOfFile),
        "short answer without a size is the end"
    );
}

#[test]
fn full_unknown_size_answer_does_not_end_the_file() {
    let (mut state, ids, id) = started(4);
    state.accept_bytes(id, 0, b"abcd").expect("bytes");
    state
        .finish_request(id, completed(ResourceSizeReport::Unknown), Instant::now())
        .expect("completed");
    assert_eq!(read_all(&mut state, 0, 100), b"abcd");
    assert_eq!(
        state.plan_read(&ids, 4, 4, MAX_REQUEST, Instant::now()),
        Ok(ReadPlan::RequestQueued)
    );
    assert_eq!(state.take_requests()[0].offset, 4);
}

#[test]
fn resource_size_must_not_change_or_flip_between_known_and_unknown() {
    // Известный размер изменился.
    let (mut state, ids, id) = started(4);
    state.accept_bytes(id, 0, b"abcd").expect("bytes");
    state
        .finish_request(
            id,
            completed(ResourceSizeReport::Known(100)),
            Instant::now(),
        )
        .expect("first size");
    read_all(&mut state, 0, 10);
    state
        .plan_read(&ids, 4, 4, MAX_REQUEST, Instant::now())
        .expect("plan");
    let second = state.take_requests()[0].id;
    state.accept_bytes(second, 4, b"efgh").expect("bytes");
    assert_eq!(
        state.finish_request(
            second,
            completed(ResourceSizeReport::Known(101)),
            Instant::now()
        ),
        Err(PushedFileError::ResourceSizeChanged {
            before: 100,
            after: 101
        })
    );

    // Известный стал неизвестным и наоборот.
    for (first, second_report) in [
        (ResourceSizeReport::Known(100), ResourceSizeReport::Unknown),
        (ResourceSizeReport::Unknown, ResourceSizeReport::Known(100)),
    ] {
        let (mut state, ids, id) = started(4);
        state.accept_bytes(id, 0, b"abcd").expect("bytes");
        state
            .finish_request(id, completed(first), Instant::now())
            .expect("first");
        read_all(&mut state, 0, 10);
        state
            .plan_read(&ids, 4, 4, MAX_REQUEST, Instant::now())
            .expect("plan");
        let second = state.take_requests()[0].id;
        state.accept_bytes(second, 4, b"efgh").expect("bytes");
        assert_eq!(
            state.finish_request(second, completed(second_report), Instant::now()),
            Err(PushedFileError::ResourceSizeKnowledgeChanged)
        );
    }
}

#[test]
fn bytes_past_the_reported_resource_size_are_a_violation() {
    // Размер объявлен меньше, чем уже доставлено.
    let (mut state, _ids, id) = started(10);
    state.accept_bytes(id, 0, b"abcdefgh").expect("bytes");
    assert_eq!(
        state.finish_request(id, completed(ResourceSizeReport::Known(5)), Instant::now()),
        Err(PushedFileError::BytesBeyondResourceSize { size: 5, end: 8 })
    );

    // Размер уже известен, рамка выходит за него.
    let (mut state, ids, id) = started(4);
    state.accept_bytes(id, 0, b"abcd").expect("bytes");
    state
        .finish_request(id, completed(ResourceSizeReport::Known(6)), Instant::now())
        .expect("size");
    read_all(&mut state, 0, 10);
    state
        .plan_read(&ids, 4, 100, MAX_REQUEST, Instant::now())
        .expect("plan");
    let second = state.take_requests()[0];
    assert_eq!(
        second.length.get(),
        2,
        "the request is clamped to the file end"
    );
    assert_eq!(
        state.accept_bytes(second.id, 4, b"efg"),
        Err(PushedFileError::BytesBeyondRequest {
            id: second.id.get(),
            requested_end: 6
        })
    );
}

#[test]
fn bytes_or_finish_after_the_request_completed_are_a_violation() {
    let (mut state, _ids, id) = started(4);
    state.accept_bytes(id, 0, b"ab").expect("bytes");
    state
        .finish_request(id, completed(ResourceSizeReport::Known(2)), Instant::now())
        .expect("completed");
    assert_eq!(
        state.accept_bytes(id, 2, b"x"),
        Err(PushedFileError::RequestAlreadyFinished { id: id.get() })
    );
    let (mut state, _ids, id) = started(4);
    state
        .finish_request(id, completed(ResourceSizeReport::Unknown), Instant::now())
        .expect("1");
    assert_eq!(
        state.finish_request(id, completed(ResourceSizeReport::Unknown), Instant::now()),
        Err(PushedFileError::RequestAlreadyFinished { id: id.get() })
    );
}

mod failures_and_end;
