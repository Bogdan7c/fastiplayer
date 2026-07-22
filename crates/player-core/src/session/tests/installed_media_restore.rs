use std::num::NonZeroU64;
use std::time::Duration;

use crossbeam_channel::{Receiver, TryRecvError, bounded};
use media_core::TrackKind;

use super::test_support::{SharedFakeVideoDecoderThread, fake_track, install_fake_media};
use super::*;
use crate::media_install::AcceptedPlaybackIntent;
use crate::{
    InstalledMediaRestoreFailureStage, InstalledMediaStateRestore,
    InstalledMediaStateRestoreOutcome, InstalledPositionRestore, InstalledSubtitleRestore,
    InstalledTrackRestore, MediaInstallRequestId, MediaInstanceId, PlaybackIntent,
    PlaybackIntentRevision,
};

/// Создаёт exact installed target без зависимости от container или codec implementation.
fn correlated_installed_session(
    tracks: Vec<media_core::TrackInfo>,
) -> (PlayerSession, MediaInstallRequestId, MediaInstanceId) {
    let mut session = PlayerSession::default();
    install_fake_media(&mut session, tracks);
    let request_id = MediaInstallRequestId::from_non_zero(
        NonZeroU64::new(901).expect("test request id is non-zero"),
    );
    let media_instance_id = MediaInstanceId::from_non_zero(
        NonZeroU64::new(902).expect("test media instance id is non-zero"),
    );
    session.playback_intent_control.register_staged_request(
        request_id,
        AcceptedPlaybackIntent {
            revision: PlaybackIntentRevision::INITIAL,
            intent: PlaybackIntent::StartPaused,
        },
    );
    session
        .playback_intent_control
        .commit_staged_request(request_id, media_instance_id, |_| {});
    session.snapshot.media_instance_id = Some(media_instance_id);
    (session, request_id, media_instance_id)
}

/// Запускает position restore и возвращает request-owned outcome receiver.
fn begin_position_restore(
    session: &mut PlayerSession,
    request_id: MediaInstallRequestId,
    media_instance_id: MediaInstanceId,
) -> Receiver<InstalledMediaStateRestoreOutcome> {
    let (outcome_tx, outcome_rx) = bounded(1);
    session.begin_installed_media_state_restore(
        InstalledMediaStateRestore {
            request_id,
            media_instance_id,
            video_track: InstalledTrackRestore::KeepDefault,
            audio_track: InstalledTrackRestore::KeepDefault,
            subtitle_track: InstalledSubtitleRestore::KeepDefault,
            volume: crate::InstalledVolumeRestore::KeepCurrent,
            position: InstalledPositionRestore::SeekTo(Duration::from_secs(7)),
        },
        outcome_tx,
    );
    outcome_rx
}

#[test]
fn position_restore_applies_only_after_generic_seek_commit() {
    let (mut session, request_id, media_instance_id) = correlated_installed_session(Vec::new());
    let outcome_rx = begin_position_restore(&mut session, request_id, media_instance_id);

    assert_eq!(outcome_rx.try_recv(), Err(TryRecvError::Empty));
    let seek_commit = session
        .seek_runtime
        .active_commit()
        .expect("position restore должен оставить active seek commit");

    session.complete_seek_commit(seek_commit);

    assert_eq!(
        outcome_rx
            .recv()
            .expect("seek commit должен закрыть restore"),
        InstalledMediaStateRestoreOutcome::Applied { media_instance_id }
    );
    assert_eq!(session.playback_state(), PlaybackState::Paused);
}

#[test]
fn cancelled_video_seek_never_publishes_false_restore_applied() {
    let video_track = fake_track(1, TrackKind::Video);
    let (mut session, request_id, media_instance_id) =
        correlated_installed_session(vec![video_track]);
    session
        .pipeline
        .set_video_decoder_thread(SharedFakeVideoDecoderThread::new());
    let outcome_rx = begin_position_restore(&mut session, request_id, media_instance_id);

    assert_eq!(outcome_rx.try_recv(), Err(TryRecvError::Empty));
    assert!(
        session.seek_runtime.seek_landing_active(),
        "video restore должен ждать codec-neutral SeekLanding terminal"
    );

    session
        .dispatch_command(PlayerCommand::Pause)
        .expect("pause должна безопасно отменить незавершённый video seek");

    assert!(matches!(
        outcome_rx
            .recv()
            .expect("cancelled restore обязан получить terminal failure"),
        InstalledMediaStateRestoreOutcome::Failed {
            stage: InstalledMediaRestoreFailureStage::Position,
            error,
        } if error.kind == PlayerErrorKind::SeekUnavailable
    ));
}

#[test]
fn fatal_seek_error_is_forwarded_to_pending_restore_receipt() {
    let (mut session, request_id, media_instance_id) = correlated_installed_session(Vec::new());
    let outcome_rx = begin_position_restore(&mut session, request_id, media_instance_id);
    let expected_error = PlayerError::new(
        PlayerErrorKind::DemuxError,
        "synthetic codec-neutral demux failure",
    );

    session.mark_fatal_error(expected_error.clone());

    assert_eq!(
        outcome_rx
            .recv()
            .expect("fatal seek failure должен закрыть restore receipt"),
        InstalledMediaStateRestoreOutcome::Failed {
            stage: InstalledMediaRestoreFailureStage::Position,
            error: expected_error,
        }
    );
}

#[test]
fn media_replacement_makes_pending_restore_stale() {
    let (mut session, request_id, media_instance_id) = correlated_installed_session(Vec::new());
    let outcome_rx = begin_position_restore(&mut session, request_id, media_instance_id);
    session.snapshot.media_instance_id = Some(MediaInstanceId::from_non_zero(
        NonZeroU64::new(903).expect("replacement media instance id is non-zero"),
    ));

    session.reconcile_pending_seek_receipt_identities();

    assert_eq!(
        outcome_rx
            .recv()
            .expect("stale restore должен получить terminal outcome"),
        InstalledMediaStateRestoreOutcome::StaleInstance
    );
}
