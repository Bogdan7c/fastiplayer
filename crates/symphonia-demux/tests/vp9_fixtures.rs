use std::path::{Path, PathBuf};
use std::time::Duration;

use anyhow::{Context, Result};
use codec_core::VideoCodec;
use media_core::{
    DemuxReadEvent, DemuxSeekRequest, Demuxer, Packet, PacketKeyframe, TrackId, TrackInfo,
    TrackKind,
};
use symphonia_demux::SymphoniaDemuxer;

#[test]
fn vp9_sdr_decode_point_before_seek_reaches_near_target_keyframe() -> Result<()> {
    let fixture_name = "4k60fps_sdr/LXb3EKWsInQ_2160p60_sdr_vp9_opus.webm";
    let mut demuxer = open_vp9_fixture(fixture_name)?;
    let video_track = first_vp9_video_track(&mut demuxer)
        .with_context(|| format!("{fixture_name}: expected VP9 video track"))?;
    let target = Duration::from_nanos(66_932_403_380);

    let seek_result = demuxer
        .seek_with_request(DemuxSeekRequest::decode_point_before(target))
        .with_context(|| {
            format!("{fixture_name}: DecodePointBefore seek should find VP9 keyframe before target")
        })?;
    let accepted_video_packet = collect_verified_anchor_packet(
        &mut demuxer,
        video_track.id,
        seek_result.actual_position.as_duration(),
    )?
    .with_context(|| {
        format!("{fixture_name}: expected verified VP9 keyframe in post-seek replay buffer")
    })?;

    assert!(
        seek_result.actual_position.as_duration() <= target,
        "{fixture_name}: DecodePointBefore actual position must not pass target"
    );
    assert_eq!(
        seek_result.actual_position.as_duration(),
        accepted_video_packet.pts,
        "{fixture_name}: seek actual should match verified anchor video packet"
    );
    assert_eq!(
        accepted_video_packet.keyframe,
        PacketKeyframe::Keyframe,
        "{fixture_name}: accepted anchor packet should be proven VP9 keyframe"
    );
    assert!(
        accepted_video_packet.pts >= target.saturating_sub(Duration::from_secs(10)),
        "{fixture_name}: seek should not fall back to an old WebM cluster \
         (actual={:?}, target={target:?})",
        accepted_video_packet.pts,
    );

    Ok(())
}

fn open_vp9_fixture(fixture_name: &str) -> Result<SymphoniaDemuxer> {
    let fixture_path = vp9_fixture_path(fixture_name);
    SymphoniaDemuxer::from_file(&fixture_path)
        .with_context(|| format!("open VP9 fixture {}", fixture_path.display()))
}

fn first_vp9_video_track(demuxer: &mut SymphoniaDemuxer) -> Option<TrackInfo> {
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
    let mut checked_video_packets = 0_usize;

    while checked_video_packets < 1_200 {
        match demuxer.next_event()? {
            DemuxReadEvent::Packet(packet)
                if packet.kind == TrackKind::Video && packet.track_id == video_track_id =>
            {
                checked_video_packets += 1;
                if packet.pts == anchor_pts {
                    return Ok(Some(packet));
                }
            }
            DemuxReadEvent::Packet(_) | DemuxReadEvent::TracksChanged(_) => {}
            DemuxReadEvent::EndOfStream => break,
        }
    }

    Ok(None)
}

fn vp9_fixture_path(fixture_name: &str) -> PathBuf {
    workspace_root()
        .join("test-assets")
        .join("VP9")
        .join(fixture_name)
}

fn workspace_root() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR"))
        .ancestors()
        .nth(2)
        .expect("symphonia-demux crate should live two levels below workspace root")
        .to_path_buf()
}
