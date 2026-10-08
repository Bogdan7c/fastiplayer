//! Граница `into_player_demuxer`: фоновый поток, seek port и типизированные отказы.

use std::thread;
use std::time::{Duration, Instant};

use demux_api::ProgressiveDemuxStartupError;
use media_core::{
    DemuxReadEvent, DemuxSeekRequest, DemuxSeekResult, DemuxSeekability, MediaTime, Packet,
    TimelineNotSeekableReason, TrackId, TrackInfo, TrackKind,
};
use player_core::{PreparedDemuxSeekOutcome, PreparedDemuxSeekRequestId};

use super::*;

/// Каждое чтение «ждёт сеть» 300 мс, затем отдаёт packet с текущей позиции.
struct SlowNetworkDemuxer {
    seekability: DemuxSeekability,
    position: Duration,
}

impl SlowNetworkDemuxer {
    const READ_STALL: Duration = Duration::from_millis(300);

    fn seekable() -> Self {
        Self {
            seekability: DemuxSeekability::Seekable,
            position: Duration::ZERO,
        }
    }

    fn forward_only() -> Self {
        Self {
            seekability: DemuxSeekability::NotSeekable {
                reason: TimelineNotSeekableReason::UnknownTimeline,
            },
            position: Duration::ZERO,
        }
    }
}

impl Demuxer for SlowNetworkDemuxer {
    fn tracks(&self) -> &[TrackInfo] {
        &[]
    }

    fn duration(&self) -> Option<Duration> {
        Some(Duration::from_secs(60))
    }

    fn seekability(&self) -> DemuxSeekability {
        self.seekability
    }

    fn next_event(&mut self) -> anyhow::Result<DemuxReadEvent> {
        thread::sleep(Self::READ_STALL);
        let packet = Packet::new_unbounded(
            TrackId::new(1),
            TrackKind::Audio,
            self.position,
            None,
            true,
            vec![0x01_u8].into(),
        );
        self.position += Duration::from_millis(20);
        Ok(DemuxReadEvent::Packet(packet))
    }

    fn seek(&mut self, timestamp: Duration) -> anyhow::Result<DemuxSeekResult> {
        self.position = timestamp;
        Ok(DemuxSeekResult {
            requested_position: MediaTime::from_duration(timestamp),
            actual_position: MediaTime::from_duration(timestamp),
            actual_track_timestamp: None,
        })
    }
}

fn prefetch_config() -> media_prefetch::PrefetchConfig {
    media_prefetch::PrefetchConfig::new(64 * 1024, 1024 * 1024, 4 * 1024 * 1024)
        .expect("test prefetch config")
}

/// Ждёт packet, проверяя, что ни один вызов `next_event` не блокирует caller-а.
fn next_packet_without_blocking(demuxer: &mut dyn Demuxer) -> Packet {
    let deadline = Instant::now() + Duration::from_secs(5);
    loop {
        let started = Instant::now();
        let event = demuxer.next_event().expect("fake не падает");
        assert!(
            started.elapsed() < Duration::from_millis(100),
            "next_event заблокировал поток player-а на {:?}",
            started.elapsed()
        );
        match event {
            DemuxReadEvent::Packet(packet) => return packet,
            DemuxReadEvent::TemporarilyUnavailable(hint) => thread::sleep(hint.retry_after()),
            other => panic!("неожиданное событие: {other:?}"),
        }
        assert!(Instant::now() < deadline, "packet так и не пришёл");
    }
}

/// Seekable вход: player получает seek port, медленное чтение уходит в фон, а
/// перемотка через порт даёт успешную квитанцию и пакеты с новой позиции.
#[test]
fn seekable_input_gets_seek_port_and_never_blocks_player_thread() {
    let mut player_demuxer = into_player_demuxer(
        Box::new(SlowNetworkDemuxer::seekable()),
        TransportSeekability::Seekable,
        CancellationToken::new(),
        prefetch_config(),
    )
    .expect("seekable runtime стартует");
    let seek_port = player_demuxer
        .seek_port
        .take()
        .expect("seekable вход отдаёт seek port");

    assert_eq!(
        player_demuxer.demuxer.seekability(),
        DemuxSeekability::Seekable
    );
    let first = next_packet_without_blocking(player_demuxer.demuxer.as_mut());
    assert_eq!(first.pts, Duration::ZERO);

    let request_id = PreparedDemuxSeekRequestId::new(1);
    seek_port
        .enqueue_seek(
            request_id,
            DemuxSeekRequest::accurate(Duration::from_secs(5)),
        )
        .expect("seek принят в очередь worker-а");
    let deadline = Instant::now() + Duration::from_secs(5);
    let receipt = loop {
        if let Some(receipt) = seek_port.poll_seek_receipt() {
            break receipt;
        }
        assert!(Instant::now() < deadline, "seek receipt не пришёл");
        thread::sleep(Duration::from_millis(1));
    };
    assert_eq!(receipt.request_id, request_id);
    assert!(matches!(
        receipt.outcome,
        PreparedDemuxSeekOutcome::Succeeded(result)
            if result.requested_position.as_duration() == Duration::from_secs(5)
    ));
    let after_seek = next_packet_without_blocking(player_demuxer.demuxer.as_mut());
    assert_eq!(after_seek.pts, Duration::from_secs(5));
}

/// Forward-only вход (HTTP 200 без Range): seek port не выдаётся, перемотка
/// честно отклоняется, чтение тоже не блокирует player.
#[test]
fn streaming_input_keeps_forward_only_contract_without_seek_port() {
    let mut player_demuxer = into_player_demuxer(
        Box::new(SlowNetworkDemuxer::forward_only()),
        TransportSeekability::Streaming,
        CancellationToken::new(),
        prefetch_config(),
    )
    .expect("forward-only runtime стартует");

    assert!(player_demuxer.seek_port.is_none());
    assert!(matches!(
        player_demuxer.demuxer.seekability(),
        DemuxSeekability::NotSeekable { .. }
    ));
    assert!(player_demuxer.demuxer.seek(Duration::from_secs(1)).is_err());
    assert_eq!(
        next_packet_without_blocking(player_demuxer.demuxer.as_mut()).pts,
        Duration::ZERO
    );
}

/// Транспорт сказал «seekable», а контейнер перематываться не умеет: типизированный
/// отказ старта, а не молчаливая подмена на forward-only.
#[test]
fn seekable_transport_with_non_seekable_container_is_typed_startup_error() {
    let Err(error) = into_player_demuxer(
        Box::new(SlowNetworkDemuxer::forward_only()),
        TransportSeekability::Seekable,
        CancellationToken::new(),
        prefetch_config(),
    ) else {
        panic!("несогласованная seekability должна отклоняться");
    };

    assert!(
        error.chain().any(|cause| matches!(
            cause.downcast_ref::<ProgressiveDemuxStartupError>(),
            Some(ProgressiveDemuxStartupError::SeekableInputRequired)
        )),
        "{error:#}"
    );
}

/// Мгновенный источник пакетов: очередь фонового demuxer-а наполняется сразу.
struct InstantDemuxer {
    next_pts: Duration,
}

impl Demuxer for InstantDemuxer {
    fn tracks(&self) -> &[TrackInfo] {
        &[]
    }

    fn duration(&self) -> Option<Duration> {
        Some(Duration::from_secs(60))
    }

    fn next_event(&mut self) -> anyhow::Result<DemuxReadEvent> {
        let packet = Packet::new_unbounded(
            TrackId::new(1),
            TrackKind::Audio,
            self.next_pts,
            None,
            true,
            vec![0x01_u8].into(),
        );
        self.next_pts += Duration::from_millis(20);
        Ok(DemuxReadEvent::Packet(packet))
    }

    fn seek(&mut self, timestamp: Duration) -> anyhow::Result<DemuxSeekResult> {
        self.next_pts = timestamp;
        Ok(DemuxSeekResult {
            requested_position: MediaTime::from_duration(timestamp),
            actual_position: MediaTime::from_duration(timestamp),
            actual_track_timestamp: None,
        })
    }
}

/// Регресс ручного прогона сессии 16: при `read_ahead_mb = prefetch_chunk_mb`
/// очередь была на 1 пакет, player почти каждый проход видел «данных пока нет» и
/// не выходил из Buffering. Очередь должна копить пачку пакетов независимо от
/// соотношения окна и куска prefetch-а.
#[test]
fn tiny_prefetch_window_still_queues_a_batch_of_packets() {
    let window_equals_chunk =
        media_prefetch::PrefetchConfig::new(64 * 1024, 1024 * 1024, 1024 * 1024)
            .expect("минимальное окно из config-а");
    let mut player_demuxer = into_player_demuxer(
        Box::new(InstantDemuxer {
            next_pts: Duration::ZERO,
        }),
        TransportSeekability::Seekable,
        CancellationToken::new(),
        window_equals_chunk,
    )
    .expect("seekable runtime стартует");
    // Дать worker-у наполнить очередь.
    thread::sleep(Duration::from_millis(200));

    let consecutive_packets = (0..64)
        .take_while(|_| {
            matches!(
                player_demuxer.demuxer.next_event(),
                Ok(DemuxReadEvent::Packet(_))
            )
        })
        .count();

    assert_eq!(
        consecutive_packets, 64,
        "player должен получить пачку пакетов подряд, без TemporarilyUnavailable"
    );
}
