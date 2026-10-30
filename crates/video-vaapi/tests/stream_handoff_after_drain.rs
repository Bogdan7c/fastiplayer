//! Ignored hardware-тест VA-API: передача decoder-а новому описанию после EOF drain
//! (сессия 03d моста, «эстафета» смены дорожек).
//!
//! Плеер при смене качества без перемотки выжимает decoder до последнего кадра,
//! делает `flush`, перенастраивает поток и подаёт пакеты нового эпизода — в той
//! же seek generation. Тест проверяет договор `flush` из `video-core`: после него
//! decoder больше не «выжат», и следующий EOF drain той же generation честно
//! дожимает хвост второго эпизода. Без этого конец файла после смены качества
//! считался бы «уже выжатым», а последние кадры пропадали бы.
//!
//! Запуск: `scripts/media-regression.sh --scenario h264-mp4-vaapi-handoff-after-drain --path <файл>`.

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

use media_core::{DemuxSeekRequest, Demuxer, Packet, TrackInfo};
use support::h264_media::{
    decode_packet, first_h264_video_track, h264_stream_config, next_video_packet,
    selected_media_path,
};
use symphonia_demux::SymphoniaDemuxer;
use video_core::{
    VideoDecoderEndOfStreamDrainResult, VideoDecoderEndOfStreamDrainState, VideoStreamConfigResult,
};
use video_vaapi::{VideoDecodeThread, VideoDecodeThreadConfig};

/// Сколько packet-ов подаём в каждый эпизод (короче GOP-а не важно: начинаем с ключевого).
const PACKETS_PER_EPISODE: usize = 24;

/// Одна seek generation на оба эпизода: смена описания её не меняет.
const SHARED_GENERATION: u64 = 5;

/// Если декодер не дал ни ACK, ни кадра за это время — он завис.
const DECODER_PROGRESS_TIMEOUT: Duration = Duration::from_secs(5);

/// Пауза опроса, когда декодеру ещё нечего отдать.
const POLL_INTERVAL: Duration = Duration::from_micros(200);

#[test]
#[ignore = "requires explicit local H.264 MP4 and VA-API H.264 hardware; use scripts/media-regression.sh"]
fn h264_vaapi_second_drain_in_same_generation_after_flush_returns_full_tail() {
    let media_path = selected_media_path();
    let mut demuxer = SymphoniaDemuxer::from_file(&media_path)
        .unwrap_or_else(|error| panic!("open {}: {error}", media_path.display()));
    let video_track = first_h264_video_track(&demuxer);
    let decoder = VideoDecodeThread::new_with_config(VideoDecodeThreadConfig::from_env())
        .expect("VA-API decoder thread should start");
    let first_episode = read_episode(&mut demuxer, &video_track);
    let stream_config = h264_stream_config(&video_track, &first_episode[0]);
    assert_eq!(
        decoder.configure_stream(stream_config.clone()),
        VideoStreamConfigResult::Configured
    );

    // Эпизод 1: каждый packet даёт ровно один кадр, хвост приходит только через drain.
    let first_frames = decode_episode_and_drain(&decoder, &first_episode);
    assert_eq!(first_frames, first_episode.len());

    // Передача: flush возвращает decoder в «не выжат», перенастройка — как у плеера.
    decoder.flush().expect("flush after drain must succeed");
    assert_eq!(
        decoder.end_of_stream_drain_state(),
        VideoDecoderEndOfStreamDrainState::Idle,
        "flushed decoder must not stay drained"
    );
    let reconfigure = decoder.configure_stream(stream_config);
    assert!(
        matches!(
            reconfigure,
            VideoStreamConfigResult::Configured | VideoStreamConfigResult::Unchanged
        ),
        "decoder must accept the next episode description, got {reconfigure:?}"
    );

    // Эпизод 2 начинается с ключевого кадра (как новый init у MSE) в той же generation.
    demuxer
        .seek_with_request(DemuxSeekRequest::decode_point_before(Duration::ZERO))
        .expect("demux seek to the first keyframe must succeed");
    let second_episode = read_episode(&mut demuxer, &video_track);
    let second_frames = decode_episode_and_drain(&decoder, &second_episode);
    assert_eq!(
        second_frames,
        second_episode.len(),
        "second drain in the same generation must return the whole tail"
    );
}

/// Читает packet-ы одного эпизода; первый обязан быть ключевым.
fn read_episode(demuxer: &mut SymphoniaDemuxer, video_track: &TrackInfo) -> Vec<Packet> {
    let episode: Vec<Packet> = std::iter::from_fn(|| next_video_packet(demuxer, video_track))
        .take(PACKETS_PER_EPISODE)
        .collect();
    assert_eq!(episode.len(), PACKETS_PER_EPISODE, "file is too short");
    assert!(
        episode[0].keyframe.is_known_keyframe(),
        "episode must start at a keyframe"
    );
    episode
}

/// Подаёт эпизод, ждёт ACK всех packet-ов, выжимает decoder и считает кадры.
fn decode_episode_and_drain(decoder: &VideoDecodeThread, episode: &[Packet]) -> usize {
    let mut progress = DecoderProgress::new(decoder);
    for packet in episode {
        decoder
            .send_packet(decode_packet(packet, SHARED_GENERATION))
            .expect("packet channel must accept the episode");
        progress.packets_in_flight += 1;
        progress.collect();
    }
    progress.wait_until(|progress| progress.packets_in_flight == 0, "packet ACKs");
    let begin_result = decoder.begin_end_of_stream_drain(SHARED_GENERATION);
    assert!(
        matches!(begin_result, VideoDecoderEndOfStreamDrainResult::Started(_)),
        "every episode must start its own drain, got {begin_result:?}"
    );
    progress.wait_until(
        |progress| match progress.decoder.end_of_stream_drain_state() {
            VideoDecoderEndOfStreamDrainState::Drained { generation } => {
                generation == SHARED_GENERATION
            }
            VideoDecoderEndOfStreamDrainState::Fatal { error, .. } => {
                panic!("VA-API EOF drain ended in error: {error}")
            }
            _ => false,
        },
        "EOF drain",
    );
    // `Drained` по договору означает пустой канал кадров: всё уже собрано.
    assert!(decoder.try_recv_frame().is_none());
    progress.frames
}

/// Учёт ACK-ов и кадров одного эпизода.
struct DecoderProgress<'decoder> {
    decoder: &'decoder VideoDecodeThread,
    packets_in_flight: usize,
    frames: usize,
}

impl<'decoder> DecoderProgress<'decoder> {
    fn new(decoder: &'decoder VideoDecodeThread) -> Self {
        Self {
            decoder,
            packets_in_flight: 0,
            frames: 0,
        }
    }

    /// Забирает ACK-и и кадры; кадры сразу отпускаются, чтобы пул не кончился.
    fn collect(&mut self) -> bool {
        let acknowledged = self.decoder.drain_completed_packet_count();
        self.packets_in_flight -= acknowledged;
        let mut collected = 0;
        while let Some(frame) = self.decoder.try_recv_frame() {
            assert_eq!(frame.generation, SHARED_GENERATION);
            self.decoder.release_frame(frame.resource_handle);
            collected += 1;
        }
        self.frames += collected;
        acknowledged > 0 || collected > 0
    }

    fn wait_until(&mut self, mut condition: impl FnMut(&Self) -> bool, waiting_for: &str) {
        let mut last_progress_at = Instant::now();
        loop {
            if self.collect() {
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
}
