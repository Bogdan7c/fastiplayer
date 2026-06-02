use std::path::{Path, PathBuf};
use std::time::Duration;

use anyhow::{Context, Result};
use codec_core::{VideoCodec, parse_avc_decoder_configuration_record};
use media_core::{DemuxReadEvent, Demuxer, Packet, PacketKeyframe, TrackId, TrackInfo, TrackKind};
use symphonia_demux::SymphoniaDemuxer;

#[test]
fn h264_mp4_and_mkv_tracks_expose_avcc_codec_private() -> Result<()> {
    for fixture_name in [
        "720p_baseline_nobframes.mp4",
        "<MEDIA_DIR>/h264-720p-main-bframes.mp4",
        "<MEDIA_DIR>/h264-720p-high-bframes-aac.mp4",
        "<MEDIA_DIR>/h264-720p-high-bframes-aac.mkv",
    ] {
        let mut demuxer = open_h264_fixture(fixture_name)?;
        let video_track = first_h264_video_track(&mut demuxer)
            .with_context(|| format!("{fixture_name}: expected H.264 video track"))?;
        let codec_private = video_track
            .codec_private
            .as_deref()
            .with_context(|| format!("{fixture_name}: expected H.264 codec_private/avcC"))?;

        let avcc_record = parse_avc_decoder_configuration_record(codec_private)
            .with_context(|| format!("{fixture_name}: codec_private should be avcC"))?;

        assert!(
            !avcc_record.sequence_parameter_sets().is_empty(),
            "{fixture_name}: avcC must expose SPS"
        );
        assert!(
            !avcc_record.picture_parameter_sets().is_empty(),
            "{fixture_name}: avcC must expose PPS"
        );
    }

    Ok(())
}

#[test]
fn h264_fixtures_get_codec_aware_keyframe_classification() -> Result<()> {
    for fixture_name in [
        "720p_baseline_nobframes.mp4",
        "<MEDIA_DIR>/h264-720p-main-bframes.mp4",
        "<MEDIA_DIR>/h264-720p-high-bframes-aac.mp4",
        "<MEDIA_DIR>/h264-720p-high-bframes-aac.mkv",
    ] {
        let mut demuxer = open_h264_fixture(fixture_name)?;
        let video_track = first_h264_video_track(&mut demuxer)
            .with_context(|| format!("{fixture_name}: expected H.264 video track"))?;
        let keyframe_states = collect_video_keyframe_states(&mut demuxer, video_track.id, 90)
            .with_context(|| format!("{fixture_name}: collect video packets"))?;

        assert!(
            keyframe_states.contains(&PacketKeyframe::Keyframe),
            "{fixture_name}: expected at least one proven IDR/keyframe packet"
        );
        assert!(
            keyframe_states.contains(&PacketKeyframe::NotKeyframe),
            "{fixture_name}: expected at least one proven non-IDR packet"
        );
        assert!(
            !keyframe_states.contains(&PacketKeyframe::Unknown),
            "{fixture_name}: valid H.264 packets should not remain Unknown"
        );
    }

    Ok(())
}

#[test]
fn h264_mp4_bframes_expose_presentation_pts_and_decode_dts() -> Result<()> {
    let mut demuxer = open_h264_fixture("<MEDIA_DIR>/h264-720p-main-bframes.mp4")?;
    let video_track = first_h264_video_track(&mut demuxer)
        .context("<MEDIA_DIR>/h264-720p-main-bframes.mp4: expected H.264 video track")?;
    let video_packets = collect_video_packets(&mut demuxer, video_track.id, 24)
        .context("<MEDIA_DIR>/h264-720p-main-bframes.mp4: collect video packets")?;

    assert!(
        video_packets.iter().any(|packet| packet.dts.is_some()),
        "MP4 B-frame packets must carry separate DTS after ctts is applied"
    );

    let presentation_pts = packet_pts_sequence(&video_packets);
    assert!(
        duration_sequence_has_backward_step(&presentation_pts),
        "MP4 B-frame packet PTS must reflect presentation order, not decode order"
    );

    let decode_timestamps = packet_decode_timestamp_sequence(&video_packets);
    assert!(
        duration_sequence_is_monotonic(&decode_timestamps),
        "MP4 B-frame decode timestamps must remain monotonic in demux packet order"
    );

    Ok(())
}

#[test]
fn h264_mp4_baseline_without_bframes_keeps_pts_and_dts_aligned() -> Result<()> {
    let mut demuxer = open_h264_fixture("720p_baseline_nobframes.mp4")?;
    let video_track = first_h264_video_track(&mut demuxer)
        .context("720p_baseline_nobframes.mp4: expected H.264 video track")?;
    let video_packets = collect_video_packets(&mut demuxer, video_track.id, 24)
        .context("720p_baseline_nobframes.mp4: collect video packets")?;

    assert!(
        video_packets.iter().all(|packet| packet.dts.is_none()),
        "no-B-frame MP4 packets should not invent a separate DTS"
    );

    let presentation_pts = packet_pts_sequence(&video_packets);
    assert!(
        duration_sequence_is_monotonic(&presentation_pts),
        "no-B-frame MP4 packet PTS should stay monotonic"
    );

    Ok(())
}

fn open_h264_fixture(fixture_name: &str) -> Result<SymphoniaDemuxer> {
    let fixture_path = h264_fixture_path(fixture_name);
    SymphoniaDemuxer::from_file(&fixture_path)
        .with_context(|| format!("open H.264 fixture {}", fixture_path.display()))
}

fn first_h264_video_track(demuxer: &mut SymphoniaDemuxer) -> Option<TrackInfo> {
    demuxer
        .tracks()
        .iter()
        .find(|track| {
            track.kind == TrackKind::Video
                && VideoCodec::from_container_codec_id(&track.codec_id) == Some(VideoCodec::H264)
        })
        .cloned()
}

fn collect_video_keyframe_states(
    demuxer: &mut SymphoniaDemuxer,
    video_track_id: TrackId,
    max_video_packets: usize,
) -> Result<Vec<PacketKeyframe>> {
    let mut keyframe_states = Vec::new();

    while keyframe_states.len() < max_video_packets {
        match demuxer.next_event()? {
            DemuxReadEvent::Packet(packet)
                if packet.kind == TrackKind::Video && packet.track_id == video_track_id =>
            {
                keyframe_states.push(packet.keyframe);
            }
            DemuxReadEvent::Packet(_) | DemuxReadEvent::TracksChanged(_) => {}
            DemuxReadEvent::EndOfStream => break,
        }
    }

    Ok(keyframe_states)
}

fn collect_video_packets(
    demuxer: &mut SymphoniaDemuxer,
    video_track_id: TrackId,
    max_video_packets: usize,
) -> Result<Vec<Packet>> {
    let mut video_packets = Vec::new();

    while video_packets.len() < max_video_packets {
        match demuxer.next_event()? {
            DemuxReadEvent::Packet(packet)
                if packet.kind == TrackKind::Video && packet.track_id == video_track_id =>
            {
                video_packets.push(packet);
            }
            DemuxReadEvent::Packet(_) | DemuxReadEvent::TracksChanged(_) => {}
            DemuxReadEvent::EndOfStream => break,
        }
    }

    Ok(video_packets)
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

fn h264_fixture_path(fixture_name: &str) -> PathBuf {
    repo_root()
        .join("test-assets")
        .join("H264")
        .join(fixture_name)
}

fn repo_root() -> &'static Path {
    Path::new(env!("CARGO_MANIFEST_DIR"))
        .parent()
        .and_then(Path::parent)
        .expect("symphonia-demux crate must live under crates/")
}
