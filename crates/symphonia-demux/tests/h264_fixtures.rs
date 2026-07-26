mod support;

use std::path::Path;
use std::time::Duration;

use anyhow::{Context, Result, ensure};
use codec_core::{VideoCodec, parse_avc_decoder_configuration_record};
use media_core::{
    DemuxReadEvent, DemuxSeekRequest, Demuxer, Packet, PacketKeyframe, TrackId, TrackInfo,
    TrackKind,
};
use support::manual_media::{report_selected_media, selected_media_path};
use symphonia_demux::SymphoniaDemuxer;

#[test]
#[ignore = "manual media regression; use scripts/media-regression.sh"]
fn h264_avcc_codec_private_is_present() -> Result<()> {
    let path = selected_media_path()?;
    let demuxer = open_h264_media(&path, "h264-avcc")?;
    let video_track =
        first_h264_video_track(&demuxer).context("selected file has no H.264 video track")?;
    let codec_private = video_track
        .codec_private
        .as_deref()
        .context("selected H.264 track has no avcC codec private")?;
    let avcc_record = parse_avc_decoder_configuration_record(codec_private)
        .context("selected H.264 codec private is not avcC")?;
    ensure!(
        !avcc_record.sequence_parameter_sets().is_empty(),
        "selected avcC has no SPS"
    );
    ensure!(
        !avcc_record.picture_parameter_sets().is_empty(),
        "selected avcC has no PPS"
    );
    Ok(())
}

#[test]
#[ignore = "manual media regression; use scripts/media-regression.sh"]
fn h264_packets_have_codec_aware_keyframe_states() -> Result<()> {
    let path = selected_media_path()?;
    let mut demuxer = open_h264_media(&path, "h264-keyframes")?;
    let video_track =
        first_h264_video_track(&demuxer).context("selected file has no H.264 video track")?;
    let keyframe_states = collect_video_packets(&mut demuxer, video_track.id, 90)?
        .into_iter()
        .map(|packet| packet.keyframe)
        .collect::<Vec<_>>();
    ensure!(
        keyframe_states.contains(&PacketKeyframe::Keyframe),
        "selected H.264 stream has no proven keyframe"
    );
    ensure!(
        keyframe_states.contains(&PacketKeyframe::NotKeyframe),
        "selected H.264 stream has no proven inter frame"
    );
    ensure!(
        !keyframe_states.contains(&PacketKeyframe::Unknown),
        "selected H.264 packets remained keyframe-unknown"
    );
    Ok(())
}

#[test]
#[ignore = "manual media regression; use scripts/media-regression.sh"]
fn h264_bframes_keep_presentation_pts_and_decode_dts() -> Result<()> {
    let path = selected_media_path()?;
    let mut demuxer = open_h264_media(&path, "h264-bframes-pts-dts")?;
    let video_track =
        first_h264_video_track(&demuxer).context("selected file has no H.264 video track")?;
    let video_packets = collect_video_packets(&mut demuxer, video_track.id, 24)?;
    ensure!(
        video_packets.iter().any(|packet| packet.dts.is_some()),
        "selected B-frame stream has no separate DTS"
    );
    ensure!(
        duration_sequence_has_backward_step(&packet_pts_sequence(&video_packets)),
        "selected B-frame stream has no presentation-order PTS step"
    );
    ensure!(
        duration_sequence_is_monotonic(&packet_decode_timestamp_sequence(&video_packets)),
        "selected B-frame stream has non-monotonic DTS"
    );
    Ok(())
}

#[test]
#[ignore = "manual media regression; use scripts/media-regression.sh"]
fn h264_signed_ctts_offsets_do_not_wrap_pts() -> Result<()> {
    let path = selected_media_path()?;
    let mut demuxer = open_h264_media(&path, "h264-signed-ctts")?;
    let video_track =
        first_h264_video_track(&demuxer).context("selected file has no H.264 video track")?;
    let video_packets = collect_video_packets(&mut demuxer, video_track.id, 24)?;
    ensure!(
        video_packets.iter().any(|packet| packet.dts.is_some()),
        "selected ctts stream has no separate DTS"
    );
    ensure!(
        video_packets
            .iter()
            .all(|packet| packet.pts < Duration::from_secs(2)),
        "selected ctts stream has wrapped PTS"
    );
    ensure!(
        duration_sequence_has_backward_step(&packet_pts_sequence(&video_packets)),
        "selected ctts stream has no B-frame presentation order"
    );
    ensure!(
        duration_sequence_is_monotonic(&packet_decode_timestamp_sequence(&video_packets)),
        "selected ctts stream has non-monotonic DTS"
    );
    Ok(())
}

#[test]
#[ignore = "manual media regression; use scripts/media-regression.sh"]
fn h264_startup_decode_point_accepts_first_keyframe() -> Result<()> {
    let path = selected_media_path()?;
    let mut demuxer = open_h264_media(&path, "h264-startup-decode-point")?;
    let video_track =
        first_h264_video_track(&demuxer).context("selected file has no H.264 video track")?;
    let seek_result =
        demuxer.seek_with_request(DemuxSeekRequest::decode_point_before(Duration::ZERO))?;
    let first_packet = collect_video_packets(&mut demuxer, video_track.id, 1)?
        .into_iter()
        .next()
        .context("selected H.264 stream has no packet after startup seek")?;
    ensure!(
        seek_result.actual_position.as_duration() <= Duration::from_millis(250),
        "startup decode point is too far from zero"
    );
    ensure!(
        seek_result.actual_position.as_duration() == first_packet.pts,
        "startup seek actual position does not match first packet PTS"
    );
    ensure!(
        first_packet.keyframe == PacketKeyframe::Keyframe,
        "startup packet is not a proven H.264 keyframe"
    );
    Ok(())
}

#[test]
#[ignore = "manual fragmented MP4 regression; use RUSTIPLAYER_MEDIA_PATH"]
fn h264_fragmented_mp4_middle_seek_uses_indexed_anchor() -> Result<()> {
    let path = selected_media_path()?;
    let mut demuxer = open_h264_media(&path, "h264-fragmented-mp4-middle-seek")?;
    let video_track =
        first_h264_video_track(&demuxer).context("selected file has no H.264 video track")?;
    let duration = demuxer
        .duration()
        .context("selected file has no duration")?;
    let target = duration / 2;

    let seek_result = demuxer.seek_with_request(DemuxSeekRequest::decode_point_before(target))?;
    let first_packet = collect_video_packets(&mut demuxer, video_track.id, 1)?
        .into_iter()
        .next()
        .context("selected H.264 stream has no packet after indexed middle seek")?;

    ensure!(
        seek_result.actual_position.as_duration() <= target,
        "indexed middle seek landed after target"
    );
    ensure!(
        first_packet.pts <= target,
        "indexed middle seek first packet landed after target"
    );
    ensure!(
        first_packet.keyframe == PacketKeyframe::Keyframe,
        "indexed middle seek did not start on a proven H.264 keyframe"
    );
    Ok(())
}

#[test]
#[ignore = "manual media regression; use scripts/media-regression.sh"]
fn h264_matroska_cue_seek_uses_near_decode_anchor() -> Result<()> {
    let path = selected_media_path()?;
    for target in [
        Duration::from_secs(3),
        Duration::from_secs(5),
        Duration::from_secs(10),
    ] {
        assert_h264_decode_point_before_uses_near_mkv_cue(&path, target)?;
    }
    Ok(())
}

fn open_h264_media(path: &Path, scenario: &str) -> Result<SymphoniaDemuxer> {
    let demuxer = SymphoniaDemuxer::from_file(path)
        .with_context(|| format!("open selected H.264 media: {}", path.display()))?;
    report_selected_media(scenario, path, demuxer.tracks())?;
    Ok(demuxer)
}

fn first_h264_video_track(demuxer: &SymphoniaDemuxer) -> Option<TrackInfo> {
    demuxer
        .tracks()
        .iter()
        .find(|track| {
            track.kind == TrackKind::Video
                && VideoCodec::from_container_codec_id(&track.codec_id) == Some(VideoCodec::H264)
        })
        .cloned()
}

fn assert_h264_decode_point_before_uses_near_mkv_cue(path: &Path, target: Duration) -> Result<()> {
    let mut demuxer = open_h264_media(path, "h264-mkv-cue")?;
    let video_track =
        first_h264_video_track(&demuxer).context("selected file has no H.264 video track")?;
    let seek_result = demuxer.seek_with_request(DemuxSeekRequest::decode_point_before(target))?;
    let first_packet = collect_video_packets(&mut demuxer, video_track.id, 1)?
        .into_iter()
        .next()
        .context("selected H.264 stream has no packet after cue seek")?;
    ensure!(
        seek_result.actual_position.as_duration() <= target,
        "cue seek actual position passed target"
    );
    ensure!(
        seek_result.actual_position.as_duration() == first_packet.pts,
        "cue seek actual position does not match first packet PTS"
    );
    ensure!(
        first_packet.keyframe == PacketKeyframe::Keyframe,
        "cue seek did not start on a H.264 keyframe"
    );
    ensure!(
        seek_result.actual_position.as_duration()
            >= target.saturating_sub(Duration::from_millis(2_500)),
        "cue seek fell back to a distant anchor"
    );
    Ok(())
}

fn collect_video_packets(
    demuxer: &mut SymphoniaDemuxer,
    video_track_id: TrackId,
    maximum_packets: usize,
) -> Result<Vec<Packet>> {
    let mut packets = Vec::new();
    while packets.len() < maximum_packets {
        match demuxer.next_event()? {
            DemuxReadEvent::Packet(packet)
                if packet.kind == TrackKind::Video && packet.track_id == video_track_id =>
            {
                packets.push(packet)
            }
            DemuxReadEvent::Packet(_)
            | DemuxReadEvent::TracksChanged(_)
            | DemuxReadEvent::MediaMetadataChanged(_) => {}
            DemuxReadEvent::TemporarilyUnavailable(hint) => anyhow::bail!(
                "finite H.264 fixture неожиданно вернула temporary readiness: {hint:?}"
            ),
            DemuxReadEvent::EndOfStream => break,
        }
    }
    Ok(packets)
}

fn packet_pts_sequence(packets: &[Packet]) -> Vec<Duration> {
    packets.iter().map(|packet| packet.pts).collect()
}

fn packet_decode_timestamp_sequence(packets: &[Packet]) -> Vec<Duration> {
    packets
        .iter()
        .map(|packet| packet.dts.unwrap_or(packet.pts))
        .collect()
}

fn duration_sequence_has_backward_step(timestamps: &[Duration]) -> bool {
    timestamps.windows(2).any(|window| window[1] < window[0])
}

fn duration_sequence_is_monotonic(timestamps: &[Duration]) -> bool {
    timestamps.windows(2).all(|window| window[1] >= window[0])
}
