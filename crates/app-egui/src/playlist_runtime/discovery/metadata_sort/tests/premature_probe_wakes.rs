//! Consumer contract допускает notification до готовности terminal metadata.

use super::*;

#[test]
fn premature_discovery_wakes_preserve_probe_then_commit_exact_sort() {
    let (discovery_wake_sender, discovery_wake_receiver) = mpsc::channel();
    let (app_wake_sender, app_wake_receiver) = mpsc::channel();
    let contract_wake = Arc::new(ContractWake {
        discovery_sender: discovery_wake_sender,
        app_sender: app_wake_sender,
    });
    let (probe_entered_sender, probe_entered_receiver) = mpsc::sync_channel(0);
    let (probe_release_sender, probe_release_receiver) = mpsc::sync_channel(0);
    let blocking_probe = Arc::new(BlockingMetadataProbe {
        entered_sender: probe_entered_sender,
        release_receiver: Mutex::new(probe_release_receiver),
    });
    assert_eq!(
        blocking_probe
            .read_fingerprint(
                Path::new("/music/unknown.mkv"),
                &source_core::CancellationToken::new(),
            )
            .expect("fixture fingerprint read must succeed"),
        LocalMediaFingerprint::new(20, SystemTime::UNIX_EPOCH)
    );

    let discovery_executor = playlist_discovery::DiscoveryExecutor::start_with_probe(
        blocking_probe,
        contract_wake.clone(),
    )
    .expect("discovery executor must start");
    let cpu_executor = start_cpu_executor().expect("metadata sort CPU executor must start");
    let mut controller = PlaylistController::new();
    controller
        .append(vec![
            PlaylistItemDraft::local(
                LocalLocator::Native("/music/bravo.mkv".into()),
                Some(LocalSourceFingerprint::new(10, SystemTime::UNIX_EPOCH)),
                CachedPlaylistMetadata::new("bravo.mkv", PlaylistMediaKind::Video)
                    .with_title(Some("Bravo".to_owned())),
            ),
            PlaylistItemDraft::local(
                LocalLocator::Native("/music/unknown.mkv".into()),
                Some(LocalSourceFingerprint::new(10, SystemTime::UNIX_EPOCH)),
                CachedPlaylistMetadata::new("unknown.mkv", PlaylistMediaKind::Video),
            ),
        ])
        .expect("fixture playlist rows must be admitted");
    let structural_revision = controller.view_snapshot().structural_revision();
    let app_wake_port = AppWakePort::new(AppWakeOwner::PlaylistRuntime, contract_wake);
    let mut owner = MetadataSortOwner::new(app_wake_port);
    owner
        .start(
            Some(&discovery_executor),
            Some(&cpu_executor),
            &controller,
            SortCanonicalQueue::new(PlaylistSortKey::Title, SortDirection::Ascending),
        )
        .expect("metadata sort must start");

    probe_entered_receiver
        .recv_timeout(RENDEZVOUS_TIMEOUT)
        .expect("discovery worker did not enter the held probe");
    wait_for_discovery_wake(&discovery_wake_receiver);
    assert!(
        owner
            .drain(Some(&cpu_executor), structural_revision)
            .is_none()
    );

    // Второй rendezvous-send завершится только после повторного recv в helper-е.
    // Значит первый wake уже прошёл drain/read_model, пока probe ещё удерживался.
    let (forward_sender, forward_receiver) = mpsc::sync_channel(0);
    let forwarder = std::thread::spawn(move || {
        forward_sender
            .send(())
            .expect("first premature notification");
        forward_sender
            .send(())
            .expect("second premature notification");
        probe_release_sender
            .send(())
            .expect("release after pending observation");
        // После release передаём настоящие уведомления executor-а. Disconnect
        // означает штатное завершение consumer-а, а не потерю worker error.
        while let Ok(()) = discovery_wake_receiver.recv() {
            if forward_sender.send(()).is_err() {
                return;
            }
        }
    });
    advance_from_probe_to_cpu_after_wakes(
        &mut owner,
        &cpu_executor,
        structural_revision,
        &forward_receiver,
    );
    assert_eq!(
        app_wake_receiver
            .recv_timeout(RENDEZVOUS_TIMEOUT)
            .expect("metadata sort CPU completion wake was not delivered"),
        AppWakeOwner::PlaylistRuntime
    );

    let Some(MetadataSortTerminal::Prepared {
        prepared,
        patches,
        failure_counts,
        ..
    }) = owner.drain(Some(&cpu_executor), structural_revision)
    else {
        panic!("completed probe and CPU task must publish an exact prepared terminal");
    };
    assert_eq!(patches.len(), 1);
    assert_eq!(failure_counts, DiscoveryFailureCounts::default());
    let commit = controller
        .preflight_canonical_sort(structural_revision, prepared, patches)
        .expect("matching prepared sort must pass preflight");
    let outcome = controller.commit_canonical_sort(commit);
    assert!(outcome.domain.reordered());
    assert_eq!(outcome.domain.metadata().applied_count(), 1);
    assert!(
        owner
            .drain(Some(&cpu_executor), structural_revision)
            .is_none()
    );
    drop(forward_receiver);
    drop(owner);
    drop(discovery_executor);
    forwarder.join().expect("join notification forwarder");
}
