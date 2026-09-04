//! Control receipt не должен обгонять consumer-visible snapshot установленного media.

use super::*;
use crate::worker::MediaInstallControlCommand;
use crate::{FrameCounters, MediaInstallControl, MediaInstallReceipt};

#[test]
fn installed_snapshot_is_visible_before_authorization_acknowledgement() {
    let request_id = MediaInstallRequestId::new_unique();
    let (receipt, install_port) = MediaInstallReceipt::new(request_id);
    // Rendezvous удерживает worker точно на публикации ack. Поэтому outer loop
    // не может случайно скрыть неправильный порядок своим последующим publish.
    let (outcome_tx, outcome_rx) = bounded(0);
    let (snapshots_tx, snapshots_rx) = bounded(1);
    let owner = std::thread::spawn(move || {
        let mut runtime = crate::worker::tests::runtime_for_tests(Instant::now());
        runtime.session.stage_prepared_media_install(
            request_id,
            PreparedMedia::from_external_label("snapshot-order", Box::new(EmptyDemuxer)),
            PlaybackIntent::StartPaused,
            PlaybackIntentRevision::INITIAL,
            install_port,
            MediaInstallVideoResourcePort::any_playable(NoVideoResourcePort),
        );
        snapshots_tx
            .send(
                runtime
                    .snapshot_publisher
                    .snapshot_rx_for_drain_latest
                    .clone(),
            )
            .unwrap();

        runtime.handle_worker_command(WorkerCommand::MediaInstallControl(
            MediaInstallControlCommand {
                control: MediaInstallControl::Authorize(AuthorizeInstallCommit { request_id }),
                outcome_tx,
            },
        ));
        runtime
            .session
            .snapshot_with_frame_counters(FrameCounters::default())
    });
    let snapshots = snapshots_rx.recv_timeout(Duration::from_secs(2)).unwrap();
    let snapshot_before_ack = snapshots.recv_timeout(Duration::from_secs(2));
    let outcome = outcome_rx.recv_timeout(Duration::from_secs(2));
    let final_snapshot = owner.join().expect("worker control turn must finish");
    assert_eq!(
        outcome.unwrap(),
        MediaInstallControlOutcome::AuthorizationAccepted
    );
    let snapshot = snapshot_before_ack.expect("Installed snapshot must precede control ack");
    let Some(MediaInstallCompletion::Installed {
        media_instance_id, ..
    }) = receipt.try_take_completion()
    else {
        panic!("authorization must install the exact prepared source");
    };
    assert_eq!(snapshot.media_instance_id, Some(media_instance_id));
    assert_eq!(snapshot.source_label.as_deref(), Some("snapshot-order"));
    assert_eq!(snapshot.duration, Some(Duration::from_secs(5)));
    assert_eq!(snapshot.playback_state, PlaybackState::Paused);
    assert_eq!(final_snapshot.media_instance_id, snapshot.media_instance_id,);
}
