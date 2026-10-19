//! Сквозная ignored-приёмка реального H.264 seek до WGPU submit/release boundary.

// Интеграционный тест целиком является тестовым кодом: unwrap/expect/panic
// здесь работают как assertions. Production-политика паник сюда не относится.
#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::panic,
    reason = "integration test crate: panics are test assertions"
)]
#![cfg(feature = "ffmpeg")]

mod support;

use std::path::Path;
use std::time::Duration;

use codec_core::{H264Packetization, VideoCodec, VideoDecodeRequirement};
use demux_api::DemuxInput;
use media_core::{DemuxReadEvent, DemuxSeekRequest, Demuxer, Packet, TrackKind};
use mpeg_ts_demux::{MpegTsDemuxOptions, MpegTsDemuxer};
use render_wgpu_video::HostPlanarWgpuFrameMaterializer;
use source_core::{CancellationToken, LocalFileSource};
use support::wgpu_acceptance::{
    AcceptanceGeneration, OffscreenWgpuHarness, drain_decoder_to_eof_presentable,
    open_wrapped_decoder, release_unsubmitted_decoder_tail, send_decode_packet_and_collect_frames,
};
use video_backend_api::VideoBackendDecoderThreadHandle;
use video_core::{DecodePacket, DecodedFrame, VideoStreamDecodeConfig, VideoStreamPacketization};
use video_frame_contract::VideoFrameContract;

/// Открывает explicit local synthetic asset через production MPEG-TS demuxer.
fn open_demuxer(asset_path: &Path) -> MpegTsDemuxer {
    let local_source = LocalFileSource::open(asset_path).expect("Open AUD-013 MPEG-TS asset");

    MpegTsDemuxer::open(
        DemuxInput::byte_source(Box::new(local_source)),
        CancellationToken::never_cancelled(),
        MpegTsDemuxOptions::default(),
    )
    .expect("Open production MPEG-TS demuxer")
}

/// Synthetic corpus кодируется H.264 Annex-B внутри MPEG-TS и выводится как YUV420P8.
fn h264_annex_b_stream_config(track_id: media_core::TrackId) -> VideoStreamDecodeConfig {
    let decode_requirement = VideoDecodeRequirement::new(VideoCodec::H264);
    VideoStreamDecodeConfig::from_requirement(
        track_id,
        &decode_requirement,
        VideoFrameContract::host_yuv420_planar8(),
    )
    .with_packetization(Some(VideoStreamPacketization::H264(
        H264Packetization::AnnexB,
    )))
}

/// Переводит demux packet в decoder packet нужного поколения и отправляет его.
fn send_packet_and_collect_frames(
    decoder: &VideoBackendDecoderThreadHandle,
    generation: AcceptanceGeneration,
    packet: Packet,
) -> Vec<DecodedFrame> {
    let decode_packet = DecodePacket {
        track_id: packet.track_id,
        pts: packet.pts,
        dts: packet.dts,
        track_pts: packet.track_pts,
        track_dts: packet.track_dts,
        generation: generation.value(),
        encoded_bytes: packet.data,
        keyframe: packet.keyframe.is_known_keyframe(),
        resolved_color: None,
    };
    send_decode_packet_and_collect_frames(decoder, decode_packet)
}

/// Читает production demux до первого current-generation frame не раньше нижней PTS-границы.
fn decode_first_presentable_frame(
    demuxer: &mut MpegTsDemuxer,
    decoder: &VideoBackendDecoderThreadHandle,
    generation: AcceptanceGeneration,
    minimum_pts: Duration,
) -> DecodedFrame {
    loop {
        match demuxer
            .next_event()
            .expect("Read more about MPEG-TS demux event")
        {
            DemuxReadEvent::Packet(packet) if packet.kind == TrackKind::Video => {
                let mut decoded_frames =
                    send_packet_and_collect_frames(decoder, generation, packet);
                let presentable_index = decoded_frames.iter().position(|frame| {
                    frame.generation == generation.value() && frame.pts >= minimum_pts
                });

                if let Some(presentable_index) = presentable_index {
                    let presentable_frame = decoded_frames.remove(presentable_index);
                    for unsubmitted_frame in decoded_frames {
                        decoder.release_frame(unsubmitted_frame.resource_handle);
                    }
                    return presentable_frame;
                }

                for unsubmitted_frame in decoded_frames {
                    assert_eq!(
                        unsubmitted_frame.generation,
                        generation.value(),
                        "decoder don't publish stale generation"
                    );
                    decoder.release_frame(unsubmitted_frame.resource_handle);
                }
            }
            DemuxReadEvent::EndOfStream => {
                // FFmpeg вправе удерживать последний decoded frame до explicit EOF drain.
                return drain_decoder_to_eof_presentable(decoder, generation, minimum_pts)
                    .unwrap_or_else(|| {
                        panic!(
                            "MPEG-TS terminated before current-generation frame >= {minimum_pts:?}"
                        )
                    });
            }
            _ => {}
        }
    }
}

/// Доказывает real compressed start и nonzero seek до production renderer submit boundary.
#[test]
#[ignore = "requires explicit generated MPEG-TS, system FFmpeg libraries and Vulkan adapter"]
fn h264_mpeg_ts_reaches_wgpu_submit_and_release_before_and_after_seek() {
    // Corpus передаётся явно: тест ничего не скачивает и не использует неясные fixtures.
    let asset_path = std::env::var_os("FASTIPLAYER_MEDIA_PATH")
        .map(std::path::PathBuf::from)
        .expect("FASTIPLAYER_MEDIA_PATH should indicate generated MPEG-TS");

    // Один demuxer и один decoder сохраняют production seek lifecycle между поколениями.
    let mut demuxer = open_demuxer(&asset_path);
    let video_track_id = demuxer
        .tracks()
        .iter()
        .find(|track| track.kind == TrackKind::Video)
        .map(|track| track.id)
        .expect("MPEG-TS should contain video track");

    // Headless target проверяет renderer без window/compositor нестабильности.
    let mut wgpu_harness = OffscreenWgpuHarness::new();
    let (decoder, renderer_provider) = open_wrapped_decoder(
        h264_annex_b_stream_config(video_track_id),
        &wgpu_harness.queue,
    );
    let materializer = HostPlanarWgpuFrameMaterializer::new(
        &wgpu_harness.device,
        &wgpu_harness.queue,
        renderer_provider.clone(),
    );

    // Первый кадр обязан пройти полную вертикаль в generation 1.
    let before_seek_frame = decode_first_presentable_frame(
        &mut demuxer,
        decoder.as_ref(),
        AcceptanceGeneration::BeforeSeek,
        Duration::ZERO,
    );
    let before_seek_evidence =
        wgpu_harness.submit_and_release(&materializer, &renderer_provider, before_seek_frame);
    assert_eq!(
        before_seek_evidence.generation,
        AcceptanceGeneration::BeforeSeek.value()
    );
    assert!(before_seek_evidence.contains_visible_video);

    // Production player сначала flush-ит decoder, затем повышает generation и seek-ит demuxer.
    release_unsubmitted_decoder_tail(decoder.as_ref());
    decoder
        .flush()
        .expect("flush software decoder beforehand seek");
    let seek_target = Duration::from_secs(2);
    let seek_result = demuxer
        .seek_with_request(DemuxSeekRequest::decode_point_before(seek_target))
        .expect("fulfill nonzero decode-safe seek");
    assert!(seek_result.actual_position.as_duration() <= seek_target);

    // Кадр generation 2 должен быть не раньше target и пройти те же production boundaries.
    let after_seek_frame = decode_first_presentable_frame(
        &mut demuxer,
        decoder.as_ref(),
        AcceptanceGeneration::AfterSeek,
        seek_target,
    );
    let after_seek_evidence =
        wgpu_harness.submit_and_release(&materializer, &renderer_provider, after_seek_frame);
    assert_eq!(
        after_seek_evidence.generation,
        AcceptanceGeneration::AfterSeek.value()
    );
    assert!(after_seek_evidence.pts >= seek_target);
    assert!(after_seek_evidence.contains_visible_video);
    // Числовой handle может корректно переиспользоваться после proven release; generation — fence.

    // Evidence marker позволяет CI и аудитору отличить полный submit от packet/decode PASS.
    eprintln!(
        "AUD013_FIXED before_generation={} before_pts_us={} after_generation={} after_pts_us={} before_handle={} after_handle={} materializer=host-planar-wgpu renderer=wgpu-video submit=completed release=completed",
        before_seek_evidence.generation,
        before_seek_evidence.pts.as_micros(),
        after_seek_evidence.generation,
        after_seek_evidence.pts.as_micros(),
        before_seek_evidence.resource_handle.0,
        after_seek_evidence.resource_handle.0,
    );
}
