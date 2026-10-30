//! Тесты состояния файла: сбои сети и повторы, итоги запросов у конца файла, `seal`,
//! закрытие читателя и память. Общие помощники и проверки порядка рамок — в
//! родительском модуле.

use super::*;

#[test]
fn network_failure_retries_the_missing_range_with_a_new_request_after_the_pause() {
    let (mut state, ids, id) = started(100);
    state.accept_bytes(id, 0, &[7; 30]).expect("bytes");
    let now = Instant::now();
    state
        .finish_request(id, RangeOutcome::Failed(RangeFailure::Network), now)
        .expect("retry scheduled");
    // До паузы повтор не уходит. Прочитанные байты ещё лежат в буфере (читатель их не
    // забирал): повтор обязан просить только недостающее, а не весь запрос заново.
    let early = state.plan_read(&ids, 0, 70, MAX_REQUEST, now);
    assert!(matches!(early, Ok(ReadPlan::Wait(Some(left))) if left <= RETRY_PAUSE));
    assert!(state.take_requests().is_empty());
    // После паузы — новый запрос с первого недостающего байта на остаток.
    let later = state.plan_read(&ids, 0, 70, MAX_REQUEST, now + RETRY_PAUSE);
    assert_eq!(later, Ok(ReadPlan::RequestQueued));
    assert_eq!(
        read_all(&mut state, 0, 100),
        vec![7; 30],
        "bytes before the failure are kept"
    );
    let retry = state.take_requests()[0];
    assert_eq!((retry.offset, retry.length.get()), (30, 70));
    assert_ne!(retry.id, id);
    // Запоздалая рамка старого запроса — устаревшая, новая принимается.
    assert_eq!(
        state.accept_bytes(id, 30, &[0; 5]),
        Ok(RangeBytesVerdict::StaleDropped)
    );
    state
        .accept_bytes(retry.id, 30, &[8; 70])
        .expect("retry bytes");
    assert_eq!(read_all(&mut state, 30, 100), vec![8; 70]);
}

#[test]
fn retry_budget_exhaustion_is_the_final_unavailable_error() {
    let (mut state, ids) = fresh_state(RangeRetryPolicy::disabled());
    state.begin_initial_read(&ids, 10).expect("initial");
    let id = state.take_requests()[0].id;
    let error = state
        .finish_request(id, RangeOutcome::Failed(RangeFailure::Http), Instant::now())
        .expect_err("no budget");
    assert_eq!(error, PushedFileError::RetryBudgetExhausted { attempts: 1 });
}

#[test]
fn resource_changed_is_final() {
    let (mut state, _ids, id) = started(10);
    assert_eq!(
        state.finish_request(
            id,
            RangeOutcome::Failed(RangeFailure::ResourceChanged),
            Instant::now()
        ),
        Err(PushedFileError::ResourceChanged)
    );
}

#[test]
fn out_of_range_past_the_known_end_is_the_end_and_inside_the_file_is_an_error() {
    // Размер 4 известен; запрос за концом → «конец», не ошибка.
    let (mut state, ids, id) = started(4);
    state.accept_bytes(id, 0, b"abcd").expect("bytes");
    state
        .finish_request(id, completed(ResourceSizeReport::Known(4)), Instant::now())
        .expect("size");
    read_all(&mut state, 0, 10);
    // Читатель ушёл за конец (перемотка): план — конец файла, запросов нет.
    assert_eq!(
        state.plan_read(&ids, 9, 4, MAX_REQUEST, Instant::now()),
        Ok(ReadPlan::EndOfFile)
    );
    // Размер 100, запрос за 50 отвергнут как «вне файла» — это ошибка.
    let (mut state, ids, id) = started(4);
    state.accept_bytes(id, 0, b"abcd").expect("bytes");
    state
        .finish_request(
            id,
            completed(ResourceSizeReport::Known(100)),
            Instant::now(),
        )
        .expect("size");
    read_all(&mut state, 0, 10);
    state
        .plan_read(&ids, 50, 10, MAX_REQUEST, Instant::now())
        .expect("plan");
    let request = state.take_requests()[0].id;
    assert_eq!(
        state.finish_request(
            request,
            RangeOutcome::Failed(RangeFailure::OutOfRange),
            Instant::now()
        ),
        Err(PushedFileError::RangeRejected { offset: 50 })
    );
    // Первый запрос с неизвестным размером: «вне файла» — ошибка.
    let (mut state, _ids, id) = started(4);
    assert_eq!(
        state.finish_request(
            id,
            RangeOutcome::Failed(RangeFailure::OutOfRange),
            Instant::now()
        ),
        Err(PushedFileError::RangeRejected { offset: 0 })
    );
}

#[test]
fn seal_keeps_received_bytes_readable_then_reports_the_end_and_refuses_new_requests() {
    let (mut state, ids, id) = started(100);
    state.accept_bytes(id, 0, &[5; 40]).expect("bytes");
    state.seal();
    assert_eq!(
        read_all(&mut state, 0, 100),
        vec![5; 40],
        "what arrived still plays"
    );
    assert_eq!(
        state.plan_read(&ids, 40, 10, MAX_REQUEST, Instant::now()),
        Ok(ReadPlan::EndOfFile)
    );
    assert_eq!(
        state.plan_read(&ids, 7000, 10, MAX_REQUEST, Instant::now()),
        Ok(ReadPlan::EndOfFile),
        "reading outside of what arrived is the end of the file"
    );
    assert!(
        state.take_requests().is_empty(),
        "no new requests after seal"
    );
    assert_eq!(
        state.accept_bytes(id, 40, b"x"),
        Err(PushedFileError::WriterSealed)
    );
    assert!(state.failure().is_none(), "seal is not a failure");
}

#[test]
fn closed_reader_releases_the_buffer_and_makes_every_frame_stale() {
    let (mut state, _ids, id) = started(100);
    state.accept_bytes(id, 0, &[1; 10]).expect("bytes");
    state.close_reader();
    assert_eq!(
        state.accept_bytes(id, 10, b"x"),
        Ok(RangeBytesVerdict::StaleDropped)
    );
    assert!(
        read_all(&mut state, 0, 100).is_empty(),
        "the buffer was released"
    );
}

#[test]
fn the_reader_gets_bytes_exactly_once_and_memory_does_not_grow_with_the_file() {
    // Запрос на 1000 байт принимается по 1, а читается по 7: ничего не теряется и не
    // повторяется.
    let (mut state, _ids, id) = started(1000);
    let mut read = Vec::new();
    let mut position = 0_u64;
    for index in 0..1000_u64 {
        state
            .accept_bytes(id, index, &[u8::try_from(index % 251).expect("fits")])
            .expect("frame");
        if index % 7 == 6 {
            let chunk = read_all(&mut state, position, 7);
            position += chunk.len() as u64;
            read.extend(chunk);
        }
    }
    let rest = read_all(&mut state, position, 1000);
    read.extend(rest);
    let expected: Vec<u8> = (0..1000_u64)
        .map(|index| u8::try_from(index % 251).expect("fits"))
        .collect();
    assert_eq!(read, expected);
}

#[test]
fn size_knowledge_is_recorded_from_the_first_completion() {
    let (mut state, _ids, id) = started(4);
    state.accept_bytes(id, 0, b"abcd").expect("bytes");
    state
        .finish_request(id, completed(ResourceSizeReport::Known(4)), Instant::now())
        .expect("completed");
    // Через публичный шаг открытия.
    assert_eq!(
        state.step_initial(&RangeRequestIds::new(), Instant::now()),
        Ok(super::super::state::InitialStep::Completed(
            ResourceSize::Known(4)
        ))
    );
}

#[test]
fn exactly_one_mebibyte_frames_are_accepted_back_to_back() {
    const MEBIBYTE: usize = 1024 * 1024;
    let (mut state, ids) = fresh_state(retry_policy());
    state
        .begin_initial_read(&ids, 4 * MEBIBYTE as u32)
        .expect("initial");
    let id = state.take_requests()[0].id;
    let frame = vec![9_u8; MEBIBYTE];
    for index in 0..3_u64 {
        assert_eq!(
            state.accept_bytes(id, index * MEBIBYTE as u64, &frame),
            Ok(RangeBytesVerdict::Accepted)
        );
    }
    let mut output = vec![0_u8; 3 * MEBIBYTE];
    assert_eq!(state.copy_available(0, &mut output), 3 * MEBIBYTE);
    assert!(output.iter().all(|byte| *byte == 9));
}

#[test]
fn an_empty_frame_changes_nothing() {
    let (mut state, _ids, id) = started(10);
    assert_eq!(
        state.accept_bytes(id, 0, b""),
        Ok(RangeBytesVerdict::Accepted)
    );
    state
        .accept_bytes(id, 0, b"ab")
        .expect("next frame still starts at zero");
    assert_eq!(read_all(&mut state, 0, 10), b"ab");
}

#[test]
fn a_previous_files_number_is_stale_even_before_this_file_issued_anything() {
    let (mut state, ids) = fresh_state(retry_policy());
    assert_eq!(
        state.accept_bytes(RangeRequestId::new(2), 0, b"late"),
        Ok(RangeBytesVerdict::StaleDropped)
    );
    // А номер, который этот файл ещё не выдал, — нарушение.
    let next = RangeRequestId::new(u32::try_from(ids.next_unissued()).expect("fits"));
    assert_eq!(
        state.accept_bytes(next, 0, b"x"),
        Err(PushedFileError::UnissuedRequest { id: next.get() })
    );
}

#[test]
fn progress_between_failures_restarts_the_retry_budget() {
    // Бюджет 1 с; сбой, байты, и снова сбой через 2 с: это новая серия, а не конец.
    let policy = RangeRetryPolicy::with_budget(Duration::from_secs(1))
        .with_delays(Duration::from_millis(10), Duration::from_millis(10));
    let (mut state, ids) = fresh_state(policy);
    state.begin_initial_read(&ids, 100).expect("initial");
    let first = state.take_requests()[0].id;
    let start = Instant::now();
    state
        .finish_request(first, RangeOutcome::Failed(RangeFailure::Network), start)
        .expect("first failure is retried");
    state
        .plan_read(&ids, 0, 100, MAX_REQUEST, start + Duration::from_millis(10))
        .expect("plan");
    let retry = state.take_requests()[0].id;
    state.accept_bytes(retry, 0, &[1; 10]).expect("progress");
    assert_eq!(
        state.finish_request(
            retry,
            RangeOutcome::Failed(RangeFailure::Network),
            start + Duration::from_secs(2)
        ),
        Ok(()),
        "after progress the budget starts over"
    );
}

#[test]
fn only_a_read_cut_short_by_seal_is_recorded_not_a_seal_at_the_real_end() {
    // Файл целиком получен (размер известен), потом seal: чтение у настоящего конца
    // — не обрыв.
    let (mut state, ids, id) = started(4);
    state.accept_bytes(id, 0, b"abcd").expect("bytes");
    state
        .finish_request(id, completed(ResourceSizeReport::Known(4)), Instant::now())
        .expect("completed");
    read_all(&mut state, 0, 10);
    state.seal();
    assert!(!state.read_cut_by_seal(), "seal alone records nothing");
    assert_eq!(
        state.plan_read(&ids, 4, 4, MAX_REQUEST, Instant::now()),
        Ok(ReadPlan::EndOfFile)
    );
    assert!(
        !state.read_cut_by_seal(),
        "the real end of the file is not a cut"
    );

    // Файл длиннее полученного: чтение за полученным оборвано.
    let (mut state, ids, id) = started(100);
    state.accept_bytes(id, 0, &[1; 10]).expect("bytes");
    state.seal();
    read_all(&mut state, 0, 10);
    assert_eq!(
        state.plan_read(&ids, 10, 4, MAX_REQUEST, Instant::now()),
        Ok(ReadPlan::EndOfFile)
    );
    assert!(state.read_cut_by_seal());
}

#[test]
fn failure_after_the_last_byte_with_a_known_size_counts_as_completed() {
    let (mut state, ids, id) = started(4);
    state.accept_bytes(id, 0, b"abcd").expect("bytes");
    state
        .finish_request(
            id,
            completed(ResourceSizeReport::Known(100)),
            Instant::now(),
        )
        .expect("size is known now");
    read_all(&mut state, 0, 10);
    state
        .plan_read(&ids, 4, 4, MAX_REQUEST, Instant::now())
        .expect("plan");
    let second = state.take_requests()[0].id;
    state
        .accept_bytes(second, 4, b"efgh")
        .expect("all bytes of the second request");
    // Связь оборвалась после последнего байта, до итога: запрос считается завершённым.
    assert_eq!(
        state.finish_request(
            second,
            RangeOutcome::Failed(RangeFailure::Network),
            Instant::now()
        ),
        Ok(())
    );
    assert!(state.failure().is_none());
    assert_eq!(read_all(&mut state, 4, 10), b"efgh");
    assert_eq!(
        state.plan_read(&ids, 8, 4, MAX_REQUEST, Instant::now()),
        Ok(ReadPlan::RequestQueued),
        "no retry of the delivered range, reading simply continues"
    );
    assert_eq!(state.take_requests()[0].offset, 8);
}

#[test]
fn failure_after_the_last_byte_with_an_unknown_size_asks_for_the_continuation() {
    let (mut state, ids, id) = started(4);
    state
        .accept_bytes(id, 0, b"abcd")
        .expect("all bytes of the first request");
    let now = Instant::now();
    state
        .finish_request(id, RangeOutcome::Failed(RangeFailure::Network), now)
        .expect("normal retry path, not a violation");
    assert!(state.failure().is_none());
    let queued = state.plan_read(&ids, 0, 4, MAX_REQUEST, now + RETRY_PAUSE);
    // Читатель ещё не забрал байты — они отдаются первыми; план нужен лишь после них.
    assert!(matches!(queued, Ok(ReadPlan::RequestQueued)));
    let retry = state.take_requests()[0];
    assert_eq!(
        (retry.offset, retry.length.get()),
        (4, 4),
        "a continuation, not an empty range"
    );
    state
        .accept_bytes(retry.id, 4, b"efgh")
        .expect("continuation");
    state
        .finish_request(
            retry.id,
            completed(ResourceSizeReport::Known(8)),
            Instant::now(),
        )
        .expect("the size is learned from the continuation");
    assert_eq!(read_all(&mut state, 0, 10), b"abcdefgh");
}

#[test]
fn an_empty_range_is_never_requested_near_the_top_of_the_offset_space() {
    let (mut state, ids) = fresh_state(retry_policy());
    for position in [u64::MAX, u64::MAX - 1] {
        let plan = state.plan_read(&ids, position, 64, MAX_REQUEST, Instant::now());
        if position == u64::MAX {
            assert_eq!(plan, Ok(ReadPlan::EndOfFile));
        } else {
            assert_eq!(plan, Ok(ReadPlan::RequestQueued));
        }
    }
    let requests = state.take_requests();
    assert_eq!(requests.len(), 1, "only the one-byte range below the limit");
    assert_eq!(
        (requests[0].offset, requests[0].length.get()),
        (u64::MAX - 1, 1)
    );
}

#[test]
fn unknown_size_range_not_satisfiable_at_the_end_is_the_clean_end_of_the_file() {
    // Длина файла кратна запросу (2 × 4 байта): сервер отвечает 416 на диапазон с самого конца.
    let (mut state, ids, id) = started(4);
    state.accept_bytes(id, 0, b"abcd").expect("bytes");
    state
        .finish_request(id, completed(ResourceSizeReport::Unknown), Instant::now())
        .expect("first");
    read_all(&mut state, 0, 10);
    state
        .plan_read(&ids, 4, 4, MAX_REQUEST, Instant::now())
        .expect("plan");
    let second = state.take_requests()[0].id;
    state.accept_bytes(second, 4, b"efgh").expect("bytes");
    state
        .finish_request(
            second,
            completed(ResourceSizeReport::Unknown),
            Instant::now(),
        )
        .expect("second");
    read_all(&mut state, 4, 10);
    state
        .plan_read(&ids, 8, 4, MAX_REQUEST, Instant::now())
        .expect("plan");
    let third = state.take_requests()[0].id;
    assert_eq!(
        state.finish_request(
            third,
            RangeOutcome::Failed(RangeFailure::OutOfRange),
            Instant::now()
        ),
        Ok(())
    );
    assert!(state.failure().is_none());
    assert_eq!(
        state.plan_read(&ids, 8, 4, MAX_REQUEST, Instant::now()),
        Ok(ReadPlan::EndOfFile)
    );
}

#[test]
fn unknown_size_out_of_range_elsewhere_is_still_an_error() {
    let (mut state, ids, id) = started(4);
    state.accept_bytes(id, 0, b"abcd").expect("bytes");
    state
        .finish_request(id, completed(ResourceSizeReport::Unknown), Instant::now())
        .expect("first");
    read_all(&mut state, 0, 10);
    // Перемотка далеко вперёд: 416 не у конца прочитанного — не «конец файла».
    state
        .plan_read(&ids, 500, 4, MAX_REQUEST, Instant::now())
        .expect("plan");
    let far = state.take_requests()[0].id;
    assert_eq!(
        state.finish_request(
            far,
            RangeOutcome::Failed(RangeFailure::OutOfRange),
            Instant::now()
        ),
        Err(PushedFileError::RangeRejected { offset: 500 })
    );
}

#[test]
fn the_unknown_end_of_the_file_cannot_move() {
    let (mut state, ids, id) = started(10);
    state.accept_bytes(id, 0, b"abcdef").expect("bytes");
    state
        .finish_request(id, completed(ResourceSizeReport::Unknown), Instant::now())
        .expect("short answer: the end is 6");
    read_all(&mut state, 0, 10);
    // Перемотка назад и другой короткий итог: конец оказался в другом месте.
    state
        .plan_read(&ids, 2, 8, MAX_REQUEST, Instant::now())
        .expect("plan");
    let again = state.take_requests()[0].id;
    state.accept_bytes(again, 2, b"cde").expect("bytes");
    assert_eq!(
        state.finish_request(
            again,
            completed(ResourceSizeReport::Unknown),
            Instant::now()
        ),
        Err(PushedFileError::ResourceSizeChanged {
            before: 6,
            after: 5
        })
    );
}

#[test]
fn a_zero_length_request_is_an_error_not_a_one_byte_request() {
    let (mut state, ids) = fresh_state(retry_policy());
    assert_eq!(
        state.begin_initial_read(&ids, 0),
        Err(PushedFileError::EmptyRangeRequest { offset: 0 })
    );
    assert!(state.take_requests().is_empty(), "nothing was queued");
}
