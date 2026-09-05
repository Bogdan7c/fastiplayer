//! Worker завершается только после наблюдения Pending настоящим metadata owner-ом.

use std::sync::mpsc;

use super::*;

struct GatedMetadataResolver {
    started: mpsc::SyncSender<()>,
    release: Mutex<mpsc::Receiver<()>>,
    calls: AtomicUsize,
}

impl YtDlpMetadataResolver for GatedMetadataResolver {
    fn resolve(
        &self,
        _locator: &service_ytdlp::YtDlpMediaLocator,
        _config: &YtDlpConfig,
        cancellation: &CancellationToken,
    ) -> YtDlpMetadataTaskOutcome {
        self.calls.fetch_add(1, Ordering::SeqCst);
        self.started.send(()).expect("signal metadata worker entry");
        self.release
            .lock()
            .expect("metadata completion gate")
            .recv_timeout(Duration::from_secs(10))
            .expect("owner must release metadata completion gate");
        assert!(
            !cancellation.is_cancelled(),
            "active metadata job remains owned"
        );
        YtDlpMetadataTaskOutcome::Resolved {
            title: Some("Resolved after pending".to_string()),
            duration: Some(Duration::from_secs(42)),
        }
    }
}

#[test]
fn pending_drain_retains_exact_job_until_resolved_metadata_reaches_queue() {
    let mut controller = PlaylistController::new();
    let url = "https://youtu.be/pending-owner";
    let item_id = append_yt_dlp_item(
        &mut controller,
        url,
        CachedPlaylistMetadata::new("Fallback title", PlaylistMediaKind::Unknown)
            .with_title(Some("Initial title".to_string())),
    );
    let (started_sender, started_receiver) = mpsc::sync_channel(1);
    let (release_sender, release_receiver) = mpsc::sync_channel(1);
    let resolver = Arc::new(GatedMetadataResolver {
        started: started_sender,
        release: Mutex::new(release_receiver),
        calls: AtomicUsize::new(0),
    });
    let mut owner =
        YtDlpMetadataOwner::new(AppWakePort::disconnected(AppWakeOwner::PlaylistRuntime));
    owner.replace_resolver_for_test(resolver.clone());
    let now = Instant::now();
    assert_eq!(
        owner
            .request(vec![demand(&controller, item_id, url)], now)
            .accepted,
        1
    );
    started_receiver
        .recv_timeout(Duration::from_secs(10))
        .expect("worker entered resolver");

    // Готовность worker-а ещё запрещена gate-ом: Pending не зависит от скорости CPU.
    let visible_change = owner.drain(&mut controller, now);
    let pending_title = controller
        .queue()
        .item(item_id)
        .expect("retained item")
        .cached_metadata()
        .title()
        .map(ToOwned::to_owned);
    let coalesced = owner
        .request(vec![demand(&controller, item_id, url)], now)
        .coalesced;

    // Сначала освобождаем worker, чтобы даже провал assertions не оставлял его в gate.
    release_sender.send(()).expect("allow metadata completion");
    assert!(!visible_change);
    assert_eq!(pending_title.as_deref(), Some("Initial title"));
    assert_eq!(coalesced, 1);
    drain_until_idle(&mut owner, &mut controller, now);
    assert_eq!(resolver.calls.load(Ordering::SeqCst), 1);
    assert_eq!(
        controller
            .queue()
            .item(item_id)
            .expect("same item after completion")
            .cached_metadata()
            .title(),
        Some("Resolved after pending")
    );
}
