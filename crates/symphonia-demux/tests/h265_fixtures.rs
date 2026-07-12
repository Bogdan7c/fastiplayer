mod support;

use std::path::Path;
use std::time::Duration;

use anyhow::{Context, Result, ensure};
use codec_core::{VideoCodec, parse_hevc_decoder_configuration_record};
use media_core::{
    DemuxReadEvent, DemuxSeekRequest, Demuxer, Packet, PacketKeyframe, TrackId, TrackInfo,
    TrackKind,
};
use support::manual_media::{report_selected_media, selected_media_path};
use symphonia_demux::SymphoniaDemuxer;

#[test]
#[ignore = "manual media regression; use scripts/media-regression.sh"]
fn h265_iso_bmff_decode_point_before_starts_on_sync_sample() -> Result<()> {
    let path = selected_media_path()?;
    let mut demuxer = open_h265_media(&path, "h265-mov-sync-sample")?;
    let video_track =
        first_h265_video_track(&demuxer).context("selected file has no H.265 video track")?;
    let target = Duration::from_secs(8);
    let seek_result = demuxer.seek_with_request(DemuxSeekRequest::decode_point_before(target))?;
    let first_packet = collect_video_packets(&mut demuxer, video_track.id, 1)?
        .into_iter()
        .next()
        .context("selected H.265 stream has no packet after seek")?;
    ensure!(
        seek_result.actual_position.as_duration() <= target,
        "H.265 sync seek passed target"
    );
    ensure!(
        seek_result.actual_position.as_duration() == first_packet.pts,
        "H.265 sync seek actual position does not match first packet PTS"
    );
    ensure!(
        first_packet.keyframe == PacketKeyframe::Keyframe,
        "H.265 sync seek did not start on a proven keyframe"
    );
    ensure!(
        seek_result.actual_position.as_duration()
            >= target.saturating_sub(Duration::from_millis(1_500)),
        "H.265 sync seek selected a distant GOP"
    );
    Ok(())
}

#[test]
#[ignore = "manual media regression; use scripts/media-regression.sh"]
fn h265_iso_bmff_track_exposes_hvcc_codec_private() -> Result<()> {
    let path = selected_media_path()?;
    let demuxer = open_h265_media(&path, "h265-hvcc")?;
    let video_track =
        first_h265_video_track(&demuxer).context("selected file has no H.265 video track")?;
    let codec_private = video_track
        .codec_private
        .as_deref()
        .context("selected H.265 track has no hvcC codec private")?;
    parse_hevc_decoder_configuration_record(codec_private)
        .context("selected H.265 codec private is not hvcC")?;
    Ok(())
}

#[test]
#[ignore = "manual media regression; use scripts/media-regression.sh"]
fn h265_matroska_cue_seek_uses_near_decode_anchor() -> Result<()> {
    let path = selected_media_path()?;
    for target in [
        Duration::ZERO,
        Duration::from_secs(3),
        Duration::from_secs(5),
        Duration::from_secs(10),
    ] {
        assert_h265_decode_point_before_uses_near_mkv_cue(&path, target)?;
    }
    Ok(())
}

fn open_h265_media(path: &Path, scenario: &str) -> Result<SymphoniaDemuxer> {
    let demuxer = SymphoniaDemuxer::from_file(path)
        .with_context(|| format!("open selected H.265 media: {}", path.display()))?;
    report_selected_media(scenario, path, demuxer.tracks())?;
    Ok(demuxer)
}

fn first_h265_video_track(demuxer: &SymphoniaDemuxer) -> Option<TrackInfo> {
    demuxer
        .tracks()
        .iter()
        .find(|track| {
            track.kind == TrackKind::Video
                && VideoCodec::from_container_codec_id(&track.codec_id) == Some(VideoCodec::H265)
        })
        .cloned()
}

fn assert_h265_decode_point_before_uses_near_mkv_cue(path: &Path, target: Duration) -> Result<()> {
    let mut demuxer = open_h265_media(path, "h265-mkv-cue")?;
    let video_track =
        first_h265_video_track(&demuxer).context("selected file has no H.265 video track")?;
    let seek_result = demuxer.seek_with_request(DemuxSeekRequest::decode_point_before(target))?;
    let first_packet = collect_video_packets(&mut demuxer, video_track.id, 1)?
        .into_iter()
        .next()
        .context("selected H.265 stream has no packet after cue seek")?;
    let latest_accepted_position = if target.is_zero() {
        Duration::from_millis(250)
    } else {
        target
    };
    ensure!(
        seek_result.actual_position.as_duration() <= latest_accepted_position,
        "H.265 cue seek passed target/startup tolerance"
    );
    ensure!(
        seek_result.actual_position.as_duration() == first_packet.pts,
        "H.265 cue seek actual position does not match first packet PTS"
    );
    ensure!(
        first_packet.keyframe == PacketKeyframe::Keyframe,
        "H.265 cue seek did not start on a proven keyframe"
    );
    ensure!(
        seek_result.actual_position.as_duration()
            >= target.saturating_sub(Duration::from_millis(2_500)),
        "H.265 cue seek selected a distant anchor"
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
            DemuxReadEvent::EndOfStream => break,
        }
    }
    Ok(packets)
}
