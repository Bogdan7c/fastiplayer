//! Ignored-регрессия ограниченного пула выходных VA surfaces (cpu-parity, сессия 01).
//!
//! До пула каждый декодированный кадр получал новую VA surface
//! (`vaCreateSurfaces`) и уничтожал её после release (`vaDestroySurfaces`).
//! Тест прогоняет выбранный H.264 MP4 через production decoder thread и
//! проверяет по счётчикам пула, что:
//! 1. в установившемся воспроизведении surfaces переиспользуются: создано не
//!    больше ёмкости пула, хотя кадров декодировано во много раз больше;
//! 2. когда «плеер» держит кадры, пул исчерпывается и decoder встаёт на
//!    backpressure, а не выделяет новые surfaces; после release decoder
//!    продолжает без потери и без повтора кадров (pending access unit не
//!    потребляется дважды);
//! 3. seek-flush и EOF-drain проходят, не создавая surfaces сверх ёмкости.

// Интеграционный тест целиком является тестовым кодом: unwrap/expect/panic
// здесь работают как assertions. Production-политика паник сюда не относится.
#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::panic,
    reason = "integration test crate: panics are test assertions"
)]

use std::collections::VecDeque;
use std::path::PathBuf;
use std::thread;
use std::time::{Duration, Instant};

use codec_core::{
    H264Packetization, VideoCodec, VideoRequirementProbe, parse_avc_decoder_configuration_record,
    probe_video_packet_requirement_with_codec_private,
};
use media_core::{DemuxReadEvent, DemuxSeekRequest, Demuxer, Packet, TrackInfo, TrackKind};
use symphonia_demux::SymphoniaDemuxer;
use video_core::{
    DecodedFrame, VideoDecoderEndOfStreamDrainResult, VideoDecoderEndOfStreamDrainState,
    VideoStreamConfigResult, VideoStreamDecodeConfig, VideoStreamPacketization,
};
use video_frame_contract::{DmaBufImageLayout, VideoFrameContract};
use video_vaapi::output_surface_pool::OutputSurfacePoolStats;
use video_vaapi::{
    DecodePacket, DecodeThreadSendError, VideoDecodeThread, VideoDecodeThreadConfig,
};

/// Сколько кадров «плеер» держит в установившемся воспроизведении (очередь показа).
const STEADY_HELD_FRAMES: usize = 6;

/// Сколько packet-ов декодируем до фазы исчерпания пула.
const STEADY_PHASE_PACKETS: usize = 300;

/// Сколько packet-ов декодируем после seek.
const AFTER_SEEK_PACKETS: usize = 120;

/// Ожидание ACK/кадров/EOF от decoder thread.
const DECODER_WAIT_TIMEOUT: Duration = Duration::from_secs(5);

/// Если ACK не пришёл за это время при удержанных кадрах — decoder на backpressure.
const BACKPRESSURE_PROBE: Duration = Duration::from_millis(400);

/// Generation проигрывания с начала файла.
const PLAYBACK_GENERATION: u64 = 1;

/// Generation после seek-flush.
const AFTER_SEEK_GENERATION: u64 = 2;

#[test]
#[ignore = "requires explicit local H.264 MP4 and VA-API H.264 hardware; use scripts/media-regression.sh"]
fn h264_vaapi_recycles_bounded_output_surfaces_through_backpressure_seek_and_eof() {
    let media_path = selected_media_path();
    let mut demuxer = SymphoniaDemuxer::from_file(&media_path)
        .unwrap_or_else(|error| panic!("open {}: {error}", media_path.display()));
    let video_track = first_h264_video_track(&demuxer);
    let track_duration = video_track
        .duration
        .expect("Regression requires an H.264 track with a known duration to seek in the middle");

    let decoder = VideoDecodeThread::new_with_config(VideoDecodeThreadConfig::from_env())
        .expect("VA-API decoder thread should start");
    let mut session = PlaybackSession::new(&decoder, PLAYBACK_GENERATION);
    let first_packet = next_video_packet(&mut demuxer, &video_track)
        .expect("H.264 track must contain at least one packet");
    assert_eq!(
        decoder.configure_stream(h264_stream_config(&video_track, &first_packet)),
        VideoStreamConfigResult::Configured,
        "VA-API must accept the selected H.264 stream"
    );

    // Фаза 1: установившееся воспроизведение со скользящим окном удержанных кадров.
    session.send_and_wait_ack(first_packet);
    for _ in 1..STEADY_PHASE_PACKETS {
        let Some(packet) = next_video_packet(&mut demuxer, &video_track) else {
            break;
        };
        session.send_and_wait_ack(packet);
    }
    let steady_stats = decoder.output_surface_pool_stats();
    assert_bounded_pool(&steady_stats, "steady playback");
    assert!(
        session.decoded_frames >= STEADY_PHASE_PACKETS / 2,
        "steady phase must decode a meaningful number of frames, got {}",
        session.decoded_frames
    );
    assert!(
        steady_stats.surfaces_created < session.decoded_frames as u64,
        "surfaces must be recycled, not created per frame: created={} frames={}",
        steady_stats.surfaces_created,
        session.decoded_frames
    );

    // Фаза 2: «плеер» перестал отпускать кадры → пул исчерпан → backpressure.
    session.hold_all_frames = true;
    let backpressured_packet = loop {
        let packet = next_video_packet(&mut demuxer, &video_track)
            .expect("file ended before the surface pool was exhausted");
        if !session.send_and_probe_ack(packet.clone()) {
            break packet;
        }
    };
    let exhausted_stats = decoder.output_surface_pool_stats();
    assert!(
        exhausted_stats.exhausted_lease_attempts > 0,
        "decoder must be blocked by an exhausted pool: {exhausted_stats:?}"
    );
    assert_bounded_pool(&exhausted_stats, "exhausted pool");
    assert_eq!(
        exhausted_stats.live_surfaces, exhausted_stats.capacity,
        "backpressure must start exactly when the whole capacity is in use: {exhausted_stats:?}"
    );

    // Release всех удержанных кадров снимает backpressure: тот же packet доходит.
    session.hold_all_frames = false;
    session.release_held_frames();
    session.wait_ack(&backpressured_packet);
    while let Some(packet) = next_video_packet(&mut demuxer, &video_track) {
        session.send_and_wait_ack(packet);
    }
    session.drain_to_eof();
    session.assert_every_packet_shown_exactly_once("playback with backpressure");
    let eof_stats = decoder.output_surface_pool_stats();
    assert_bounded_pool(&eof_stats, "EOF drain");

    // Фаза 3: seek-flush в середину и короткое воспроизведение после него.
    decoder.flush().expect("seek flush VA-API decoder");
    session.release_held_frames();
    release_pending_frames(&decoder);
    let seek_target = track_duration / 2;
    demuxer
        .seek_with_request(DemuxSeekRequest::decode_point_before(seek_target))
        .expect("demux seek to the middle of the H.264 MP4");
    let mut after_seek = PlaybackSession::new(&decoder, AFTER_SEEK_GENERATION);
    for _ in 0..AFTER_SEEK_PACKETS {
        let Some(packet) = next_video_packet(&mut demuxer, &video_track) else {
            break;
        };
        after_seek.send_and_wait_ack(packet);
    }
    after_seek.drain_to_eof();
    assert!(
        after_seek.decoded_frames > 0,
        "decoder must publish frames after seek"
    );
    let after_seek_stats = decoder.output_surface_pool_stats();
    assert_bounded_pool(&after_seek_stats, "after seek");
    assert_eq!(
        after_seek_stats.surfaces_created, eof_stats.surfaces_created,
        "seek within the same format must reuse pooled surfaces"
    );

    eprintln!(
        "H.264 VA-API surface pool evidence: frames={} after_seek_frames={} \
         capacity={} created={} peak_live={} reused={} exhausted_attempts={}",
        session.decoded_frames,
        after_seek.decoded_frames,
        after_seek_stats.capacity,
        after_seek_stats.surfaces_created,
        after_seek_stats.peak_live_surfaces,
        after_seek_stats.leases_reused,
        after_seek_stats.exhausted_lease_attempts,
    );
}

/// Инвариант пула: живых и созданных surfaces не больше наибольшей ёмкости.
///
/// Сравниваем с `peak_capacity`, а не с текущей: после смены формата с меньшим
/// минимумом ёмкость сжимается, но старые surfaces у плеера ещё живы — это
/// штатно и не нарушение. Для файла с одним форматом обе ёмкости равны.
fn assert_bounded_pool(stats: &OutputSurfacePoolStats, phase_name: &str) {
    assert!(stats.capacity > 0, "{phase_name}: pool must have capacity");
    assert!(
        stats.peak_capacity >= stats.capacity,
        "{phase_name}: peak capacity must track the current one: {stats:?}"
    );
    assert!(
        stats.peak_live_surfaces <= stats.peak_capacity,
        "{phase_name}: live surfaces exceeded capacity: {stats:?}"
    );
    assert!(
        stats.surfaces_created <= stats.peak_capacity as u64,
        "{phase_name}: surfaces created beyond capacity (per-frame allocation?): {stats:?}"
    );
    assert_eq!(
        stats.allocation_failures, 0,
        "{phase_name}: driver allocation failed: {stats:?}"
    );
    assert_eq!(
        stats.surfaces_destroyed_not_idle, 0,
        "{phase_name}: a surface came back with unknown driver state: {stats:?}"
    );
}

/// Состояние «плеера» одной generation: отправленные PTS, показанные кадры, удержание.
struct PlaybackSession<'decoder> {
    decoder: &'decoder VideoDecodeThread,
    generation: u64,
    /// PTS отправленных packet-ов (микросекунды, как их видит VA-API decoder).
    sent_packet_pts: Vec<Duration>,
    /// PTS опубликованных кадров.
    shown_frame_pts: Vec<Duration>,
    /// Кадры, которые «плеер» ещё держит (очередь показа).
    held_frames: VecDeque<DecodedFrame>,
    /// Держать все кадры без release (моделирует остановившийся показ).
    hold_all_frames: bool,
    decoded_frames: usize,
}

impl<'decoder> PlaybackSession<'decoder> {
    fn new(decoder: &'decoder VideoDecodeThread, generation: u64) -> Self {
        Self {
            decoder,
            generation,
            sent_packet_pts: Vec::new(),
            shown_frame_pts: Vec::new(),
            held_frames: VecDeque::new(),
            hold_all_frames: false,
            decoded_frames: 0,
        }
    }

    /// Отправляет packet и ждёт его ACK, параллельно забирая кадры.
    fn send_and_wait_ack(&mut self, packet: Packet) {
        self.send(&packet);
        self.wait_ack(&packet);
    }

    /// Отправляет packet; `false`, если ACK не пришёл за `BACKPRESSURE_PROBE`.
    fn send_and_probe_ack(&mut self, packet: Packet) -> bool {
        self.send(&packet);
        self.wait_ack_until(Instant::now() + BACKPRESSURE_PROBE)
    }

    fn send(&mut self, packet: &Packet) {
        let decode_packet = DecodePacket {
            track_id: packet.track_id,
            pts: packet.pts,
            dts: packet.dts,
            track_dts: packet.track_dts,
            generation: self.generation,
            encoded_bytes: packet.data.clone(),
            keyframe: packet.keyframe.is_known_keyframe(),
            resolved_color: None,
        };
        match self.decoder.send_packet(decode_packet) {
            Ok(()) => self.sent_packet_pts.push(microsecond_pts(packet)),
            Err(DecodeThreadSendError::Backpressure(reason)) => {
                panic!("unexpected send backpressure for serial packets: {reason:?}")
            }
            Err(DecodeThreadSendError::Fatal(error)) => panic!("fatal decoder send: {error}"),
        }
    }

    /// Ждёт ACK уже отправленного packet-а (в том числе после снятия backpressure).
    fn wait_ack(&mut self, packet: &Packet) {
        assert!(
            self.wait_ack_until(Instant::now() + DECODER_WAIT_TIMEOUT),
            "decoder packet ACK timeout for pts {:?}",
            packet.pts
        );
    }

    fn wait_ack_until(&mut self, deadline: Instant) -> bool {
        loop {
            self.collect_frames();
            if self.decoder.drain_completed_packet_count() > 0 {
                return true;
            }
            assert_decoder_alive(self.decoder);
            if Instant::now() >= deadline {
                return false;
            }
            thread::sleep(Duration::from_millis(1));
        }
    }

    /// Забирает опубликованные кадры; держит их, как очередь показа плеера.
    fn collect_frames(&mut self) {
        while let Some(frame) = self.decoder.try_recv_frame() {
            assert_eq!(
                frame.generation, self.generation,
                "frame of a stale generation after seek"
            );
            self.shown_frame_pts.push(frame.pts);
            self.decoded_frames += 1;
            self.held_frames.push_back(frame);
        }
        if self.hold_all_frames {
            return;
        }
        while self.held_frames.len() > STEADY_HELD_FRAMES {
            if let Some(frame) = self.held_frames.pop_front() {
                self.decoder.release_frame(frame.resource_handle);
            }
        }
    }

    fn release_held_frames(&mut self) {
        while let Some(frame) = self.held_frames.pop_front() {
            self.decoder.release_frame(frame.resource_handle);
        }
    }

    /// EOF-drain текущей generation: DPB отдаёт хвост кадров.
    fn drain_to_eof(&mut self) {
        let begin_result = self.decoder.begin_end_of_stream_drain(self.generation);
        assert!(
            matches!(
                begin_result,
                VideoDecoderEndOfStreamDrainResult::Started(
                    VideoDecoderEndOfStreamDrainState::Draining { generation: started }
                        | VideoDecoderEndOfStreamDrainState::Drained { generation: started }
                ) if started == self.generation
            ),
            "decoder must accept EOF drain for the current generation, got {begin_result:?}"
        );
        let deadline = Instant::now() + DECODER_WAIT_TIMEOUT;
        loop {
            self.collect_frames();
            match self.decoder.end_of_stream_drain_state() {
                VideoDecoderEndOfStreamDrainState::Drained {
                    generation: drained,
                } if drained == self.generation => break,
                VideoDecoderEndOfStreamDrainState::Fatal { error, .. } => {
                    panic!("VA-API EOF drain ended in error: {error}")
                }
                _ => {}
            }
            assert_decoder_alive(self.decoder);
            assert!(Instant::now() < deadline, "decoder EOF drain timeout");
            thread::sleep(Duration::from_millis(1));
        }
        self.collect_frames();
        self.release_held_frames();
    }

    /// Каждый отправленный access unit показан ровно одним кадром со своим PTS.
    ///
    /// H.264 с B-кадрами выдаёт кадры в порядке показа, а packet-ы идут в порядке
    /// декодирования, поэтому сравниваются отсортированные множества PTS. Потеря
    /// кадра (packet выброшен на backpressure) или повтор (pending AU потреблён
    /// дважды) меняют множество.
    fn assert_every_packet_shown_exactly_once(&self, phase_name: &str) {
        let mut sent = self.sent_packet_pts.clone();
        let mut shown = self.shown_frame_pts.clone();
        sent.sort_unstable();
        shown.sort_unstable();
        assert_eq!(
            shown.len(),
            sent.len(),
            "{phase_name}: every H.264 access unit must produce exactly one frame"
        );
        assert_eq!(
            shown, sent,
            "{phase_name}: shown frame PTS must match sent packet PTS"
        );
    }
}

/// PTS packet-а, усечённый до микросекунд, как его передаёт VA-API decoder.
fn microsecond_pts(packet: &Packet) -> Duration {
    Duration::from_micros(
        u64::try_from(packet.pts.as_micros()).expect("packet PTS fits u64 microseconds"),
    )
}

/// Читает путь, выбранный `scripts/media-regression.sh`.
fn selected_media_path() -> PathBuf {
    let media_path = std::env::var_os("FASTIPLAYER_MEDIA_PATH")
        .map(PathBuf::from)
        .expect("FASTIPLAYER_MEDIA_PATH should point to an H.264 MP4");
    assert!(
        media_path.is_file(),
        "{} is not a file.",
        media_path.display()
    );
    media_path
}

/// Находит первую H.264 video-дорожку выбранного файла.
fn first_h264_video_track(demuxer: &SymphoniaDemuxer) -> TrackInfo {
    demuxer
        .tracks()
        .iter()
        .find(|track| {
            track.kind == TrackKind::Video
                && VideoCodec::from_container_codec_id(&track.codec_id) == Some(VideoCodec::H264)
        })
        .cloned()
        .expect("the selected file must contain an H.264 video track")
}

/// Следующий video-packet выбранной дорожки или `None` на EOF.
fn next_video_packet(demuxer: &mut SymphoniaDemuxer, video_track: &TrackInfo) -> Option<Packet> {
    loop {
        match demuxer.next_event().expect("demux next event") {
            DemuxReadEvent::Packet(packet)
                if packet.kind == TrackKind::Video && packet.track_id == video_track.id =>
            {
                return Some(packet);
            }
            DemuxReadEvent::Packet(_)
            | DemuxReadEvent::TracksChanged(_)
            | DemuxReadEvent::MediaMetadataChanged(_) => {}
            DemuxReadEvent::TemporarilyUnavailable(hint) => {
                panic!("local file unexpectedly reports temporary unavailability: {hint:?}")
            }
            DemuxReadEvent::EndOfStream => return None,
        }
    }
}

/// Строит stream config, как player: requirement из первого AU + `avcC` packetization.
fn h264_stream_config(video_track: &TrackInfo, first_packet: &Packet) -> VideoStreamDecodeConfig {
    let codec_private = video_track
        .codec_private
        .as_deref()
        .expect("H.264 MP4 must carry avcC");
    let requirement = match probe_video_packet_requirement_with_codec_private(
        VideoCodec::H264,
        &first_packet.data,
        Some(codec_private),
    ) {
        VideoRequirementProbe::Candidate(candidate) => candidate.requirement,
        other => panic!("H.264 requirement probe must succeed, got {other:?}"),
    };
    let record = parse_avc_decoder_configuration_record(codec_private)
        .expect("avcC of the selected file must parse");
    VideoStreamDecodeConfig::from_requirement(
        video_track.id,
        &requirement,
        VideoFrameContract::dma_buf_nv12(DmaBufImageLayout::SeparateLayers),
    )
    .with_codec_private(video_track.codec_private.clone())
    .with_packetization(Some(VideoStreamPacketization::H264(
        H264Packetization::from_avc_decoder_configuration_record(&record),
    )))
}

/// Освобождает кадры, оставшиеся в канале к моменту seek-flush.
fn release_pending_frames(decoder: &VideoDecodeThread) {
    while let Some(frame) = decoder.try_recv_frame() {
        decoder.release_frame(frame.resource_handle);
    }
}

/// Падает с причиной, если decoder thread сообщил ошибку.
fn assert_decoder_alive(decoder: &VideoDecodeThread) {
    if let Some(error) = decoder.try_recv_error() {
        panic!("VA-API decoder thread ended with an error: {error}");
    }
}
