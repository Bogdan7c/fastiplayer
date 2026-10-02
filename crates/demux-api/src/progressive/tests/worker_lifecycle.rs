//! Границы очереди, drop/отмена worker-а и синхронизация push-исходов.

use super::*;

#[test]
fn oversized_packet_fails_with_typed_bounded_error() {
    let (sender, inner) = blocking_demuxer();
    let mut progressive =
        ProgressiveDemuxer::new(inner, CancellationToken::new(), limits(2, 4), retry_hint())
            .expect("progressive worker starts");
    sender
        .send(DemuxReadEvent::Packet(Packet::new_unbounded(
            TrackId::new(1),
            TrackKind::Video,
            Duration::ZERO,
            None,
            true,
            Bytes::from(vec![0_u8; 5]),
        )))
        .expect("worker receiver lives");

    let error = loop {
        match progressive.next_event() {
            Ok(DemuxReadEvent::TemporarilyUnavailable(_)) => {
                thread::sleep(DemuxRetryHint::MIN_RETRY_AFTER);
            }
            Err(error) => break error,
            Ok(other) => panic!("unexpected event: {other:?}"),
        }
    };
    let typed = error
        .downcast_ref::<ProgressiveDemuxPacketTooLargeError>()
        .expect("typed oversize source сохраняется");
    assert_eq!(typed.packet_bytes, 5);
    assert_eq!(typed.budget_bytes, 4);
}

#[test]
fn drop_cancels_worker_waiting_on_full_backpressure_queue() {
    let read_count = Arc::new(AtomicUsize::new(0));
    let inner = Box::new(CountingPacketDemuxer {
        read_count: Arc::clone(&read_count),
    });
    let progressive =
        ProgressiveDemuxer::new(inner, CancellationToken::new(), limits(1, 1), retry_hint())
            .expect("progressive worker starts");
    let shared = Arc::clone(&progressive.shared);

    let fill_deadline = Instant::now() + Duration::from_secs(1);
    while read_count.load(Ordering::SeqCst) < 2 && Instant::now() < fill_deadline {
        thread::yield_now();
    }
    assert_eq!(read_count.load(Ordering::SeqCst), 2);

    drop(progressive);
    let stop_deadline = Instant::now() + Duration::from_secs(1);
    while !shared.lock_queue().worker_stopped && Instant::now() < stop_deadline {
        thread::sleep(Duration::from_millis(1));
    }
    assert!(shared.lock_queue().worker_stopped);
}

/// Уже опубликованная отмена не позволяет EOF worker-у войти в timed wait.
#[test]
fn eof_wait_observes_preexisting_cancellation_without_blocking() {
    // Shared state использует production queue/Condvar boundary без фонового thread-а.
    let shared = ProgressiveSharedState::new(limits(1, 1));
    // Отмена устанавливается до wait, точно моделируя lost-wakeup guard.
    let cancellation = CancellationToken::new();
    // Production token выражает lifecycle intent без прямой мутации queue state.
    cancellation.cancel();

    // Guard обязан вернуть управление синхронно, не входя в timed wait.
    wait_for_seek_command(&shared, &cancellation);

    // Wait не меняет lifecycle state, которым владеет caller.
    assert!(cancellation.is_cancelled());
}

/// Уже опубликованный seek command не теряется внутри EOF wait boundary.
#[test]
fn eof_wait_observes_preexisting_seek_without_blocking() {
    // Shared state использует production queue/Condvar boundary без фонового thread-а.
    let shared = ProgressiveSharedState::new(limits(1, 1));
    // Exact request остаётся владельцем целевой позиции.
    let request = DemuxSeekRequest::accurate(Duration::from_secs(1));
    // Preview фиксирует уже подтверждённый player-owner anchor.
    let preview = DemuxSeekResult {
        requested_position: MediaTime::from_duration(request.timestamp),
        actual_position: MediaTime::from_duration(request.timestamp),
        actual_track_timestamp: None,
    };
    // Command публикуется до wait, точно моделируя lost-wakeup guard.
    shared.lock_queue().pending_seek = Some(ProgressiveSeekCommand::Previewed {
        generation: 1,
        request,
        preview,
        worker_result_policy: ProgressivePreviewWorkerResultPolicy::ExactPreview,
        cancellation: DemuxSeekCancellationToken::new(),
    });
    // Неотменённый token заставляет проверку дойти именно до pending seek.
    let cancellation = CancellationToken::new();

    // Guard обязан вернуть управление синхронно, не входя в timed wait.
    wait_for_seek_command(&shared, &cancellation);

    // Wait не забирает command: его применяет только worker owner.
    assert!(shared.lock_queue().pending_seek.is_some());
}

/// Seekable TUA выполняет реальный timeout, затем cancellation завершает worker.
#[test]
fn seekable_tua_wait_observes_cancellation_and_stops_before_return() {
    // Zero-capacity channels делают каждый inner read наблюдаемым.
    let (read_started, event_sender, inner) = gated_seekable_demuxer();
    // Test owner сохраняет cancellation handle до terminal assertion.
    let cancellation = CancellationToken::new();
    // Deferred constructor запускает production seekable worker boundary.
    let mut progressive = ProgressiveDemuxer::new_deferred_seekable(
        move || Ok(inner),
        exact_seek_controller(),
        cancellation.clone(),
        limits(2, 1024),
        retry_hint(),
    )
    .expect("seekable worker starts");

    // Initial track publication освобождает worker до первого controlled read.
    assert!(matches!(
        poll_until_event(&mut progressive).expect("initial tracks"),
        DemuxReadEvent::TracksChanged(_)
    ));
    // Первый read уже принадлежит worker-у и ждёт test event.
    wait_for_gated_read(&read_started, 1);
    // 50 ms гарантированно больше production cancellation poll quantum 25 ms.
    let completed_retry_hint = DemuxRetryHint::new(Duration::from_millis(50))
        .expect("controlled retry hint обязан быть валиден");
    // Первая TUA проходит normal retry timeout без queue publication.
    event_sender
        .send(DemuxReadEvent::TemporarilyUnavailable(completed_retry_hint))
        .expect("worker ждёт первый controlled event");
    // Второй read доказывает, что wait_for_inner_retry завершил timeout path.
    wait_for_gated_read(&read_started, 2);

    // Длинный retry не может естественно истечь раньше cancellation.
    let cancelled_retry_hint = DemuxRetryHint::new(DemuxRetryHint::MAX_RETRY_AFTER)
        .expect("maximum retry hint обязан быть валиден");
    // Worker получает вторую TUA, оставаясь внутри уже начатого read path.
    event_sender
        .send(DemuxReadEvent::TemporarilyUnavailable(cancelled_retry_hint))
        .expect("worker ждёт второй controlled event");
    // Cancellation прерывает retry wait, а не публикует fake EOF/error.
    cancellation.cancel();
    // Test не возвращается раньше mark_worker_stopped.
    wait_until_worker_stopped(&progressive);
    // Ни одна inner TUA не должна попасть в bounded message queue.
    assert!(progressive.shared.lock_queue().messages.is_empty());
}

/// Gated old-generation event даёт Stale, а cancelled event даёт Stopped.
#[test]
fn seekable_stale_and_stopped_push_outcomes_are_synchronized() {
    // Один fake управляет старым, актуальным и cancelled reads.
    let (read_started, event_sender, inner) = gated_seekable_demuxer();
    // Cancellation handle нужен для exact Stopped push outcome.
    let cancellation = CancellationToken::new();
    // Worker использует production deferred seekable orchestration.
    let mut progressive = ProgressiveDemuxer::new_deferred_seekable(
        move || Ok(inner),
        exact_seek_controller(),
        cancellation.clone(),
        limits(2, 1024),
        retry_hint(),
    )
    .expect("seekable worker starts");

    // Initial tracks удаляются до generation race.
    assert!(matches!(
        poll_until_event(&mut progressive).expect("initial tracks"),
        DemuxReadEvent::TracksChanged(_)
    ));
    // Generation zero read блокируется до публикации нового seek intent.
    wait_for_gated_read(&read_started, 1);
    // Новый player intent немедленно меняет visible queue generation.
    let requested_position = Duration::from_secs(2);
    progressive
        .seek_with_request(DemuxSeekRequest::accurate(requested_position))
        .expect("new generation seek accepted");
    // Старый EOF возвращается только после generation change.
    event_sender
        .send(DemuxReadEvent::EndOfStream)
        .expect("worker ждёт stale controlled event");
    // Второй read возможен только после Stale drop и применения pending seek.
    wait_for_gated_read(&read_started, 2);
    // Stale event не должен занимать current-generation queue.
    assert!(progressive.shared.lock_queue().messages.is_empty());

    // Актуальный packet доказывает, что Stale outcome не остановил worker.
    event_sender
        .send(DemuxReadEvent::Packet(Packet::new_unbounded(
            TrackId::new(1),
            TrackKind::Audio,
            requested_position,
            None,
            true,
            Bytes::from_static(&[0x7a]),
        )))
        .expect("worker ждёт current-generation packet");
    // Player owner получает только packet актуальной generation.
    let DemuxReadEvent::Packet(packet) =
        poll_until_event(&mut progressive).expect("current-generation packet")
    else {
        panic!("current-generation packet expected");
    };
    // Exact timestamp подтверждает применение authoritative seek.
    assert_eq!(packet.pts, requested_position);

    // Третий read блокирует worker внутри parser boundary.
    wait_for_gated_read(&read_started, 3);
    // Cancellation устанавливается до возврата controlled event-а.
    cancellation.cancel();
    // Event после cancellation обязан получить Stopped, а не Published.
    event_sender
        .send(DemuxReadEvent::EndOfStream)
        .expect("worker ждёт cancelled controlled event");
    // Test ждёт exact mark_worker_stopped notification.
    wait_until_worker_stopped(&progressive);
    // Cancelled event не должен остаться скрытым terminal message-ом.
    assert!(progressive.shared.lock_queue().messages.is_empty());
}

#[test]
fn seekable_worker_wakes_after_eof_and_drops_superseded_generation_output() {
    let controller = ProgressiveSeekController::new(|request| {
        Ok(DemuxSeekResult {
            requested_position: MediaTime::from_duration(request.timestamp),
            actual_position: MediaTime::from_duration(request.timestamp),
            actual_track_timestamp: None,
        })
    });
    let mut progressive = ProgressiveDemuxer::new_deferred_seekable(
        || {
            Ok(Box::new(CommandSeekableDemuxer {
                position: Duration::ZERO,
                packet_emitted: false,
            }))
        },
        controller,
        CancellationToken::new(),
        limits(4, 16),
        retry_hint(),
    )
    .expect("seekable worker starts");

    assert!(matches!(
        poll_until_event(&mut progressive).expect("initial tracks"),
        DemuxReadEvent::TracksChanged(_)
    ));
    assert!(matches!(
        poll_until_event(&mut progressive).expect("initial packet"),
        DemuxReadEvent::Packet(_)
    ));
    assert!(matches!(
        poll_until_event(&mut progressive).expect("initial EOF"),
        DemuxReadEvent::EndOfStream
    ));

    progressive
        .seek_with_request(DemuxSeekRequest::accurate(Duration::from_secs(8)))
        .expect("first command");
    progressive
        .seek_with_request(DemuxSeekRequest::accurate(Duration::from_secs(2)))
        .expect("latest command");
    let DemuxReadEvent::Packet(packet) =
        poll_until_event(&mut progressive).expect("post-seek packet")
    else {
        panic!("latest seek packet expected");
    };
    assert_eq!(packet.pts, Duration::from_secs(2));
}
