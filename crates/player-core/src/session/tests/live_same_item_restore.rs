//! Focused S35S proofs для neutral dynamic live same-item restore.

use std::num::NonZeroU64;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use crossbeam_channel::{TryRecvError, bounded};
use media_core::{
    DemuxReadEvent, DemuxSeekRequest, DemuxSeekResult, DemuxSeekability, Demuxer,
    DynamicMediaTimelineEpoch, DynamicMediaTimelineInitial, DynamicMediaTimelinePortGeneration,
    DynamicMediaTimelinePublishError, DynamicMediaTimelinePublisher, DynamicMediaTimelineState,
    MediaTime, TimelineRange, dynamic_media_timeline,
};

use super::test_support::FakeDemuxer;
use super::*;
use crate::media_install::AcceptedPlaybackIntent;
use crate::{
    InstalledLiveEdgeAdjustmentReason, InstalledMediaStateRestore,
    InstalledMediaStateRestoreOutcome, InstalledPositionRestore, InstalledSubtitleRestore,
    InstalledTrackRestore, InstalledVolumeRestore, MediaInstallRequestId, MediaInstanceId,
    PlaybackIntent, PlaybackIntentRevision, PlaybackState, PreparedMedia,
};

/// Neutral fake provider resource отмечает каждый реальный drop demux owner-а.
struct DropTrackedLiveDemuxer {
    inner: FakeDemuxer,
    drop_count: Arc<AtomicUsize>,
}

impl Drop for DropTrackedLiveDemuxer {
    fn drop(&mut self) {
        self.drop_count.fetch_add(1, Ordering::SeqCst);
    }
}

impl Demuxer for DropTrackedLiveDemuxer {
    fn tracks(&self) -> &[media_core::TrackInfo] {
        self.inner.tracks()
    }

    fn duration(&self) -> Option<Duration> {
        self.inner.duration()
    }

    fn seekability(&self) -> DemuxSeekability {
        self.inner.seekability()
    }

    fn next_event(&mut self) -> anyhow::Result<DemuxReadEvent> {
        self.inner.next_event()
    }

    fn seek(&mut self, timestamp: Duration) -> anyhow::Result<DemuxSeekResult> {
        self.inner.seek(timestamp)
    }

    fn seek_with_request(&mut self, request: DemuxSeekRequest) -> anyhow::Result<DemuxSeekResult> {
        self.inner.seek_with_request(request)
    }
}

fn generation(value: u64) -> DynamicMediaTimelinePortGeneration {
    DynamicMediaTimelinePortGeneration::new(
        NonZeroU64::new(value).expect("test generation is non-zero"),
    )
}

fn dvr_state(start: u64, end: u64, live_edge: u64) -> DynamicMediaTimelineState {
    DynamicMediaTimelineState::with_dvr(
        MediaTime::from_secs(live_edge),
        TimelineRange::new(MediaTime::from_secs(start), MediaTime::from_secs(end))
            .expect("focused DVR range is non-empty"),
    )
    .expect("DVR end does not exceed live edge")
}

fn fake_live_media(
    generation_value: u64,
    initial_state: DynamicMediaTimelineState,
    drop_count: Arc<AtomicUsize>,
) -> (PreparedMedia, DynamicMediaTimelinePublisher) {
    let (port, publisher) = dynamic_media_timeline(DynamicMediaTimelineInitial {
        port_generation: generation(generation_value),
        source_epoch: DynamicMediaTimelineEpoch::new(1),
        state: initial_state,
    });
    let inner = FakeDemuxer::new(Vec::new(), None, Arc::new(Mutex::new(Vec::new())));
    let demuxer = DropTrackedLiveDemuxer { inner, drop_count };
    let prepared_media = PreparedMedia::from_external_label("fake-live", Box::new(demuxer))
        .with_dynamic_timeline(port)
        .expect("duration-less fake accepts live timeline");
    (prepared_media, publisher)
}

fn install_correlated_live(
    session: &mut PlayerSession,
    prepared_media: PreparedMedia,
    autoplay: bool,
    request_value: u64,
) -> (MediaInstallRequestId, MediaInstanceId) {
    session.load_prepared_media_with_autoplay(prepared_media, autoplay);
    let media_instance_id = session
        .snapshot()
        .media_instance_id
        .expect("live media instance is installed");
    let request_id = MediaInstallRequestId::from_non_zero(
        NonZeroU64::new(request_value).expect("test request id is non-zero"),
    );
    session.playback_intent_control.register_staged_request(
        request_id,
        AcceptedPlaybackIntent {
            revision: PlaybackIntentRevision::INITIAL,
            intent: if autoplay {
                PlaybackIntent::StartPlaying
            } else {
                PlaybackIntent::StartPaused
            },
        },
    );
    session
        .playback_intent_control
        .commit_staged_request(request_id, media_instance_id, |_| {});
    (request_id, media_instance_id)
}

fn begin_live_restore(
    session: &mut PlayerSession,
    request_id: MediaInstallRequestId,
    media_instance_id: MediaInstanceId,
    previous_absolute_position: Duration,
) -> crossbeam_channel::Receiver<InstalledMediaStateRestoreOutcome> {
    let (outcome_tx, outcome_rx) = bounded(1);
    session.begin_installed_media_state_restore(
        InstalledMediaStateRestore {
            request_id,
            media_instance_id,
            video_track: InstalledTrackRestore::KeepDefault,
            audio_track: InstalledTrackRestore::KeepDefault,
            subtitle_track: InstalledSubtitleRestore::KeepDefault,
            volume: InstalledVolumeRestore::KeepCurrent,
            position: InstalledPositionRestore::RestoreLiveSameItemPosition {
                previous_absolute_position,
            },
        },
        outcome_tx,
    );
    outcome_rx
}

#[test]
fn retained_dvr_position_uses_existing_seek_lifecycle_for_playing_and_paused() {
    for (autoplay, expected_state) in [
        (true, PlaybackState::Playing),
        (false, PlaybackState::Paused),
    ] {
        let drop_count = Arc::new(AtomicUsize::new(0));
        let (prepared_media, publisher) =
            fake_live_media(10, dvr_state(10, 50, 50), Arc::clone(&drop_count));
        publisher
            .publish(DynamicMediaTimelineEpoch::new(2), dvr_state(20, 70, 70))
            .expect("fresh prepare-time DVR range is published");
        let mut session = PlayerSession::new();
        let (request_id, media_instance_id) =
            install_correlated_live(&mut session, prepared_media, autoplay, 101);
        publisher
            .publish(DynamicMediaTimelineEpoch::new(3), dvr_state(25, 80, 80))
            .expect("fresh post-install DVR range is published");

        let outcome_rx = begin_live_restore(
            &mut session,
            request_id,
            media_instance_id,
            Duration::from_secs(30),
        );

        assert_eq!(outcome_rx.try_recv(), Err(TryRecvError::Empty));
        let seek_commit = session
            .seek_runtime
            .active_commit()
            .expect("retained DVR target uses the existing seek transaction");
        session.complete_seek_commit(seek_commit);
        assert_eq!(
            outcome_rx.recv().expect("retained restore outcome"),
            InstalledMediaStateRestoreOutcome::Applied { media_instance_id }
        );
        assert_eq!(session.snapshot().current_position, Duration::from_secs(30));
        assert_eq!(session.playback_state(), expected_state);
        drop(session);
        assert_eq!(drop_count.load(Ordering::SeqCst), 1);
    }
}

#[test]
fn expired_dvr_position_adjusts_to_fresh_edge_without_starting_seek() {
    let drop_count = Arc::new(AtomicUsize::new(0));
    let (prepared_media, publisher) =
        fake_live_media(20, dvr_state(10, 50, 50), Arc::clone(&drop_count));
    let fresh_range =
        TimelineRange::new(MediaTime::from_secs(40), MediaTime::from_secs(90)).unwrap();
    publisher
        .publish(
            DynamicMediaTimelineEpoch::new(2),
            DynamicMediaTimelineState::with_dvr(MediaTime::from_secs(90), fresh_range).unwrap(),
        )
        .expect("prepare-time expiry is published");
    let mut session = PlayerSession::new();
    let (request_id, media_instance_id) =
        install_correlated_live(&mut session, prepared_media, false, 201);

    let outcome_rx = begin_live_restore(
        &mut session,
        request_id,
        media_instance_id,
        Duration::from_secs(30),
    );

    assert_eq!(
        outcome_rx.recv().expect("explicit live-edge outcome"),
        InstalledMediaStateRestoreOutcome::AdjustedToLiveEdge {
            media_instance_id,
            requested_position: Duration::from_secs(30),
            live_edge: Duration::from_secs(90),
            reason: InstalledLiveEdgeAdjustmentReason::PreviousPositionOutsideDvr {
                available_range: fresh_range,
            },
        }
    );
    assert!(session.seek_runtime.active_commit().is_none());
    assert_eq!(session.snapshot().current_position, Duration::from_secs(90));
}

#[test]
fn no_dvr_switch_always_opens_on_fresh_provider_edge() {
    let drop_count = Arc::new(AtomicUsize::new(0));
    let (prepared_media, publisher) = fake_live_media(
        30,
        DynamicMediaTimelineState::without_dvr(MediaTime::from_secs(100)),
        drop_count,
    );
    let mut session = PlayerSession::new();
    let (request_id, media_instance_id) =
        install_correlated_live(&mut session, prepared_media, true, 301);
    publisher
        .publish(
            DynamicMediaTimelineEpoch::new(2),
            DynamicMediaTimelineState::without_dvr(MediaTime::from_secs(125)),
        )
        .expect("fresh no-DVR edge is published");

    let outcome_rx = begin_live_restore(
        &mut session,
        request_id,
        media_instance_id,
        Duration::from_secs(100),
    );

    assert_eq!(
        outcome_rx.recv().expect("explicit no-DVR adjustment"),
        InstalledMediaStateRestoreOutcome::AdjustedToLiveEdge {
            media_instance_id,
            requested_position: Duration::from_secs(100),
            live_edge: Duration::from_secs(125),
            reason: InstalledLiveEdgeAdjustmentReason::DvrWindowUnavailable,
        }
    );
    assert_eq!(
        session.snapshot().current_position,
        Duration::from_secs(125)
    );
    assert!(session.seek_runtime.active_commit().is_none());
}

#[test]
fn replacement_isolates_generations_and_releases_old_port_and_resource_once() {
    let old_drop_count = Arc::new(AtomicUsize::new(0));
    let new_drop_count = Arc::new(AtomicUsize::new(0));
    let (old_media, old_publisher) = fake_live_media(
        40,
        DynamicMediaTimelineState::without_dvr(MediaTime::from_secs(50)),
        Arc::clone(&old_drop_count),
    );
    let (new_media, new_publisher) = fake_live_media(
        41,
        DynamicMediaTimelineState::without_dvr(MediaTime::from_secs(200)),
        Arc::clone(&new_drop_count),
    );
    let mut session = PlayerSession::new();
    let (_old_request_id, old_instance_id) =
        install_correlated_live(&mut session, old_media, false, 401);
    let (new_request_id, new_instance_id) =
        install_correlated_live(&mut session, new_media, false, 402);

    assert_eq!(old_drop_count.load(Ordering::SeqCst), 1);
    assert_eq!(new_drop_count.load(Ordering::SeqCst), 0);
    assert_eq!(
        old_publisher.publish(
            DynamicMediaTimelineEpoch::new(2),
            DynamicMediaTimelineState::without_dvr(MediaTime::from_secs(999)),
        ),
        Err(DynamicMediaTimelinePublishError::ConsumerDisconnected)
    );
    new_publisher
        .publish(
            DynamicMediaTimelineEpoch::new(2),
            DynamicMediaTimelineState::without_dvr(MediaTime::from_secs(220)),
        )
        .expect("new generation remains connected");

    let stale_outcome = begin_live_restore(
        &mut session,
        new_request_id,
        old_instance_id,
        Duration::from_secs(50),
    );
    assert_eq!(
        stale_outcome.recv().expect("stale instance outcome"),
        InstalledMediaStateRestoreOutcome::StaleInstance
    );
    let current_outcome = begin_live_restore(
        &mut session,
        new_request_id,
        new_instance_id,
        Duration::from_secs(50),
    );
    assert!(matches!(
        current_outcome.recv().expect("new generation outcome"),
        InstalledMediaStateRestoreOutcome::AdjustedToLiveEdge {
            media_instance_id,
            live_edge,
            ..
        } if media_instance_id == new_instance_id && live_edge == Duration::from_secs(220)
    ));

    drop(session);
    assert_eq!(old_drop_count.load(Ordering::SeqCst), 1);
    assert_eq!(new_drop_count.load(Ordering::SeqCst), 1);
}

#[test]
fn cancelled_pre_barrier_candidate_releases_only_new_generation_once() {
    let old_drop_count = Arc::new(AtomicUsize::new(0));
    let cancelled_drop_count = Arc::new(AtomicUsize::new(0));
    let (old_media, old_publisher) = fake_live_media(
        50,
        DynamicMediaTimelineState::without_dvr(MediaTime::from_secs(70)),
        Arc::clone(&old_drop_count),
    );
    let (cancelled_media, cancelled_publisher) = fake_live_media(
        51,
        DynamicMediaTimelineState::without_dvr(MediaTime::from_secs(300)),
        Arc::clone(&cancelled_drop_count),
    );
    let mut session = PlayerSession::new();
    let (_old_request_id, old_instance_id) =
        install_correlated_live(&mut session, old_media, true, 501);
    let old_playback_state = session.playback_state();

    drop(cancelled_media);

    assert_eq!(cancelled_drop_count.load(Ordering::SeqCst), 1);
    assert_eq!(old_drop_count.load(Ordering::SeqCst), 0);
    assert_eq!(
        cancelled_publisher.publish(
            DynamicMediaTimelineEpoch::new(2),
            DynamicMediaTimelineState::without_dvr(MediaTime::from_secs(320)),
        ),
        Err(DynamicMediaTimelinePublishError::ConsumerDisconnected)
    );
    old_publisher
        .publish(
            DynamicMediaTimelineEpoch::new(2),
            DynamicMediaTimelineState::without_dvr(MediaTime::from_secs(90)),
        )
        .expect("old playback remains connected before commit");
    session.refresh_dynamic_timeline();

    assert_eq!(session.snapshot().media_instance_id, Some(old_instance_id));
    assert_eq!(
        session.snapshot().timeline.live_edge,
        Some(MediaTime::from_secs(90))
    );
    assert_eq!(session.snapshot().current_position, Duration::from_secs(70));
    assert_eq!(session.playback_state(), old_playback_state);

    drop(session);
    assert_eq!(old_drop_count.load(Ordering::SeqCst), 1);
    assert_eq!(cancelled_drop_count.load(Ordering::SeqCst), 1);
}
