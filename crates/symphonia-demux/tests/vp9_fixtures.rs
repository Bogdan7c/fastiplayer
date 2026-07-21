mod support;

use std::path::Path;
use std::time::Duration;

use anyhow::{Context, Result, ensure};
use codec_core::VideoCodec;
use media_core::{
    DemuxReadEvent, DemuxSeekRequest, Demuxer, Packet, PacketKeyframe, TrackId, TrackInfo,
    TrackKind,
};
use support::manual_media::{report_selected_media, selected_media_path};
use symphonia_demux::SymphoniaDemuxer;

#[test]
#[ignore = "manual media regression; use scripts/media-regression.sh"]
fn vp9_decode_point_before_seek_reaches_near_target_keyframe() -> Result<()> {
    let path = selected_media_path()?;
    let mut demuxer = open_vp9_media(&path)?;
    let video_track =
        first_vp9_video_track(&demuxer).context("selected file has no VP9 video track")?;
    let target = Duration::from_nanos(66_932_403_380);
    let seek_result = demuxer.seek_with_request(DemuxSeekRequest::decode_point_before(target))?;
    let accepted_packet = collect_verified_anchor_packet(
        &mut demuxer,
        video_track.id,
        seek_result.actual_position.as_duration(),
    )?
    .context("selected VP9 stream has no verified anchor packet")?;
    ensure!(
        seek_result.actual_position.as_duration() <= target,
        "VP9 decode-point seek passed target"
    );
    ensure!(
        seek_result.actual_position.as_duration() == accepted_packet.pts,
        "VP9 seek actual position does not match anchor PTS"
    );
    ensure!(
        accepted_packet.keyframe == PacketKeyframe::Keyframe,
        "VP9 anchor packet is not a proven keyframe"
    );
    ensure!(
        accepted_packet.pts >= target.saturating_sub(Duration::from_secs(10)),
        "VP9 decode-point seek selected an old cluster"
    );
    Ok(())
}

fn open_vp9_media(path: &Path) -> Result<SymphoniaDemuxer> {
    let demuxer = SymphoniaDemuxer::from_file(path)
        .with_context(|| format!("open selected VP9 media: {}", path.display()))?;
    report_selected_media("vp9-decode-point", path, demuxer.tracks())?;
    Ok(demuxer)
}

fn first_vp9_video_track(demuxer: &SymphoniaDemuxer) -> Option<TrackInfo> {
    demuxer
        .tracks()
        .iter()
        .find(|track| {
            track.kind == TrackKind::Video
                && VideoCodec::from_container_codec_id(&track.codec_id) == Some(VideoCodec::Vp9)
        })
        .cloned()
}

fn collect_verified_anchor_packet(
    demuxer: &mut SymphoniaDemuxer,
    video_track_id: TrackId,
    anchor_pts: Duration,
) -> Result<Option<Packet>> {
    let mut checked_packets = 0_usize;
    while checked_packets < 1_200 {
        match demuxer.next_event()? {
            DemuxReadEvent::Packet(packet)
                if packet.kind == TrackKind::Video && packet.track_id == video_track_id =>
            {
                checked_packets += 1;
                if packet.pts == anchor_pts {
                    return Ok(Some(packet));
                }
            }
            DemuxReadEvent::Packet(_)
            | DemuxReadEvent::TracksChanged(_)
            | DemuxReadEvent::MediaMetadataChanged(_) => {}
            DemuxReadEvent::TemporarilyUnavailable(hint) => {
                anyhow::bail!("finite VP9 fixture неожиданно вернула temporary readiness: {hint:?}")
            }
            DemuxReadEvent::EndOfStream => break,
        }
    }
    Ok(None)
}
