//! Ignored-тест пропускной способности VA-API декодера H.264 (gles2-renderer, сессия 09a).
//!
//! Файл целиком проходит через production `VideoDecodeThread` без показа:
//! packet-ы подаются конвейером (несколько в полёте, как у плеера), каждый
//! опубликованный кадр сразу освобождается. Так меряется потолок самого
//! декодера — без рендера, очереди показа и часов воспроизведения.
//!
//! Что проверяется (результат, а не вызов):
//! 1. каждый отправленный access unit дал ровно один кадр со своим PTS
//!    (ни потерь, ни повторов при конвейерной подаче и EOF-drain);
//! 2. пул выходных VA surfaces остался ограниченным (`peak_live ≤ capacity`,
//!    surfaces не создаются на каждый кадр, нет «неизвестного состояния» на возврате).
//!
//! Скорость (кадры/с) печатается как доказательство замера. Порог в тесте не
//! задаётся: он зависит от машины и живёт в критерии сессии.
//!
//! Запуск: `scripts/media-regression.sh --scenario h264-mp4-vaapi-throughput --media <файл>`.

// Интеграционный тест целиком является тестовым кодом: unwrap/expect/panic
// здесь работают как assertions. Production-политика паник сюда не относится.
#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::panic,
    reason = "integration test crate: panics are test assertions"
)]

mod support;

use std::thread;
use std::time::{Duration, Instant};

use media_core::Packet;
use support::h264_media::{
    assert_bounded_pool, decode_packet, first_h264_video_track, h264_stream_config,
    microsecond_pts, next_video_packet, selected_media_path,
};
use symphonia_demux::SymphoniaDemuxer;
use video_core::{
    VideoDecoderEndOfStreamDrainResult, VideoDecoderEndOfStreamDrainState, VideoStreamConfigResult,
};
use video_vaapi::{DecodeThreadSendError, VideoDecodeThread, VideoDecodeThreadConfig};

/// Сколько packet-ов держим «в полёте» (отправлены, ACK ещё нет).
///
/// Столько же подавал одноразовый бенч сессии 00, по которому мерился потолок
/// «до». Значение меньше ёмкости packet-канала decoder thread (32), поэтому
/// отправка никогда не упирается в канал.
const PACKETS_IN_FLIGHT: usize = 8;

/// Если декодер не дал ни ACK, ни кадра за это время — он завис.
const DECODER_PROGRESS_TIMEOUT: Duration = Duration::from_secs(5);

/// Пауза опроса, когда декодеру ещё нечего отдать.
const POLL_INTERVAL: Duration = Duration::from_micros(200);

/// Generation проигрывания с начала файла.
const PLAYBACK_GENERATION: u64 = 1;

#[test]
#[ignore = "requires explicit local H.264 MP4 and VA-API H.264 hardware; use scripts/media-regression.sh"]
fn h264_vaapi_decodes_whole_file_pipelined_with_one_frame_per_packet() {
    let media_path = selected_media_path();
    let mut demuxer = SymphoniaDemuxer::from_file(&media_path)
        .unwrap_or_else(|error| panic!("open {}: {error}", media_path.display()));
    let video_track = first_h264_video_track(&demuxer);

    let decoder = VideoDecodeThread::new_with_config(VideoDecodeThreadConfig::from_env())
        .expect("VA-API decoder thread should start");
    let first_packet = next_video_packet(&mut demuxer, &video_track)
        .expect("H.264 track must contain at least one packet");
    assert_eq!(
        decoder.configure_stream(h264_stream_config(&video_track, &first_packet)),
        VideoStreamConfigResult::Configured,
        "VA-API must accept the selected H.264 stream"
    );

    let mut run = ThroughputRun::new(&decoder);
    let started_at = Instant::now();
    run.send_pipelined(first_packet);
    while let Some(packet) = next_video_packet(&mut demuxer, &video_track) {
        run.send_pipelined(packet);
    }
    run.wait_all_acks();
    run.drain_to_eof();
    let elapsed = started_at.elapsed();

    run.assert_every_packet_shown_exactly_once();
    let pool_stats = decoder.output_surface_pool_stats();
    assert_bounded_pool(&pool_stats, run.shown_frame_pts.len());

    let pending_stats = decoder.gpu_pending_frame_stats();
    assert!(
        pending_stats.completed_after_next_packet > 0,
        "pipelined decoding must submit the next packet while frames are still on the GPU: {pending_stats:?}"
    );
    assert_eq!(
        pending_stats.current_depth, 0,
        "EOF drain must leave no frame in flight: {pending_stats:?}"
    );

    let frames_per_second = run.shown_frame_pts.len() as f64 / elapsed.as_secs_f64();
    // Строку разбирают скрипты замера сессии 09a: формат не менять без них.
    println!(
        "vaapi_decode_throughput frames={} seconds={:.3} fps={:.1} pool_capacity={} pool_peak_live={} surfaces_created={} exhausted_lease_attempts={} in_flight_after_next_packet={} in_flight_already_ready={} in_flight_peak_depth={}",
        run.shown_frame_pts.len(),
        elapsed.as_secs_f64(),
        frames_per_second,
        pool_stats.capacity,
        pool_stats.peak_live_surfaces,
        pool_stats.surfaces_created,
        pool_stats.exhausted_lease_attempts,
        pending_stats.completed_after_next_packet,
        pending_stats.completed_already_ready,
        pending_stats.peak_depth,
    );
}

/// Конвейерная подача одной generation: учёт «в полёте», PTS отправленных и показанных.
struct ThroughputRun<'decoder> {
    decoder: &'decoder VideoDecodeThread,
    /// Отправлено, но ACK ещё не пришёл.
    packets_in_flight: usize,
    /// PTS отправленных packet-ов (микросекунды, как их видит VA-API decoder).
    sent_packet_pts: Vec<Duration>,
    /// PTS опубликованных кадров.
    shown_frame_pts: Vec<Duration>,
}

impl<'decoder> ThroughputRun<'decoder> {
    fn new(decoder: &'decoder VideoDecodeThread) -> Self {
        Self {
            decoder,
            packets_in_flight: 0,
            sent_packet_pts: Vec::new(),
            shown_frame_pts: Vec::new(),
        }
    }

    /// Ждёт свободного места в окне «в полёте» и отправляет packet.
    fn send_pipelined(&mut self, packet: Packet) {
        self.wait_until(
            |run| run.packets_in_flight < PACKETS_IN_FLIGHT,
            "packet ACK",
        );
        let decode_packet = decode_packet(&packet, PLAYBACK_GENERATION);
        match self.decoder.send_packet(decode_packet) {
            Ok(()) => {
                self.packets_in_flight += 1;
                self.sent_packet_pts.push(microsecond_pts(&packet));
            }
            Err(DecodeThreadSendError::Backpressure(reason)) => {
                panic!("packet channel is larger than the in-flight window: {reason:?}")
            }
            Err(DecodeThreadSendError::Fatal(error)) => panic!("fatal decoder send: {error}"),
        }
    }

    /// Ждёт ACK всех отправленных packet-ов (нужно до EOF-drain по контракту).
    fn wait_all_acks(&mut self) {
        self.wait_until(|run| run.packets_in_flight == 0, "final packet ACKs");
    }

    /// EOF-drain: DPB отдаёт хвост кадров, после `Drained` их больше не будет.
    fn drain_to_eof(&mut self) {
        let begin_result = self.decoder.begin_end_of_stream_drain(PLAYBACK_GENERATION);
        assert!(
            matches!(
                begin_result,
                VideoDecoderEndOfStreamDrainResult::Started(
                    VideoDecoderEndOfStreamDrainState::Draining { generation }
                        | VideoDecoderEndOfStreamDrainState::Drained { generation }
                ) if generation == PLAYBACK_GENERATION
            ),
            "decoder must accept EOF drain for the current generation, got {begin_result:?}"
        );
        self.wait_until(
            |run| match run.decoder.end_of_stream_drain_state() {
                VideoDecoderEndOfStreamDrainState::Drained { generation } => {
                    generation == PLAYBACK_GENERATION
                }
                VideoDecoderEndOfStreamDrainState::Fatal { error, .. } => {
                    panic!("VA-API EOF drain ended in error: {error}")
                }
                _ => false,
            },
            "EOF drain",
        );
        // Кадры, опубликованные вместе с переходом в `Drained`.
        self.collect_and_release_frames();
    }

    /// Крутит сбор ACK/кадров, пока условие не выполнится; падает, если декодер встал.
    fn wait_until(&mut self, mut condition: impl FnMut(&Self) -> bool, waiting_for: &str) {
        let mut last_progress_at = Instant::now();
        loop {
            if self.collect_progress() {
                last_progress_at = Instant::now();
            }
            if condition(self) {
                return;
            }
            if let Some(error) = self.decoder.try_recv_error() {
                panic!("VA-API decoder thread ended with an error: {error}");
            }
            assert!(
                last_progress_at.elapsed() < DECODER_PROGRESS_TIMEOUT,
                "decoder made no progress while waiting for {waiting_for}"
            );
            thread::sleep(POLL_INTERVAL);
        }
    }

    /// Забирает ACK и кадры; `true`, если декодер продвинулся.
    fn collect_progress(&mut self) -> bool {
        let acknowledged = self.decoder.drain_completed_packet_count();
        assert!(
            acknowledged <= self.packets_in_flight,
            "decoder acknowledged more packets than were sent"
        );
        self.packets_in_flight -= acknowledged;
        let collected_frames = self.collect_and_release_frames();
        acknowledged > 0 || collected_frames > 0
    }

    /// Забирает опубликованные кадры и сразу отдаёт их surfaces декодеру.
    fn collect_and_release_frames(&mut self) -> usize {
        let mut collected_frames = 0;
        while let Some(frame) = self.decoder.try_recv_frame() {
            assert_eq!(
                frame.generation, PLAYBACK_GENERATION,
                "frame of an unexpected generation"
            );
            self.shown_frame_pts.push(frame.pts);
            self.decoder.release_frame(frame.resource_handle);
            collected_frames += 1;
        }
        collected_frames
    }

    /// Каждый отправленный access unit показан ровно одним кадром со своим PTS.
    ///
    /// H.264 с B-кадрами выдаёт кадры в порядке показа, а packet-ы идут в порядке
    /// декодирования, поэтому сравниваются отсортированные множества PTS.
    fn assert_every_packet_shown_exactly_once(&self) {
        let mut sent = self.sent_packet_pts.clone();
        let mut shown = self.shown_frame_pts.clone();
        sent.sort_unstable();
        shown.sort_unstable();
        // Без уникальных PTS сравнение множеств не отличило бы потерю кадра от повтора
        // (например, у файла с edit list, где preroll прижат к PTS 0).
        assert!(
            sent.windows(2).all(|pair| pair[0] < pair[1]),
            "selected file must give every H.264 access unit a distinct PTS"
        );
        assert_eq!(
            shown.len(),
            sent.len(),
            "every H.264 access unit must produce exactly one frame"
        );
        assert_eq!(shown, sent, "shown frame PTS must match sent packet PTS");
    }
}
