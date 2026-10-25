//! Ignored hardware-«шторм» seek/scrub для VA-API H.264 (gles2-renderer, сессия 09a).
//!
//! После перевода cros-codecs в `NonBlocking` кадры живут в очереди «в полёте»
//! (`decoder::gpu_pending_frames`), пока GPU их декодирует. Тест бьёт по всем
//! lifecycle-переходам этой очереди через production `VideoDecodeThread`:
//! 1. **пауза после одного packet-а** — кадр обязан выйти, хотя следующего
//!    packet-а нет (без правила «нечего подавать → отдать очередь» он застрял бы);
//! 2. **частые seek** — flush приходит, пока packet-ы ещё в очереди и кадры «в
//!    полёте»; после flush не должно быть ни одного кадра старой generation,
//!    а при активном preroll floor — кадров раньше floor;
//! 2. **снятие floor посреди полёта** (как commit seek-а) — декодер продолжает
//!    отдавать кадры текущей generation;
//! 3. **финал до EOF** — после последнего seek каждый packet дал ровно один кадр,
//!    пул ограничен, ни одна surface не вернулась «грязной».
//!
//! Запуск: `scripts/media-regression.sh --scenario h264-mp4-vaapi-seek-storm --path <файл>`
//! и `--scenario h264-mp4-vaapi-paused-frame --path <файл>`.

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

use media_core::{DemuxSeekRequest, Demuxer, TrackInfo};
use support::h264_media::{
    assert_bounded_pool, decode_packet, first_h264_video_track, h264_stream_config,
    microsecond_pts, next_video_packet, selected_media_path,
};
use symphonia_demux::SymphoniaDemuxer;
use video_core::{
    DecodedFrame, VideoDecoderEndOfStreamDrainResult, VideoDecoderEndOfStreamDrainState,
    VideoPrerollOutputFloor, VideoPrerollOutputFloorClear, VideoPrerollOutputFloorResult,
    VideoStreamConfigResult,
};
use video_vaapi::{DecodeThreadSendError, VideoDecodeThread, VideoDecodeThreadConfig};

/// Сколько seek-ов делает шторм.
const SEEK_COUNT: usize = 80;

/// Наибольшее число packet-ов, отправленных между двумя seek-ами.
const MAX_PACKETS_BETWEEN_SEEKS: u64 = 24;

/// Предел packet-ов от decode point до кадра на цели accurate seek-а (GOP фикстур короче).
const MAX_PACKETS_TO_FLOOR_FRAME: usize = 2_000;

/// Сколько packet-ов может ждать ACK одновременно (ниже ёмкости канала 32).
const PACKETS_IN_FLIGHT: usize = 8;

/// Сколько ждём, пока очередь «в полёте» опустеет на «паузе».
const PAUSED_FRAME_TIMEOUT: Duration = Duration::from_secs(2);

/// Сколько packet-ов подаём по одному с паузой после каждого.
const PAUSED_PACKETS: usize = 12;

/// Если декодер не дал ни ACK, ни кадра за это время — он завис.
const DECODER_PROGRESS_TIMEOUT: Duration = Duration::from_secs(5);

/// Пауза опроса, когда декодеру ещё нечего отдать.
const POLL_INTERVAL: Duration = Duration::from_micros(200);

/// Начальное состояние детерминированного генератора целей seek-а.
const STORM_SEED: u64 = 0x09A0_5EEC;

#[test]
#[ignore = "requires explicit local H.264 MP4 and VA-API H.264 hardware; use scripts/media-regression.sh"]
fn h264_vaapi_publishes_in_flight_frames_while_no_packet_follows() {
    let media_path = selected_media_path();
    let mut demuxer = SymphoniaDemuxer::from_file(&media_path)
        .unwrap_or_else(|error| panic!("open {}: {error}", media_path.display()));
    let video_track = first_h264_video_track(&demuxer);
    let decoder = VideoDecodeThread::new_with_config(VideoDecodeThreadConfig::from_env())
        .expect("VA-API decoder thread should start");
    let mut storm = StormDriver::new(&decoder);
    let first_packet =
        next_video_packet(&mut demuxer, &video_track).expect("H.264 track must have packets");
    assert_eq!(
        decoder.configure_stream(h264_stream_config(&video_track, &first_packet)),
        VideoStreamConfigResult::Configured,
        "VA-API must accept the selected H.264 stream"
    );

    // Packet-ы по одному с «паузой» после каждого: следующего packet-а нет, пока
    // декодер не отдал всё, что уже вышло из cros-codecs. Глубина DPB файла
    // не важна: проверяется именно очередь «в полёте», а не число кадров.
    storm.begin_generation(None);
    storm.send(&first_packet);
    storm.wait_for_empty_gpu_pending_queue(PAUSED_FRAME_TIMEOUT);
    for _ in 1..PAUSED_PACKETS {
        let Some(packet) = next_video_packet(&mut demuxer, &video_track) else {
            break;
        };
        storm.send(&packet);
        storm.wait_for_empty_gpu_pending_queue(PAUSED_FRAME_TIMEOUT);
    }

    let stats = decoder.gpu_pending_frame_stats();
    assert!(
        !storm.current_generation_frames.is_empty(),
        "paused decoding must publish frames, got none: {stats:?}"
    );
    assert!(
        stats.completed_on_idle > 0,
        "frames left on pause must be completed by the idle rule: {stats:?}"
    );
    assert_eq!(
        stats.enqueued,
        stats.completed_after_next_packet
            + stats.completed_already_ready
            + stats.completed_after_query_error
            + stats.completed_on_capacity
            + stats.completed_on_idle
            + stats.completed_on_output_backpressure
            + stats.completed_on_lifecycle
            + stats.discarded,
        "every queued frame must leave the queue exactly once: {stats:?}"
    );
}

#[test]
#[ignore = "requires explicit local H.264 MP4 and VA-API H.264 hardware; use scripts/media-regression.sh"]
fn h264_vaapi_survives_seek_storm_with_frames_in_flight() {
    let media_path = selected_media_path();
    let mut demuxer = SymphoniaDemuxer::from_file(&media_path)
        .unwrap_or_else(|error| panic!("open {}: {error}", media_path.display()));
    let video_track = first_h264_video_track(&demuxer);
    let track_duration = video_track
        .duration
        .expect("seek storm requires an H.264 track with a known duration");
    let decoder = VideoDecodeThread::new_with_config(VideoDecodeThreadConfig::from_env())
        .expect("VA-API decoder thread should start");
    let mut storm = StormDriver::new(&decoder);
    let first_packet =
        next_video_packet(&mut demuxer, &video_track).expect("H.264 track must have packets");
    assert_eq!(
        decoder.configure_stream(h264_stream_config(&video_track, &first_packet)),
        VideoStreamConfigResult::Configured,
        "VA-API must accept the selected H.264 stream"
    );
    storm.begin_generation(None);
    storm.send(&first_packet);

    // Фаза 1: шторм seek-ов с packet-ами и кадрами «в полёте» в момент flush.
    let mut random = DeterministicRandom::new(STORM_SEED);
    for seek_index in 0..SEEK_COUNT {
        let target = track_duration.mul_f64(random.next_fraction());
        storm.seek(&mut demuxer, target);
        // Каждый третий seek — с accurate floor, как обычный seek плеера.
        let floor = (seek_index % 3 == 0).then_some(target);
        storm.begin_generation(floor);
        if floor.is_some() {
            // Accurate seek: кадры до цели подавляются; packet-ы идут, пока не выйдет
            // кадр на цели (как ждёт плеер), потом commit снимает floor посреди полёта.
            storm.send_until_floor_frame_published(&mut demuxer, &video_track);
            storm.clear_floor();
        }
        let packet_count = 1 + random.next_below(MAX_PACKETS_BETWEEN_SEEKS);
        for _ in 0..packet_count {
            let Some(packet) = next_video_packet(&mut demuxer, &video_track) else {
                break;
            };
            storm.send(&packet);
        }
        // Flush следующего seek-а прилетает без ожидания ACK и кадров.
    }

    // Фаза 2: последний seek без floor и декодирование до EOF.
    storm.seek(&mut demuxer, track_duration / 3);
    storm.begin_generation(None);
    let mut sent_after_last_seek = Vec::new();
    while let Some(packet) = next_video_packet(&mut demuxer, &video_track) {
        sent_after_last_seek.push(microsecond_pts(&packet));
        storm.send(&packet);
    }
    storm.drain_to_eof();

    let mut shown = storm
        .current_generation_frames
        .iter()
        .map(|frame| frame.pts)
        .collect::<Vec<_>>();
    sent_after_last_seek.sort_unstable();
    shown.sort_unstable();
    assert!(
        sent_after_last_seek
            .windows(2)
            .all(|pair| pair[0] < pair[1]),
        "selected file must give every H.264 access unit a distinct PTS"
    );
    assert_eq!(
        shown, sent_after_last_seek,
        "after the storm every packet of the final generation must give exactly one frame"
    );
    let pool_stats = decoder.output_surface_pool_stats();
    assert_bounded_pool(&pool_stats, storm.total_frames);
    println!(
        "vaapi_seek_storm seeks={SEEK_COUNT} total_frames={} floor_checked_frames={} final_frames={} pool_peak_live={} surfaces_created={}",
        storm.total_frames,
        storm.floor_checked_frames,
        shown.len(),
        pool_stats.peak_live_surfaces,
        pool_stats.surfaces_created,
    );
}

/// «Плеер» шторма: generation, floor, учёт «в полёте» и проверки каждого кадра.
struct StormDriver<'decoder> {
    decoder: &'decoder VideoDecodeThread,
    generation: u64,
    /// Floor текущей generation, пока он не снят.
    active_floor: Option<Duration>,
    packets_in_flight: usize,
    /// Кадры текущей generation (метаданные; surfaces уже отпущены).
    current_generation_frames: Vec<FrameRecord>,
    total_frames: usize,
    /// Сколько кадров проверено против активного floor.
    floor_checked_frames: usize,
}

/// Что тест помнит о кадре после release его surface.
struct FrameRecord {
    pts: Duration,
}

impl<'decoder> StormDriver<'decoder> {
    fn new(decoder: &'decoder VideoDecodeThread) -> Self {
        Self {
            decoder,
            generation: 0,
            active_floor: None,
            packets_in_flight: 0,
            current_generation_frames: Vec::new(),
            total_frames: 0,
            floor_checked_frames: 0,
        }
    }

    /// Seek-flush декодера (packet-ы и кадры могут быть ещё в полёте) и demux seek.
    fn seek(&mut self, demuxer: &mut SymphoniaDemuxer, target: Duration) {
        self.collect_frames();
        self.decoder.flush().expect("seek flush must succeed");
        // Flush сам отпускает кадры канала и сбрасывает ACK: «в полёте» больше ничего нет.
        self.packets_in_flight = 0;
        demuxer
            .seek_with_request(DemuxSeekRequest::decode_point_before(target))
            .expect("demux seek must succeed");
    }

    /// Новая generation; `floor` — accurate seek floor (кадры раньше него подавляются).
    fn begin_generation(&mut self, floor: Option<Duration>) {
        self.generation += 1;
        self.current_generation_frames.clear();
        self.active_floor = floor;
        if let Some(floor_pts) = floor {
            let result = self
                .decoder
                .set_preroll_output_floor(VideoPrerollOutputFloor {
                    generation: self.generation,
                    floor_pts,
                    retain_latest_before_floor: true,
                });
            assert!(
                matches!(result, VideoPrerollOutputFloorResult::Applied),
                "floor of a fresh generation must apply, got {result:?}"
            );
        }
    }

    /// Снимает floor текущей generation (commit seek-а).
    fn clear_floor(&mut self) {
        let result = self.decoder.clear_preroll_output_floor(
            VideoPrerollOutputFloorClear::MatchingGeneration(self.generation),
        );
        assert!(
            matches!(result, VideoPrerollOutputFloorResult::Cleared),
            "active floor must clear, got {result:?}"
        );
        self.active_floor = None;
    }

    /// Шлёт packet-ы, пока при активном floor не выйдет кадр на цели или после неё.
    ///
    /// Конец файла до цели допустим (цель у самого конца): тогда floor
    /// снимается без опубликованного кадра, как при timeout-е commit-а у плеера.
    fn send_until_floor_frame_published(
        &mut self,
        demuxer: &mut SymphoniaDemuxer,
        video_track: &TrackInfo,
    ) {
        let checked_before = self.floor_checked_frames;
        for _ in 0..MAX_PACKETS_TO_FLOOR_FRAME {
            if self.floor_checked_frames > checked_before {
                return;
            }
            let Some(packet) = next_video_packet(demuxer, video_track) else {
                return;
            };
            self.send(&packet);
            self.collect_progress();
        }
        panic!("no frame at or after the floor within {MAX_PACKETS_TO_FLOOR_FRAME} packets");
    }

    /// Отправляет packet, держа не больше `PACKETS_IN_FLIGHT` без ACK.
    fn send(&mut self, packet: &media_core::Packet) {
        self.wait_until(
            |storm| storm.packets_in_flight < PACKETS_IN_FLIGHT,
            "packet ACK",
        );
        match self
            .decoder
            .send_packet(decode_packet(packet, self.generation))
        {
            Ok(()) => self.packets_in_flight += 1,
            Err(DecodeThreadSendError::Backpressure(reason)) => {
                panic!("packet channel is larger than the in-flight window: {reason:?}")
            }
            Err(DecodeThreadSendError::Fatal(error)) => panic!("fatal decoder send: {error}"),
        }
    }

    /// Ждёт ACK всех packet-ов, затем — пока очередь «в полёте» не опустеет.
    fn wait_for_empty_gpu_pending_queue(&mut self, timeout: Duration) {
        self.wait_until(
            |storm| storm.packets_in_flight == 0,
            "packet ACKs before pause",
        );
        let deadline = Instant::now() + timeout;
        loop {
            self.collect_progress();
            let stats = self.decoder.gpu_pending_frame_stats();
            if stats.current_depth == 0 {
                return;
            }
            self.assert_decoder_alive();
            assert!(
                Instant::now() < deadline,
                "decoder kept {} frame(s) in flight while no packet followed: {stats:?}",
                stats.current_depth
            );
            thread::sleep(POLL_INTERVAL);
        }
    }

    /// Дожидается ACK всех packet-ов, затем EOF-drain текущей generation.
    fn drain_to_eof(&mut self) {
        self.wait_until(|storm| storm.packets_in_flight == 0, "final packet ACKs");
        let begin_result = self.decoder.begin_end_of_stream_drain(self.generation);
        assert!(
            matches!(
                begin_result,
                VideoDecoderEndOfStreamDrainResult::Started(
                    VideoDecoderEndOfStreamDrainState::Draining { .. }
                        | VideoDecoderEndOfStreamDrainState::Drained { .. }
                )
            ),
            "decoder must accept EOF drain, got {begin_result:?}"
        );
        let generation = self.generation;
        self.wait_until(
            |storm| match storm.decoder.end_of_stream_drain_state() {
                VideoDecoderEndOfStreamDrainState::Drained {
                    generation: drained,
                } => drained == generation,
                VideoDecoderEndOfStreamDrainState::Fatal { error, .. } => {
                    panic!("VA-API EOF drain ended in error: {error}")
                }
                _ => false,
            },
            "EOF drain",
        );
        self.collect_frames();
    }

    fn wait_until(&mut self, mut condition: impl FnMut(&Self) -> bool, waiting_for: &str) {
        let mut last_progress_at = Instant::now();
        loop {
            if self.collect_progress() {
                last_progress_at = Instant::now();
            }
            if condition(self) {
                return;
            }
            self.assert_decoder_alive();
            assert!(
                last_progress_at.elapsed() < DECODER_PROGRESS_TIMEOUT,
                "decoder made no progress while waiting for {waiting_for}"
            );
            thread::sleep(POLL_INTERVAL);
        }
    }

    fn collect_progress(&mut self) -> bool {
        let acknowledged = self.decoder.drain_completed_packet_count();
        assert!(
            acknowledged <= self.packets_in_flight,
            "decoder acknowledged more packets than were sent"
        );
        self.packets_in_flight -= acknowledged;
        acknowledged > 0 || self.collect_frames() > 0
    }

    /// Проверяет каждый опубликованный кадр и сразу отпускает его surface.
    fn collect_frames(&mut self) -> usize {
        let mut collected = 0;
        while let Some(frame) = self.decoder.try_recv_frame() {
            self.check_frame(&frame);
            self.current_generation_frames
                .push(FrameRecord { pts: frame.pts });
            self.decoder.release_frame(frame.resource_handle);
            collected += 1;
        }
        self.total_frames += collected;
        collected
    }

    fn check_frame(&mut self, frame: &DecodedFrame) {
        assert_eq!(
            frame.generation, self.generation,
            "a frame of an old generation leaked past the seek flush"
        );
        if let Some(floor) = self.active_floor {
            assert!(
                frame.pts >= floor,
                "active preroll floor {floor:?} let an earlier frame {:?} through",
                frame.pts
            );
            self.floor_checked_frames += 1;
        }
    }

    fn assert_decoder_alive(&self) {
        if let Some(error) = self.decoder.try_recv_error() {
            panic!("VA-API decoder thread ended with an error: {error}");
        }
    }
}

/// Детерминированный генератор (64-битный LCG Кнута): шторм одинаков в каждом прогоне.
struct DeterministicRandom {
    state: u64,
}

impl DeterministicRandom {
    /// Множитель и приращение LCG из MMIX (Knuth).
    const MULTIPLIER: u64 = 6_364_136_223_846_793_005;
    const INCREMENT: u64 = 1_442_695_040_888_963_407;

    fn new(seed: u64) -> Self {
        Self { state: seed }
    }

    fn next_u64(&mut self) -> u64 {
        self.state = self
            .state
            .wrapping_mul(Self::MULTIPLIER)
            .wrapping_add(Self::INCREMENT);
        self.state
    }

    /// Доля в `[0, 1)` из старших 53 бит.
    fn next_fraction(&mut self) -> f64 {
        (self.next_u64() >> 11) as f64 / (1_u64 << 53) as f64
    }

    /// Целое в `[0, upper)`.
    fn next_below(&mut self, upper: u64) -> u64 {
        self.next_u64() % upper
    }
}
