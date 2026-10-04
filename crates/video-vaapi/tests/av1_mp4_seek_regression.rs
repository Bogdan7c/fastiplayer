//! Реальная ignored-регрессия аппаратного AV1 VA-API decode с global motion.
//!
//! Тест прогоняет выбранный AV1 MP4 через production decoder thread:
//! проигрывание с начала, затем seek-flush и декодирование до EOF. Зависание
//! AMD VCN из-за неверных warp-параметров роняет процесс (Mesa abort), поэтому
//! любой возврат бага global-motion parsing превращается в падение теста.

// Интеграционный тест целиком является тестовым кодом: unwrap/expect/panic
// здесь работают как assertions. Production-политика паник сюда не относится.
#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::panic,
    reason = "integration test crate: panics are test assertions"
)]

use std::path::PathBuf;
use std::thread;
use std::time::{Duration, Instant};

use codec_core::{
    VideoCodec, VideoDecodeRequirement, av1_decode_requirement_from_decoder_configuration_record,
};
use media_core::{DemuxReadEvent, DemuxSeekRequest, Demuxer, Packet, TrackInfo, TrackKind};
use symphonia_demux::SymphoniaDemuxer;
use video_core::{
    DecodedFrame, VideoDecoderEndOfStreamDrainResult, VideoDecoderEndOfStreamDrainState,
    VideoStreamConfigResult, VideoStreamDecodeConfig,
};
use video_frame_contract::{DmaBufImageLayout, VideoFrameContract};
use video_vaapi::{
    DecodePacket, DecodeThreadSendError, VideoDecodeThread, VideoDecodeThreadConfig,
};

/// Сколько media-времени проигрываем с начала до seek.
///
/// На фрагменте JrT1PjOjOjc (yt-dlp 399, 900–1000 s) до фикса VCN зависал
/// уже на ~36-м кадре, поэтому 20 s с запасом покрывают первые warp-кадры.
const PLAYBACK_PHASE_SPAN: Duration = Duration::from_secs(20);

/// Максимальное ожидание ACK/кадров/EOF от decoder thread.
const DECODER_WAIT_TIMEOUT: Duration = Duration::from_secs(5);

/// Generation проигрывания с начала файла.
const PLAYBACK_GENERATION: u64 = 1;

/// Generation после seek-flush.
const AFTER_SEEK_GENERATION: u64 = 2;

/// Итог одной фазы: сколько temporal unit-ов отправлено и какие кадры вышли.
#[derive(Default)]
struct PhaseEvidence {
    /// Отправленные decoder-у AV1 temporal unit-ы.
    sent_packets: usize,
    /// PTS опубликованных кадров в порядке получения.
    frame_pts: Vec<Duration>,
}

#[test]
#[ignore = "requires explicit local AV1 Main 8-bit MP4 and VA-API AV1 VLD hardware; use scripts/media-regression.sh"]
fn av1_mp4_vaapi_decodes_global_motion_before_and_after_seek() {
    let media_path = selected_media_path();
    let mut demuxer = SymphoniaDemuxer::from_file(&media_path)
        .unwrap_or_else(|error| panic!("open {}: {error}", media_path.display()));
    let video_track = first_av1_video_track(&demuxer);
    let track_duration = video_track
        .duration
        .expect("регрессии нужен AV1 track с известной длительностью для seek в середину");

    let decoder = VideoDecodeThread::new_with_config(VideoDecodeThreadConfig::from_env())
        .expect("VA-API decoder thread должен стартовать");
    assert_eq!(
        decoder.configure_stream(av1_stream_config(&video_track)),
        VideoStreamConfigResult::Configured,
        "VA-API должен принять AV1 Main 8-bit NV12 stream"
    );

    // Фаза 1: обычное проигрывание с начала файла.
    let mut playback = decode_until(
        &decoder,
        &mut demuxer,
        &video_track,
        PLAYBACK_GENERATION,
        PhaseEnd::AtPts(PLAYBACK_PHASE_SPAN),
    );
    wait_for_all_frames(&decoder, PLAYBACK_GENERATION, &mut playback);
    assert_phase_frames(&playback, "playback");

    // Фаза 2: seek в середину файла и декодирование до EOF.
    decoder.flush().expect("seek flush VA-API decoder-а");
    release_pending_frames(&decoder);
    let seek_target = track_duration / 2;
    let seek_result = demuxer
        .seek_with_request(DemuxSeekRequest::decode_point_before(seek_target))
        .expect("demux seek в середину AV1 MP4");
    let decode_point = seek_result.actual_position.as_duration();
    assert!(
        decode_point <= seek_target,
        "decode point не должен быть позже target"
    );

    let mut after_seek = decode_until(
        &decoder,
        &mut demuxer,
        &video_track,
        AFTER_SEEK_GENERATION,
        PhaseEnd::EndOfStream,
    );
    drain_decoder_to_eof(&decoder, AFTER_SEEK_GENERATION, &mut after_seek);
    assert_phase_frames(&after_seek, "after-seek");
    assert!(
        after_seek.frame_pts[0] >= decode_point,
        "после seek не должно быть кадров раньше decode point"
    );

    eprintln!(
        "AV1 VA-API evidence: playback_frames={} after_seek_frames={} decode_point_ms={}",
        playback.frame_pts.len(),
        after_seek.frame_pts.len(),
        decode_point.as_millis()
    );
}

/// Где заканчивается фаза отправки пакетов.
enum PhaseEnd {
    /// Остановиться на первом пакете с PTS не меньше указанного.
    AtPts(Duration),
    /// Отправить всё до конца файла.
    EndOfStream,
}

/// Читает путь, выбранный `scripts/media-regression.sh`.
fn selected_media_path() -> PathBuf {
    let media_path = std::env::var_os("FASTIPLAYER_MEDIA_PATH")
        .map(PathBuf::from)
        .expect("FASTIPLAYER_MEDIA_PATH должен указывать на AV1 MP4");
    assert!(
        media_path.is_file(),
        "{} не является файлом",
        media_path.display()
    );
    media_path
}

/// Находит первую AV1 video-дорожку выбранного файла.
fn first_av1_video_track(demuxer: &SymphoniaDemuxer) -> TrackInfo {
    demuxer
        .tracks()
        .iter()
        .find(|track| {
            track.kind == TrackKind::Video
                && VideoCodec::from_container_codec_id(&track.codec_id) == Some(VideoCodec::Av1)
        })
        .cloned()
        .expect("выбранный файл должен содержать AV1 video track")
}

/// Строит тот же stream config, что player собирает из av1C: NV12 DMA-BUF.
fn av1_stream_config(video_track: &TrackInfo) -> VideoStreamDecodeConfig {
    let requirement = video_track
        .codec_private
        .as_deref()
        .map(|av1c_record| {
            av1_decode_requirement_from_decoder_configuration_record(av1c_record)
                .expect("av1C выбранного файла должен разбираться")
        })
        .unwrap_or_else(|| VideoDecodeRequirement::new(VideoCodec::Av1));

    VideoStreamDecodeConfig::from_requirement(
        video_track.id,
        &requirement,
        VideoFrameContract::dma_buf_nv12(DmaBufImageLayout::SeparateLayers),
    )
    .with_codec_private(video_track.codec_private.clone())
}

/// Отправляет video-пакеты выбранной дорожки до границы фазы.
fn decode_until(
    decoder: &VideoDecodeThread,
    demuxer: &mut SymphoniaDemuxer,
    video_track: &TrackInfo,
    generation: u64,
    phase_end: PhaseEnd,
) -> PhaseEvidence {
    let mut evidence = PhaseEvidence::default();
    loop {
        let packet = match demuxer.next_event().expect("demux следующего события")
        {
            DemuxReadEvent::Packet(packet)
                if packet.kind == TrackKind::Video && packet.track_id == video_track.id =>
            {
                packet
            }
            DemuxReadEvent::Packet(_)
            | DemuxReadEvent::TracksChanged(_)
            | DemuxReadEvent::MediaMetadataChanged(_) => continue,
            DemuxReadEvent::TemporarilyUnavailable(hint) => {
                panic!("локальный файл неожиданно вернул temporary readiness: {hint:?}")
            }
            DemuxReadEvent::EndOfStream => return evidence,
        };
        if let PhaseEnd::AtPts(phase_end_pts) = phase_end
            && packet.pts >= phase_end_pts
        {
            return evidence;
        }
        send_video_packet(decoder, generation, packet, &mut evidence);
    }
}

/// Отправляет один temporal unit и ждёт его ACK, параллельно собирая кадры.
fn send_video_packet(
    decoder: &VideoDecodeThread,
    generation: u64,
    packet: Packet,
    evidence: &mut PhaseEvidence,
) {
    let decode_packet = DecodePacket {
        track_id: packet.track_id,
        pts: packet.pts,
        dts: packet.dts,
        track_dts: packet.track_dts,
        generation,
        encoded_bytes: packet.data,
        keyframe: packet.keyframe.is_known_keyframe(),
        resolved_color: None,
    };
    match decoder.send_packet(decode_packet) {
        Ok(()) => evidence.sent_packets += 1,
        Err(DecodeThreadSendError::Backpressure(reason)) => {
            panic!("неожиданный backpressure при serial отправке: {reason:?}")
        }
        Err(DecodeThreadSendError::Fatal(error)) => panic!("fatal decoder send: {error}"),
    }

    let deadline = Instant::now() + DECODER_WAIT_TIMEOUT;
    loop {
        collect_frames(decoder, generation, evidence);
        if decoder.drain_completed_packet_count() > 0 {
            return;
        }
        assert_decoder_alive(decoder);
        assert!(Instant::now() < deadline, "decoder packet ACK timeout");
        thread::sleep(Duration::from_millis(1));
    }
}

/// Ждёт, пока каждый отправленный temporal unit опубликует свой кадр.
///
/// Публикация идёт асинхронно после ACK, а seek-flush выбрасывает
/// неопубликованные кадры, поэтому до flush досчитываем их явно.
fn wait_for_all_frames(decoder: &VideoDecodeThread, generation: u64, evidence: &mut PhaseEvidence) {
    let deadline = Instant::now() + DECODER_WAIT_TIMEOUT;
    while evidence.frame_pts.len() < evidence.sent_packets {
        collect_frames(decoder, generation, evidence);
        assert_decoder_alive(decoder);
        assert!(
            Instant::now() < deadline,
            "decoder не опубликовал все кадры фазы"
        );
        thread::sleep(Duration::from_millis(1));
    }
}

/// Завершает фазу EOF-drain-ом текущей generation.
fn drain_decoder_to_eof(
    decoder: &VideoDecodeThread,
    generation: u64,
    evidence: &mut PhaseEvidence,
) {
    let begin_result = decoder.begin_end_of_stream_drain(generation);
    assert!(
        matches!(
            begin_result,
            VideoDecoderEndOfStreamDrainResult::Started(
                VideoDecoderEndOfStreamDrainState::Draining { generation: started }
                    | VideoDecoderEndOfStreamDrainState::Drained { generation: started }
            ) if started == generation
        ),
        "decoder должен принять EOF drain текущей generation, получено {begin_result:?}"
    );

    let deadline = Instant::now() + DECODER_WAIT_TIMEOUT;
    loop {
        collect_frames(decoder, generation, evidence);
        match decoder.end_of_stream_drain_state() {
            VideoDecoderEndOfStreamDrainState::Drained {
                generation: drained,
            } if drained == generation => {
                break;
            }
            VideoDecoderEndOfStreamDrainState::Fatal { error, .. } => {
                panic!("VA-API EOF drain завершился ошибкой: {error}")
            }
            _ => {}
        }
        assert_decoder_alive(decoder);
        assert!(Instant::now() < deadline, "decoder EOF drain timeout");
        thread::sleep(Duration::from_millis(1));
    }
    collect_frames(decoder, generation, evidence);
}

/// Забирает доступные кадры, проверяет generation и сразу освобождает surface.
fn collect_frames(decoder: &VideoDecodeThread, generation: u64, evidence: &mut PhaseEvidence) {
    while let Some(frame) = decoder.try_recv_frame() {
        assert_eq!(
            frame.generation, generation,
            "кадр устаревшей generation после seek"
        );
        evidence.frame_pts.push(frame.pts);
        release_frame(decoder, frame);
    }
}

/// Освобождает кадры, оставшиеся в канале к моменту seek-flush.
fn release_pending_frames(decoder: &VideoDecodeThread) {
    while let Some(frame) = decoder.try_recv_frame() {
        release_frame(decoder, frame);
    }
}

/// Тест не рендерит кадры, поэтому возвращает surface без GPU submission.
fn release_frame(decoder: &VideoDecodeThread, frame: DecodedFrame) {
    decoder.release_frame(frame.resource_handle);
}

/// Падает с причиной, если decoder thread сообщил ошибку.
fn assert_decoder_alive(decoder: &VideoDecodeThread) {
    if let Some(error) = decoder.try_recv_error() {
        panic!("VA-API decoder thread завершился с ошибкой: {error}");
    }
}

/// Общие проверки фазы: кадров ровно столько же, сколько temporal unit-ов.
fn assert_phase_frames(evidence: &PhaseEvidence, phase_name: &str) {
    assert!(
        evidence.sent_packets > 0,
        "{phase_name}: фаза не отправила ни одного пакета"
    );
    assert_eq!(
        evidence.frame_pts.len(),
        evidence.sent_packets,
        "{phase_name}: каждый AV1 temporal unit должен дать ровно один показанный кадр"
    );
}
