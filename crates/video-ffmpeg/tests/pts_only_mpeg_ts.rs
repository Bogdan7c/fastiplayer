//! Реальная ignored-регрессия для PTS-only MPEG-TS через software FFmpeg.

#![cfg(feature = "ffmpeg")]

use std::path::Path;
use std::thread;
use std::time::{Duration, Instant};

use codec_core::{H264Packetization, VideoCodec, VideoDecodeRequirement};
use demux_api::DemuxInput;
use media_core::{DemuxReadEvent, DemuxSeekRequest, Demuxer, Packet, TrackKind};
use mpeg_ts_demux::{MpegTsDemuxOptions, MpegTsDemuxer};
use source_core::{CancellationToken, LocalFileSource};
use video_core::{
    DecodePacket, DecodeSendError, VideoDecoderEndOfStreamDrainResult,
    VideoDecoderEndOfStreamDrainState, VideoStreamConfigResult, VideoStreamDecodeConfig,
    VideoStreamPacketization,
};
use video_ffmpeg::FfmpegSoftwareVideoBackendFactory;
use video_frame_contract::VideoFrameContract;

/// Максимальное ожидание одного asynchronous decoder шага.
const DECODER_WAIT_TIMEOUT: Duration = Duration::from_secs(5);

/// Результат одного materialized FFmpeg frame-а на render-facing boundary.
#[derive(Debug)]
struct ObservedFrame {
    /// Seek generation, сохранённая decoder thread-ом.
    generation: u64,

    /// Presentation timestamp, который получит scheduler.
    pts: Duration,

    /// Непустой opaque handle доказывает публикацию реального AVFrame-backed ресурса.
    resource_handle: u64,
}

/// Собранные доказательства одного start/seek сценария.
#[derive(Debug)]
struct ScenarioEvidence {
    /// Все materialized кадры, включая EOF/DPB tail.
    frames: Vec<ObservedFrame>,

    /// Число реально прочитанных packets с PTS и без DTS.
    pts_only_packet_count: usize,
}

impl ScenarioEvidence {
    /// Возвращает первые три scheduler PTS в микросекундах для manual evidence log-а.
    fn first_three_pts_micros(&self) -> Vec<u128> {
        self.frames
            .iter()
            .take(3)
            .map(|frame| frame.pts.as_micros())
            .collect()
    }
}

/// Открывает explicit local asset через production MPEG-TS demuxer.
fn open_demuxer(asset_path: &Path) -> MpegTsDemuxer {
    let local_source = LocalFileSource::open(asset_path).expect("открыть AUD-003 MPEG-TS asset");

    MpegTsDemuxer::open(
        DemuxInput::byte_source(Box::new(local_source)),
        CancellationToken::never_cancelled(),
        MpegTsDemuxOptions::default(),
    )
    .expect("открыть production MPEG-TS demuxer")
}

/// Стартует только software FFmpeg и конфигурирует H.264 Annex-B stream.
fn open_software_decoder(
    track_id: media_core::TrackId,
) -> Box<video_backend_api::VideoBackendDecoderThreadHandle> {
    let started_backend = FfmpegSoftwareVideoBackendFactory::new()
        .start_for_composition()
        .expect("запустить software FFmpeg backend");
    assert_eq!(started_backend.backend_id(), "ffmpeg-sw");

    let decoder = started_backend.into_decoder_thread();
    let decode_requirement = VideoDecodeRequirement::new(VideoCodec::H264);
    let stream_config = VideoStreamDecodeConfig::from_requirement(
        track_id,
        &decode_requirement,
        VideoFrameContract::host_yuv420_planar8(),
    )
    .with_packetization(Some(VideoStreamPacketization::H264(
        H264Packetization::AnnexB,
    )));

    assert_eq!(
        decoder.configure_stream(stream_config),
        VideoStreamConfigResult::Configured
    );
    decoder
}

/// Забирает опубликованные кадры и освобождает их обычным renderer release path-ом.
fn drain_available_frames(
    decoder: &video_backend_api::VideoBackendDecoderThreadHandle,
    observed_frames: &mut Vec<ObservedFrame>,
) {
    while let Some(frame) = decoder.try_recv_frame() {
        let resource_handle = frame.resource_handle.0;
        observed_frames.push(ObservedFrame {
            generation: frame.generation,
            pts: frame.pts,
            resource_handle,
        });
        decoder.release_frame(frame.resource_handle);
    }
}

/// Ждёт ACK принятого packet-а, не создавая backpressure для frame publication.
fn wait_for_packet_completion(
    decoder: &video_backend_api::VideoBackendDecoderThreadHandle,
    observed_frames: &mut Vec<ObservedFrame>,
) {
    let deadline = Instant::now() + DECODER_WAIT_TIMEOUT;

    loop {
        drain_available_frames(decoder, observed_frames);
        if decoder.drain_completed_packet_count() > 0 {
            break;
        }
        if let Some(error) = decoder.try_recv_error() {
            panic!("software FFmpeg decoder завершился с ошибкой: {error}");
        }
        assert!(Instant::now() < deadline, "decoder packet ACK timeout");
        thread::sleep(Duration::from_millis(1));
    }

    drain_available_frames(decoder, observed_frames);
}

/// Передаёт demux packet без потери raw PTS через neutral decoder protocol.
fn send_video_packet(
    decoder: &video_backend_api::VideoBackendDecoderThreadHandle,
    generation: u64,
    packet: Packet,
    observed_frames: &mut Vec<ObservedFrame>,
) {
    let decode_packet = DecodePacket {
        track_id: packet.track_id,
        pts: packet.pts,
        dts: packet.dts,
        track_pts: packet.track_pts,
        track_dts: packet.track_dts,
        generation,
        encoded_bytes: packet.data,
        keyframe: packet.keyframe.is_known_keyframe(),
        resolved_color: None,
    };

    match decoder.send_packet(decode_packet) {
        Ok(()) => {}
        Err(DecodeSendError::Backpressure(reason)) => {
            panic!("неожиданный serial decode backpressure: {reason:?}");
        }
        Err(DecodeSendError::Fatal(error)) => panic!("fatal decoder send: {error}"),
    }

    wait_for_packet_completion(decoder, observed_frames);
}

/// Проверяет, что decoder принял EOF и завершил DPB drain для текущей generation.
fn drain_decoder_to_eof(
    decoder: &video_backend_api::VideoBackendDecoderThreadHandle,
    generation: u64,
    observed_frames: &mut Vec<ObservedFrame>,
) {
    let begin_result = decoder.begin_end_of_stream_drain(generation);
    assert!(
        matches!(
            begin_result,
            VideoDecoderEndOfStreamDrainResult::Started(
                VideoDecoderEndOfStreamDrainState::Draining {
                    generation: started_generation
                } | VideoDecoderEndOfStreamDrainState::Drained {
                    generation: started_generation
                }
            ) if started_generation == generation
        ),
        "decoder должен принять EOF drain текущей generation, получено {begin_result:?}"
    );

    let deadline = Instant::now() + DECODER_WAIT_TIMEOUT;
    loop {
        drain_available_frames(decoder, observed_frames);
        match decoder.end_of_stream_drain_state() {
            VideoDecoderEndOfStreamDrainState::Drained {
                generation: drained_generation,
            } if drained_generation == generation => break,
            VideoDecoderEndOfStreamDrainState::Fatal { error, .. } => {
                panic!("software FFmpeg EOF drain завершился с ошибкой: {error}");
            }
            _ => {}
        }
        if let Some(error) = decoder.try_recv_error() {
            panic!("software FFmpeg decoder завершился во время EOF: {error}");
        }
        assert!(Instant::now() < deadline, "decoder EOF drain timeout");
        thread::sleep(Duration::from_millis(1));
    }

    drain_available_frames(decoder, observed_frames);
}

/// Декодирует video track до EOF и считает доказанные PTS-only packets.
fn decode_demux_to_eof(
    demuxer: &mut MpegTsDemuxer,
    decoder: &video_backend_api::VideoBackendDecoderThreadHandle,
    generation: u64,
) -> ScenarioEvidence {
    let mut frames = Vec::new();
    let mut pts_only_packet_count = 0usize;

    loop {
        match demuxer.next_event().expect("прочитать MPEG-TS demux event") {
            DemuxReadEvent::Packet(packet) if packet.kind == TrackKind::Video => {
                if packet.track_pts.is_some() && packet.track_dts.is_none() {
                    pts_only_packet_count = pts_only_packet_count.saturating_add(1);
                }
                send_video_packet(decoder, generation, packet, &mut frames);
            }
            DemuxReadEvent::EndOfStream => break,
            _ => {}
        }
    }

    drain_decoder_to_eof(decoder, generation, &mut frames);
    ScenarioEvidence {
        frames,
        pts_only_packet_count,
    }
}

/// Требует три materialized кадра со строго возрастающими scheduler PTS.
fn assert_first_three_pts_increase(evidence: &ScenarioEvidence, scenario_name: &str) {
    assert!(
        evidence.pts_only_packet_count >= 3,
        "{scenario_name}: fixture должен содержать минимум три PTS-only video packet-а"
    );
    assert!(
        evidence.frames.len() >= 3,
        "{scenario_name}: decoder должен materialize минимум три кадра"
    );

    let first_three_pts = evidence
        .frames
        .iter()
        .take(3)
        .map(|frame| frame.pts)
        .collect::<Vec<_>>();
    assert!(
        first_three_pts.windows(2).all(|pair| pair[0] < pair[1]),
        "{scenario_name}: первые три PTS должны строго расти, получено {first_three_pts:?}"
    );
}

/// Проверяет start, middle seek, current generation и EOF на explicit local fixture.
#[test]
#[ignore = "requires explicit generated PTS-only MPEG-TS and system FFmpeg libraries"]
fn pts_only_mpeg_ts_materializes_increasing_frames_after_start_and_seek() {
    let asset_path = std::env::var_os("FASTIPLAYER_MEDIA_PATH")
        .map(std::path::PathBuf::from)
        .expect("FASTIPLAYER_MEDIA_PATH должен указывать на generated MPEG-TS");

    let mut start_demuxer = open_demuxer(&asset_path);
    let video_track_id = start_demuxer
        .tracks()
        .iter()
        .find(|track| track.kind == TrackKind::Video)
        .map(|track| track.id)
        .expect("MPEG-TS должен содержать video track");
    let start_decoder = open_software_decoder(video_track_id);
    let start_evidence = decode_demux_to_eof(&mut start_demuxer, start_decoder.as_ref(), 1);

    assert_first_three_pts_increase(&start_evidence, "start");
    assert!(
        start_evidence
            .frames
            .iter()
            .all(|frame| frame.generation == 1 && frame.resource_handle > 0),
        "start: каждый кадр должен принадлежать current generation и реальному ресурсу"
    );

    let mut seek_demuxer = open_demuxer(&asset_path);
    let seek_target = Duration::from_secs(2);
    let seek_result = seek_demuxer
        .seek_with_request(DemuxSeekRequest::decode_point_before(seek_target))
        .expect("выполнить middle decode-safe seek");
    assert!(
        seek_result.actual_position.as_duration() <= seek_target,
        "decode point должен находиться не позже requested target"
    );

    let seek_decoder = open_software_decoder(video_track_id);
    let seek_evidence = decode_demux_to_eof(&mut seek_demuxer, seek_decoder.as_ref(), 2);

    assert_first_three_pts_increase(&seek_evidence, "middle seek");
    let first_seek_frame = seek_evidence
        .frames
        .first()
        .expect("middle seek должен materialize первый кадр");
    assert_eq!(first_seek_frame.generation, 2);
    assert!(
        first_seek_frame.pts >= seek_target,
        "первый показанный post-seek кадр должен быть не раньше target"
    );
    assert!(
        seek_evidence
            .frames
            .iter()
            .all(|frame| frame.generation == 2 && frame.resource_handle > 0),
        "middle seek: stale generation и пустые resource handles запрещены"
    );

    eprintln!(
        "AUD003_FIXED start_first_three_us={:?} seek_first_three_us={:?} seek_target_us={}",
        start_evidence.first_three_pts_micros(),
        seek_evidence.first_three_pts_micros(),
        seek_target.as_micros(),
    );
}
