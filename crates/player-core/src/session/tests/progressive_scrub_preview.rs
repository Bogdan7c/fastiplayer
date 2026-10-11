//! Регрессия G1: клик по таймлайну progressive HTTP-видео не должен давать ложный тост
//! «Перемотка недоступна», пока финальный seek успешен.
//!
//! У progressive HTTP финальный seek идёт через worker-receipted port, а собственный
//! demuxer плеера не умеет синхронный seek для live-scrub preview. Последовательность
//! команд клика — `BeginScrub` → `PreviewScrub` → `EndScrub`.

use std::sync::{Arc, Mutex};
use std::time::Duration;

use media_core::{MediaDemuxError, TimelineNotSeekableReason};

use super::prepared_demux_seek::FakePreparedDemuxSeekPort;
use super::test_support::fake_track;
use super::*;
use crate::{
    PlaybackState, PlayerError, PlayerErrorKind, PlayerEvent, PreparedDemuxSeekLandingPolicy,
    PreparedDemuxSeekMode, PreparedDemuxSeekOutcome, PreparedDemuxSeekPort, ScrubCommitPolicy,
};

/// Demuxer, который отказывает в синхронном seek-е, как progressive HTTP без preview-controller-а.
struct SyncSeekRefusingDemuxer {
    tracks: Vec<TrackInfo>,
    seekability: DemuxSeekability,
    seek_attempts: Arc<Mutex<usize>>,
}

impl Demuxer for SyncSeekRefusingDemuxer {
    fn tracks(&self) -> &[TrackInfo] {
        &self.tracks
    }

    fn duration(&self) -> Option<Duration> {
        Some(Duration::from_secs(30))
    }

    fn seekability(&self) -> DemuxSeekability {
        self.seekability
    }

    fn next_event(&mut self) -> anyhow::Result<DemuxReadEvent> {
        Ok(media_core::finite_packet_read_event(None))
    }

    fn seek(&mut self, _timestamp: Duration) -> anyhow::Result<media_core::DemuxSeekResult> {
        Err(MediaDemuxError::SeekUnavailable {
            reason: "progressive HTTP source не поддерживает byte seek".to_owned(),
        }
        .into())
    }

    fn seek_with_request(
        &mut self,
        _request: media_core::DemuxSeekRequest,
    ) -> anyhow::Result<media_core::DemuxSeekResult> {
        *self.seek_attempts.lock().expect("seek attempts lock") += 1;
        Err(MediaDemuxError::SeekUnavailable {
            reason: "progressive HTTP source не поддерживает byte seek".to_owned(),
        }
        .into())
    }
}

/// Сессия с video-треком, demuxer-ом без sync seek-а и (опционально) worker-receipted port-ом.
fn session_with_sync_seek_refusing_demuxer(
    seekability: DemuxSeekability,
    port: Option<Arc<FakePreparedDemuxSeekPort>>,
) -> (PlayerSession, Arc<Mutex<usize>>) {
    let tracks = vec![fake_track(1, TrackKind::Video)];
    let seek_attempts = Arc::new(Mutex::new(0));
    let mut session = PlayerSession::new();
    session.pipeline.install_opened_media(
        Box::new(SyncSeekRefusingDemuxer {
            tracks: tracks.clone(),
            seekability,
            seek_attempts: Arc::clone(&seek_attempts),
        }),
        None,
        None,
        tracks.clone(),
    );
    session
        .select_default_video_track(&tracks, "fake media не содержит video track")
        .expect("fake video track должен получить decode requirement");
    session.set_snapshot_duration(Some(Duration::from_secs(30)));
    session.apply_demux_seekability(seekability);
    session.set_playback_state(PlaybackState::Playing);
    if let Some(port) = port {
        let erased: Arc<dyn PreparedDemuxSeekPort> = port;
        session
            .prepared_demux_seek
            .install(PreparedDemuxSeekMode::WorkerReceipted {
                port: erased,
                landing_policy: PreparedDemuxSeekLandingPolicy::DecodeForwardToTarget,
            });
    }
    let _ = session.take_events();
    (session, seek_attempts)
}

/// Выполняет клик по таймлайну: начало жеста, preview в цель, отпускание.
fn click_timeline(session: &mut PlayerSession, target_secs: u64) {
    session
        .dispatch_command(PlayerCommand::begin_scrub())
        .expect("BeginScrub");
    session
        .dispatch_command(PlayerCommand::preview_scrub(SeekRequest::absolute(
            MediaTime::from_secs(target_secs),
        )))
        .expect("PreviewScrub");
    session
        .dispatch_command(PlayerCommand::end_scrub(
            ScrubCommitPolicy::DEFAULT_TIMELINE_RELEASE,
        ))
        .expect("EndScrub");
}

fn recoverable_errors(events: Vec<PlayerEvent>) -> Vec<PlayerError> {
    events
        .into_iter()
        .filter_map(|event| match event {
            PlayerEvent::RecoverableError(error) => Some(error),
            _ => None,
        })
        .collect()
}

/// Клик по таймлайну progressive источника: preview без sync seek-а молчит,
/// финальный seek уходит worker-у и после receipt-а позиция перемещена.
#[test]
fn timeline_click_on_progressive_source_publishes_no_seek_error_and_seeks() {
    let port = Arc::new(FakePreparedDemuxSeekPort::default());
    let (mut session, seek_attempts) =
        session_with_sync_seek_refusing_demuxer(DemuxSeekability::Seekable, Some(port.clone()));

    click_timeline(&mut session, 22);

    assert_eq!(
        *seek_attempts.lock().expect("seek attempts lock"),
        1,
        "preview действительно упёрся в demuxer без sync seek-а (иначе тест ничего не доказывает)"
    );
    let commands = port.commands();
    let [(request_id, request)] = commands.as_slice() else {
        panic!("финальный seek должен уйти worker-у ровно одним запросом: {commands:?}");
    };
    assert_eq!(request.timestamp, Duration::from_secs(22));
    port.complete(
        *request_id,
        PreparedDemuxSeekOutcome::Succeeded(media_core::DemuxSeekResult {
            requested_position: MediaTime::from_secs(22),
            actual_position: MediaTime::from_secs(20),
            actual_track_timestamp: None,
        }),
    );
    session.service_prepared_demux_seek_receipts();

    assert_eq!(
        session
            .seek_commit()
            .expect("receipt должен запустить landing")
            .target_position,
        MediaTime::from_secs(22)
    );
    assert_eq!(session.snapshot().last_error, None);
    let errors = recoverable_errors(session.take_events());
    assert!(
        errors.is_empty(),
        "ложный отказ preview всплыл бы тостом: {errors:?}"
    );
}

/// Реально неперематываемый источник (live без окна) по-прежнему даёт видимый отказ.
#[test]
fn timeline_click_on_not_seekable_source_still_publishes_seek_unavailable() {
    let (mut session, _seek_attempts) = session_with_sync_seek_refusing_demuxer(
        DemuxSeekability::NotSeekable {
            reason: TimelineNotSeekableReason::SourceNotSeekable,
        },
        None,
    );

    click_timeline(&mut session, 22);

    let errors = recoverable_errors(session.take_events());
    assert!(
        errors
            .iter()
            .any(|error| error.kind == PlayerErrorKind::SeekUnavailable),
        "для неперематываемого источника отказ обязан дойти до пользователя: {errors:?}"
    );
}
