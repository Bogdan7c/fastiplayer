//! Решение владельца 233 на настоящем потоке worker-а: перемотка эфира за окно — мягкий
//! отказ команды. Корень дефекта был ровно здесь: `dispatch_player_command` получал ошибку
//! приёма перемотки и звал `mark_fatal_error` → `Failed` + `FatalError`, и браузерный режим
//! отвергал поколение (`stream_rejected{playback_failed}`).

use super::*;

/// Эфир, у которого следующий пакет ещё не вышел: demux честно просит подождать.
///
/// `EndOfStream` общего fake-а завершил бы воспроизведение (`Ended`), а эфир не кончается.
struct WaitingLiveDemuxer;

impl Demuxer for WaitingLiveDemuxer {
    fn tracks(&self) -> &[media_core::TrackInfo] {
        &[]
    }

    fn duration(&self) -> Option<Duration> {
        None
    }

    fn seekability(&self) -> DemuxSeekability {
        DemuxSeekability::Seekable
    }

    fn next_event(&mut self) -> anyhow::Result<media_core::DemuxReadEvent> {
        Ok(media_core::DemuxReadEvent::TemporarilyUnavailable(
            media_core::DemuxRetryHint::new(Duration::from_millis(20))?,
        ))
    }

    fn seek(&mut self, timestamp: Duration) -> anyhow::Result<DemuxSeekResult> {
        Ok(DemuxSeekResult {
            requested_position: MediaTime::from_duration(timestamp),
            actual_position: MediaTime::from_duration(timestamp),
            actual_track_timestamp: None,
        })
    }
}

/// Окно эфира `[20 с, 60 с]`, край 60 с.
fn live_media_with_dvr_window(generation: u64) -> PreparedMedia {
    let window = media_core::TimelineRange::new(
        media_core::MediaTime::from_secs(20),
        media_core::MediaTime::from_secs(60),
    )
    .expect("ordered DVR window");
    let (port, publisher) =
        media_core::dynamic_media_timeline(media_core::DynamicMediaTimelineInitial {
            port_generation: media_core::DynamicMediaTimelinePortGeneration::new(
                NonZeroU64::new(generation).expect("port generation"),
            ),
            source_epoch: media_core::DynamicMediaTimelineEpoch::new(1),
            state: media_core::DynamicMediaTimelineState::with_dvr(
                media_core::MediaTime::from_secs(60),
                window,
            )
            .expect("DVR state")
            .into(),
        });
    // Publisher живёт столько же, сколько тест: окно не отключается посреди проверки.
    std::mem::forget(publisher);
    PreparedMedia::from_external_label("worker-live-refusal", Box::new(WaitingLiveDemuxer))
        .with_dynamic_timeline(port)
        .expect("duration-less media accepts a live timeline")
}

/// Ошибки player-а среди событий worker-а: `(фатальная?, вид)`.
fn player_errors(events: &[PlayerWorkerEvent]) -> Vec<(bool, PlayerErrorKind)> {
    events
        .iter()
        .filter_map(|event| match event {
            PlayerWorkerEvent::Player(CorrelatedPlayerEvent {
                event: PlayerEvent::RecoverableError(error),
                ..
            }) => Some((false, error.kind.clone())),
            PlayerWorkerEvent::Player(CorrelatedPlayerEvent {
                event: PlayerEvent::FatalError(error),
                ..
            }) => Some((true, error.kind.clone())),
            _ => None,
        })
        .collect()
}

#[test]
fn worker_keeps_its_playback_state_after_a_seek_behind_the_live_window() {
    let mut worker = PlayerWorker::spawn(worker_config_for_tests()).expect("worker starts");
    let _receipt = worker
        .load_prepared_media(live_media_with_dvr_window(81), false)
        .expect("live install accepted");
    let _installed = wait_for_snapshot(&mut worker, |snapshot| {
        snapshot.timeline.live_revision.is_some()
    });
    worker
        .try_send_command(PlayerCommand::Play)
        .expect("Play sent");
    // Пакетов эфира ещё нет, поэтому намерение «играть» видно как `Buffering`.
    let playing = wait_for_snapshot(&mut worker, |snapshot| {
        snapshot.playback_state == PlaybackState::Buffering
    });
    let _events_before_seek = worker.drain_events();

    worker
        .try_send_command(PlayerCommand::Seek(SeekRequest::absolute(
            media_core::MediaTime::from_secs(5),
        )))
        .expect("Seek sent");
    let events = drain_events_until(&worker, |events| !player_errors(events).is_empty());

    assert_eq!(
        player_errors(&events),
        vec![(false, PlayerErrorKind::SeekTargetExpired)],
        "the refusal reaches the viewer as one recoverable notice, never as FatalError"
    );
    // Плеер не ушёл в `Failed`: после отказа он в том же состоянии, что до команды, и на
    // том же месте (часы идущего воспроизведения проверяют тесты session).
    thread::sleep(Duration::from_millis(100));
    let later = worker.latest_snapshot();
    assert_eq!(later.playback_state, playing.playback_state);
    assert!(
        later.current_position >= playing.current_position,
        "the refused seek must not move playback: {:?} -> {:?}",
        playing.current_position,
        later.current_position
    );
    assert_eq!(
        later.last_error.as_ref().map(|error| error.kind.clone()),
        Some(PlayerErrorKind::SeekTargetExpired)
    );
    let settled_events = worker.drain_events();
    assert!(
        player_errors(&settled_events)
            .iter()
            .all(|(fatal, _)| !fatal),
        "no late FatalError: {settled_events:?}"
    );

    worker.shutdown().expect("worker shutdown");
}
