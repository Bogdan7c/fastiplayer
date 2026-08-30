//! Functional manifest-refresh lifecycle с детерминированным HTTP rendezvous.

use std::sync::{Arc, Mutex, mpsc};

use super::*;

/// На unwind освобождает server rendezvous раньше, чем `LocalServer::drop` ждёт join.
struct ServerRequestRelease(Option<mpsc::SyncSender<()>>);

impl ServerRequestRelease {
    fn release(&mut self) {
        if let Some(sender) = self.0.take() {
            sender.send(()).expect("release held server request");
        }
    }
}

impl Drop for ServerRequestRelease {
    fn drop(&mut self) {
        if let Some(sender) = self.0.take() {
            // Drop не может вернуть ошибку; disconnect также гарантированно будит receiver.
            let _ = sender.try_send(());
        }
    }
}

#[test]
fn live_manifest_refresh_fences_slow_stale_generation() {
    let (stale_started_sender, stale_started_receiver) = mpsc::sync_channel(1);
    let (stale_release_sender, stale_release_receiver) = mpsc::sync_channel(1);
    let stale_release_receiver = Arc::new(Mutex::new(stale_release_receiver));
    let server = LocalServer::start(move |index, _| {
        if index == 0 {
            stale_started_sender
                .send(())
                .expect("report stale request admission");
            stale_release_receiver
                .lock()
                .expect("stale release mutex")
                .recv()
                .expect("release stale request");
            Vec::new()
        } else {
            response("200 OK", &[], b"current")
        }
    });
    let mut stale_request_release = ServerRequestRelease(Some(stale_release_sender));
    let target = server.target("/refresh");
    let mut fetcher = AdaptiveManifestFetcher::new(context_with_presentation(
        &target,
        CancellationToken::new(),
        same_origin_redirects(),
        None,
        None,
        MediaPresentation::Live,
    ))
    .expect("manifest fetcher");
    fetcher
        .request(
            ManifestFetchRequest::new(target.clone(), SourceGeneration::new(1)),
            Instant::now(),
        )
        .expect("initial request");
    assert!(matches!(
        fetcher.poll(Instant::now()),
        ManifestPoll::TemporarilyUnavailable { .. }
    ));
    stale_started_receiver
        .recv_timeout(TEST_TIMEOUT)
        .expect("initial request was admitted");
    fetcher
        .request(
            ManifestFetchRequest::new(target.clone(), SourceGeneration::new(2)),
            Instant::now(),
        )
        .expect("new generation refresh");
    assert!(matches!(
        fetcher.poll(Instant::now()),
        ManifestPoll::TemporarilyUnavailable { .. }
    ));
    stale_request_release.release();
    let stale_request = fetcher.request(
        ManifestFetchRequest::new(target, SourceGeneration::new(1)),
        Instant::now(),
    );
    assert!(matches!(
        stale_request,
        Err(AdaptiveTransportError::StaleGeneration { .. })
    ));
    let resource = poll_manifest_ready(&mut fetcher);
    assert_eq!(resource.generation(), SourceGeneration::new(2));
    assert_eq!(resource.bytes().as_ref(), b"current");
    assert_eq!(server.request_count(), 2);
}
