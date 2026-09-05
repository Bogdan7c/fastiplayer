use std::path::Path;
use std::sync::mpsc::{self, Receiver, Sender, SyncSender};
use std::sync::{Arc, Mutex};
use std::time::{Duration, SystemTime};

use media_core::MediaTagMetadata;
use playlist_core::{
    CachedPlaylistMetadata, LocalLocator, LocalSourceFingerprint, PlaylistItemDraft,
    PlaylistMediaKind, PlaylistSortKey, SortCanonicalQueue, SortDirection,
};
use playlist_discovery::{
    DiscoveryFailureCounts, DiscoveryProbe, DiscoveryWakePort, LocalMediaFingerprint,
    LocalMediaKind, ProbeOneLocalMediaError, ProbedLocalMedia, WakeDisconnected,
};

use super::*;
use crate::app_wake::{AppWakeEvent, AppWakeOwner, AppWakePort, WakeEmitter};

const RENDEZVOUS_TIMEOUT: Duration = Duration::from_secs(2);

struct ContractWake {
    discovery_sender: Sender<()>,
    app_sender: Sender<AppWakeOwner>,
}

impl DiscoveryWakePort for ContractWake {
    fn wake(&self) -> Result<(), WakeDisconnected> {
        self.discovery_sender
            .send(())
            .expect("test wake receiver must outlive the discovery executor");
        Ok(())
    }
}

impl WakeEmitter for ContractWake {
    fn emit(&self, event: AppWakeEvent) -> Result<(), ()> {
        self.app_sender
            .send(event.owner())
            .expect("test wake receiver must outlive the metadata sort owner");
        Ok(())
    }
}

struct BlockingMetadataProbe {
    entered_sender: SyncSender<()>,
    release_receiver: Mutex<Receiver<()>>,
}

impl DiscoveryProbe for BlockingMetadataProbe {
    fn read_fingerprint(
        &self,
        _locator: &Path,
        _cancellation: &source_core::CancellationToken,
    ) -> Result<LocalMediaFingerprint, ProbeOneLocalMediaError> {
        Ok(LocalMediaFingerprint::new(20, SystemTime::UNIX_EPOCH))
    }

    fn probe(
        &self,
        locator: &Path,
        _cancellation: &source_core::CancellationToken,
    ) -> Result<ProbedLocalMedia, ProbeOneLocalMediaError> {
        self.entered_sender
            .send(())
            .expect("test must wait until the discovery probe owns the work");
        self.release_receiver
            .lock()
            .expect("test release mutex must not be poisoned")
            .recv()
            .expect("test must release the held discovery probe");

        let filename = locator
            .file_name()
            .expect("fixture locator must have a filename")
            .to_string_lossy()
            .into_owned();
        let tags = MediaTagMetadata {
            title: Some("Alpha".to_owned()),
            ..MediaTagMetadata::default()
        };
        Ok(ProbedLocalMedia::new(
            filename,
            LocalMediaKind::VideoContaining,
            None,
            tags,
            LocalMediaFingerprint::new(20, SystemTime::UNIX_EPOCH),
        ))
    }
}

fn wait_for_discovery_wake(receiver: &Receiver<()>) {
    receiver
        .recv_timeout(RENDEZVOUS_TIMEOUT)
        .expect("expected discovery wake was not delivered");
}

fn advance_from_probe_to_cpu_after_wakes(
    owner: &mut MetadataSortOwner,
    cpu_executor: &BoundedExecutor,
    structural_revision: PlaylistStructuralRevision,
    discovery_wake_receiver: &Receiver<()>,
) {
    loop {
        wait_for_discovery_wake(discovery_wake_receiver);
        assert!(
            owner
                .drain(Some(cpu_executor), structural_revision)
                .is_none()
        );
        if owner.read_model().phase == Some(MetadataSortPhase::PreparingKeys) {
            return;
        }
        assert_eq!(owner.read_model().phase, Some(MetadataSortPhase::Probing));
    }
}

#[test]
fn pending_discovery_probe_drains_empty_then_prepares_exact_sort_after_real_wakes() {
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

    // Первый drain опустошил mailbox, пока Probe всё ещё удерживается fake-ом.
    // Поэтому следующий Discovery wake причинно принадлежит terminal publication после release.
    probe_release_sender
        .send(())
        .expect("held discovery probe must accept release");
    advance_from_probe_to_cpu_after_wakes(
        &mut owner,
        &cpu_executor,
        structural_revision,
        &discovery_wake_receiver,
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
}

#[path = "tests/premature_probe_wakes.rs"]
mod premature_probe_wakes;
