//! Общие помощники hardware-тестов VA-API H.264 сессии 09a.
//!
//! Открытие выбранного файла, H.264-дорожка, stream config как у player-а и
//! проверки ограниченного пула выходных surfaces.

use std::path::PathBuf;
use std::time::Duration;

use codec_core::{
    H264Packetization, VideoCodec, VideoRequirementProbe, parse_avc_decoder_configuration_record,
    probe_video_packet_requirement_with_codec_private,
};
use media_core::{DemuxReadEvent, Demuxer, Packet, TrackInfo, TrackKind};
use symphonia_demux::SymphoniaDemuxer;
use video_core::{VideoStreamDecodeConfig, VideoStreamPacketization};
use video_frame_contract::{DmaBufImageLayout, VideoFrameContract};
use video_vaapi::DecodePacket;
use video_vaapi::output_surface_pool::OutputSurfacePoolStats;

/// Пул ограничен, surfaces переиспользуются, ни одна не вернулась «грязной».
pub fn assert_bounded_pool(stats: &OutputSurfacePoolStats, decoded_frames: usize) {
    assert!(
        stats.peak_live_surfaces <= stats.peak_capacity,
        "live surfaces exceeded capacity: {stats:?}"
    );
    assert!(
        stats.surfaces_created <= stats.peak_capacity as u64,
        "surfaces created beyond capacity (per-frame allocation?): {stats:?}"
    );
    assert!(
        stats.surfaces_created < decoded_frames as u64,
        "surfaces must be recycled, not created per frame: {stats:?} frames={decoded_frames}"
    );
    assert_eq!(
        stats.allocation_failures, 0,
        "driver allocation failed: {stats:?}"
    );
    assert_eq!(
        stats.surfaces_destroyed_not_idle, 0,
        "a surface came back with unknown driver state: {stats:?}"
    );
}

/// Packet демуксера в форме, которую принимает VA-API decoder thread.
pub fn decode_packet(packet: &Packet, generation: u64) -> DecodePacket {
    DecodePacket {
        track_id: packet.track_id,
        pts: packet.pts,
        dts: packet.dts,
        track_dts: packet.track_dts,
        generation,
        encoded_bytes: packet.data.clone(),
        keyframe: packet.keyframe.is_known_keyframe(),
        resolved_color: None,
    }
}

/// PTS packet-а, усечённый до микросекунд, как его передаёт VA-API decoder.
pub fn microsecond_pts(packet: &Packet) -> Duration {
    Duration::from_micros(
        u64::try_from(packet.pts.as_micros()).expect("packet PTS fits u64 microseconds"),
    )
}

/// Читает путь, выбранный `scripts/media-regression.sh`.
pub fn selected_media_path() -> PathBuf {
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
pub fn first_h264_video_track(demuxer: &SymphoniaDemuxer) -> TrackInfo {
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
pub fn next_video_packet(
    demuxer: &mut SymphoniaDemuxer,
    video_track: &TrackInfo,
) -> Option<Packet> {
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
pub fn h264_stream_config(
    video_track: &TrackInfo,
    first_packet: &Packet,
) -> VideoStreamDecodeConfig {
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
