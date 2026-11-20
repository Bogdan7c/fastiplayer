//! Чтение демуксера не обгоняет квитанцию перемотки потока демуксера (16b3c3, бэклог п. 70).
//!
//! Порядок из зависания родного HLS на стенде моста. Поток демуксера (worker) публикует
//! квитанцию перемотки и сразу читает следующий элемент: маркер `TracksChanged` сегмента-якоря.
//! Если квитанция появилась после того, как такт её уже проверил, но до догоняющего чтения
//! (adaptive catch-up) того же такта, плеер раньше забирал маркер до квитанции. Маркер
//! сбрасывал конвейер с новым номером поколения, квитанция выбрасывалась как устаревшая, и
//! перемотку никто не завершал: `Seeking` навсегда, кадра цели нет, часы на нуле.

use std::collections::VecDeque;
use std::sync::{Arc, Mutex};
use std::time::Duration;

use media_core::{
    DemuxReadEvent, DemuxSeekRequest, DemuxSeekResult, DemuxTrackListUpdate, MediaTime,
    PacketKeyframe, TrackKind,
};

use super::test_support::{
    SeekRegressionHarness, fake_track, fake_video_packet_with_keyframe, scripted_seek_demuxer,
    seek_regression_fast_preroll_tick_config,
};
use crate::{
    PlaybackState, PlayerTickConfig, PlayerTickContext, PreparedDemuxSeekEnqueueError,
    PreparedDemuxSeekMode, PreparedDemuxSeekOutcome, PreparedDemuxSeekPort,
    PreparedDemuxSeekReceipt, PreparedDemuxSeekRequestId,
};

/// Длительность медиа в сценарии: та же, что ставит `SeekRegressionHarness`.
const MEDIA_DURATION: Duration = Duration::from_secs(30);

/// Сколько тактов даём перемотке, чтобы сесть на кадр цели (с запасом над нужными 2–3).
const LANDING_TICK_LIMIT: usize = 8;

/// Поддельный поток демуксера, который публикует квитанцию «сразу после опроса такта».
///
/// Настоящий worker публикует квитанцию в любой момент между тактами плеера. Здесь худший
/// момент закреплён: квитанция, переданная в `publish_after_next_poll`, становится видна
/// только после ближайшего опроса — то есть уже после шага `service_prepared_demux_seek_receipts`
/// того такта, в котором дальше идёт чтение демуксера.
#[derive(Default)]
struct LatePublishingSeekPort {
    /// Принятые запросы перемотки, чтобы тест знал номер своего запроса.
    commands: Mutex<Vec<(PreparedDemuxSeekRequestId, DemuxSeekRequest)>>,
    /// Квитанция, которую worker «доделывает» прямо сейчас: видна со следующего опроса.
    publishing: Mutex<Option<PreparedDemuxSeekReceipt>>,
    /// Опубликованные квитанции, которые опрос отдаёт по одной.
    published: Mutex<VecDeque<PreparedDemuxSeekReceipt>>,
}

impl LatePublishingSeekPort {
    /// Worker закончил перемотку: квитанция появится сразу после ближайшего опроса.
    fn publish_after_next_poll(
        &self,
        request_id: PreparedDemuxSeekRequestId,
        outcome: PreparedDemuxSeekOutcome,
    ) {
        *self.publishing.lock().expect("publishing receipt lock") =
            Some(PreparedDemuxSeekReceipt {
                request_id,
                outcome,
            });
    }

    /// Единственный принятый запрос перемотки сценария.
    fn only_request_id(&self) -> PreparedDemuxSeekRequestId {
        let commands = self.commands.lock().expect("seek command lock");
        let [(request_id, _request)] = commands.as_slice() else {
            panic!("the public seek must create exactly one worker request");
        };
        *request_id
    }
}

impl PreparedDemuxSeekPort for LatePublishingSeekPort {
    fn enqueue_seek(
        &self,
        request_id: PreparedDemuxSeekRequestId,
        request: DemuxSeekRequest,
    ) -> Result<(), PreparedDemuxSeekEnqueueError> {
        self.commands
            .lock()
            .expect("seek command lock")
            .push((request_id, request));
        Ok(())
    }

    fn poll_seek_receipt(&self) -> Option<PreparedDemuxSeekReceipt> {
        let mut published = self.published.lock().expect("published receipt lock");
        let visible_now = published.pop_front();
        // Квитанция «в работе» публикуется после этого опроса: такт её уже не увидит.
        if let Some(receipt) = self
            .publishing
            .lock()
            .expect("publishing receipt lock")
            .take()
        {
            published.push_back(receipt);
        }
        visible_now
    }
}

/// Такт плеера с включённым догоняющим чтением: именно оно читало демуксер в обход квитанции.
fn catch_up_tick_config() -> PlayerTickConfig {
    PlayerTickConfig {
        adaptive_catch_up_time_budget: Duration::from_millis(50),
        max_demux_packets_per_tick: 4,
        ..seek_regression_fast_preroll_tick_config()
    }
}

/// Один такт плеера с догоняющим чтением; возвращает число показанных кадров.
fn tick_with_catch_up(harness: &mut SeekRegressionHarness) -> u64 {
    harness
        .session
        .tick(PlayerTickContext::with_config(
            std::time::Instant::now(),
            catch_up_tick_config(),
        ))
        .video_frames_presented
}

#[test]
fn seek_lands_on_target_frame_when_the_receipt_arrives_after_the_tick_polled_it() {
    // Как у HLS стенда: цель 1245 мс, якорь сегмента 80 мс, кадр цели — 1280 мс.
    let target_position = Duration::from_millis(1_245);
    let anchor_position = Duration::from_millis(80);
    let target_frame_position = Duration::from_millis(1_280);
    let video_track = fake_track(1, TrackKind::Video);
    let mut demuxer = scripted_seek_demuxer(
        vec![video_track.clone()],
        target_position,
        anchor_position,
        vec![
            fake_video_packet_with_keyframe(
                video_track.id,
                anchor_position,
                PacketKeyframe::Keyframe,
            ),
            fake_video_packet_with_keyframe(
                video_track.id,
                target_frame_position,
                PacketKeyframe::NotKeyframe,
            ),
        ],
    );
    // Worker уже переставил демуксер: первым в очереди лежит маркер сегмента-якоря.
    demuxer.push_event(DemuxReadEvent::TracksChanged(DemuxTrackListUpdate {
        tracks: vec![video_track.clone()],
        duration: Some(MEDIA_DURATION),
    }));
    let mut harness = SeekRegressionHarness::new(vec![video_track], demuxer);
    let port = Arc::new(LatePublishingSeekPort::default());
    let erased_port: Arc<dyn PreparedDemuxSeekPort> = port.clone();
    harness
        .session
        .prepared_demux_seek
        .install(PreparedDemuxSeekMode::WorkerReceipted {
            port: erased_port,
            landing_policy: crate::PreparedDemuxSeekLandingPolicy::DecodeForwardToTarget,
        });
    harness
        .decoder
        .decode_next_packet_as_frame(anchor_position, 901);
    harness
        .decoder
        .decode_next_packet_as_frame(target_frame_position, 902);

    harness.start_final_seek(MediaTime::from_duration(target_position));
    let generation_of_the_seek = harness.session.pipeline.seek_generation();
    port.publish_after_next_poll(
        port.only_request_id(),
        PreparedDemuxSeekOutcome::Succeeded(DemuxSeekResult {
            requested_position: MediaTime::from_duration(target_position),
            actual_position: MediaTime::from_duration(anchor_position),
            actual_track_timestamp: None,
        }),
    );

    let mut presented_frames = 0;
    for _ in 0..LANDING_TICK_LIMIT {
        presented_frames += tick_with_catch_up(&mut harness);
        if harness.session.seek_commit().is_none() && presented_frames > 0 {
            break;
        }
    }

    // Маркер обработан как часть этой перемотки, а не как внешний сброс с новым поколением.
    assert_eq!(
        harness.session.pipeline.seek_generation(),
        generation_of_the_seek,
        "the anchor marker must not restart the pipeline under the pending seek"
    );
    assert!(
        harness.session.seek_commit().is_none(),
        "the seek must be committed, not left waiting for a dropped receipt"
    );
    assert_ne!(harness.session.playback_state(), PlaybackState::Seeking);
    assert!(
        presented_frames > 0,
        "the target frame must reach presentation"
    );
    assert_eq!(
        harness
            .session
            .pipeline
            .present_video_frame()
            .map(|frame| frame.pts),
        Some(target_frame_position)
    );
    assert_eq!(
        harness.session.snapshot().current_position,
        target_position,
        "the clock must stand at the seek target, not at zero"
    );
}
