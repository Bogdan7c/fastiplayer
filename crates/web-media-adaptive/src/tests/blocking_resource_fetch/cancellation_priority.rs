//! Обе authority отменяют уже активное committed чтение через consumer boundary.

use super::*;

#[derive(Clone, Copy)]
enum CancellationAuthority {
    Source,
    Seek,
}

#[test]
fn source_cancellation_of_active_armed_read_reaches_consumer() {
    assert_active_read_cancellation(CancellationAuthority::Source);
}

#[test]
fn seek_cancellation_of_active_armed_read_reaches_consumer() {
    assert_active_read_cancellation(CancellationAuthority::Seek);
}

fn assert_active_read_cancellation(authority: CancellationAuthority) {
    let server = StalledBodyServer::start();
    let source_cancellation = CancellationToken::new();
    let seek_cancellation = media_core::DemuxSeekCancellationToken::new();
    let context = context(
        &server.target,
        source_cancellation.clone(),
        same_origin_redirects(),
        None,
        None,
    );
    let controller = AdaptiveRestartableReadInterruption::new();
    let attempt = controller.new_attempt().expect("allocate body attempt");
    let mut resource = context
        .open_resource_streaming_blocking_with_restartable_read_attempt(
            AdaptiveResourceFetchRequest::full(
                SourceGeneration::new(1),
                server.target.clone(),
                NonZeroUsize::new(16).expect("body bound"),
                AdaptiveResourcePurpose::MediaSegment,
                AdaptiveResourceQueryApplication::BypassScopedQuery,
            ),
            seek_cancellation.clone(),
            attempt.clone(),
        )
        .expect("open stalled resource");
    assert_eq!(
        attempt.arm_as_current(),
        AdaptiveRestartableReadArmOutcome::Armed
    );
    let (sender, receiver) = std::sync::mpsc::sync_channel(1);
    let reader = thread::spawn(move || {
        let first = resource.next_chunk();
        let second = resource.next_chunk();
        sender
            .send((first, second, resource.received_body_bytes()))
            .expect("publish consumer results");
    });

    // Сигнал старта потока недостаточен: он допускает отмену ещё до входа в read.
    // Владелец phase подтверждает занятый slot без отправки restartable interruption.
    controller.wait_until_network_read_is_active(TEST_TIMEOUT);
    match authority {
        CancellationAuthority::Source => source_cancellation.cancel(),
        CancellationAuthority::Seek => seek_cancellation.cancel(),
    }

    let (first, second, received_bytes) = receiver
        .recv_timeout(TEST_TIMEOUT)
        .expect("active cancellation must wake body");
    assert!(matches!(first, Err(AdaptiveTransportError::Cancelled)));
    assert!(matches!(second, Err(AdaptiveTransportError::Cancelled)));
    assert_eq!(received_bytes, 0);
    match authority {
        CancellationAuthority::Source => assert!(!seek_cancellation.is_cancelled()),
        CancellationAuthority::Seek => assert!(!source_cancellation.is_cancelled()),
    }
    reader.join().expect("join cancelled reader");
    server
        .disconnected
        .recv_timeout(TEST_TIMEOUT)
        .expect("cancelled body must close socket");
    assert_eq!(server.request_count.load(Ordering::SeqCst), 1);
    assert_eq!(
        controller.request_active_read_interruption(),
        AdaptiveRestartableReadInterruptionRequest::AlreadyQuiescent
    );
}
