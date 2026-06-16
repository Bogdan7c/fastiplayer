use std::path::{Path, PathBuf};
use std::time::Duration;

use anyhow::{Context, Result};
use codec_core::{VideoCodec, parse_hevc_decoder_configuration_record};
use media_core::{
    DemuxReadEvent, DemuxSeekRequest, Demuxer, Packet, PacketKeyframe, TrackId, TrackInfo,
    TrackKind,
};
use symphonia_demux::SymphoniaDemuxer;

#[test]
fn h265_ios_mov_decode_point_before_seek_starts_on_sync_sample() -> Result<()> {
    let fixture_name = "4k60fps/ios-hevc-main10-aac-4k60.mov";
    let Some(mut demuxer) = open_optional_h265_fixture(fixture_name)? else {
        return Ok(());
    };
    let video_track = first_h265_video_track(&mut demuxer)
        .with_context(|| format!("{fixture_name}: expected H.265 video track"))?;
    let target = Duration::from_secs(8);

    let seek_result = demuxer
        .seek_with_request(DemuxSeekRequest::decode_point_before(target))
        .with_context(|| {
            format!("{fixture_name}: DecodePointBefore seek should find MP4 sync sample")
        })?;
    let first_video_packet = collect_video_packets(&mut demuxer, video_track.id, 1)?
        .into_iter()
        .next()
        .with_context(|| format!("{fixture_name}: expected post-seek video packet"))?;

    assert!(
        seek_result.actual_position.as_duration() <= target,
        "{fixture_name}: DecodePointBefore actual position must not pass target"
    );
    assert_eq!(
        seek_result.actual_position.as_duration(),
        first_video_packet.pts,
        "{fixture_name}: seek actual should match verified first video packet"
    );
    assert_eq!(
        first_video_packet.keyframe,
        PacketKeyframe::Keyframe,
        "{fixture_name}: first video packet after seek should be proven HEVC decode-start"
    );
    // RC1: seek должен якориться на БЛИЖАЙШИЙ sync sample перед target (iOS HEVC GOP ~1 c),
    // а не на keyframe за несколько секунд раньше из-за избыточного 5-секундного pre-roll.
    assert!(
        seek_result.actual_position.as_duration()
            >= target.saturating_sub(Duration::from_millis(1_500)),
        "{fixture_name}: DecodePointBefore должен якориться на ближайший keyframe перед target, \
         а не на дальний GOP (actual={:?}, target={target:?})",
        seek_result.actual_position.as_duration(),
    );

    Ok(())
}

#[test]
fn h265_ios_mov_track_exposes_hvcc_codec_private() -> Result<()> {
    let fixture_name = "4k60fps/ios-hevc-main10-aac-4k60.mov";
    let Some(mut demuxer) = open_optional_h265_fixture(fixture_name)? else {
        return Ok(());
    };
    let video_track = first_h265_video_track(&mut demuxer)
        .with_context(|| format!("{fixture_name}: expected H.265 video track"))?;
    let codec_private = video_track
        .codec_private
        .as_deref()
        .with_context(|| format!("{fixture_name}: expected H.265 codec_private/hvcC"))?;

    parse_hevc_decoder_configuration_record(codec_private)
        .with_context(|| format!("{fixture_name}: codec_private should be hvcC"))?;

    Ok(())
}

#[test]
fn h265_mkv_decode_point_before_seek_uses_near_cue_anchor() -> Result<()> {
    let fixture_name = "4k60fps/LXb3EKWsInQ_2160p60_sdr_hevc_main_8bit_mkv_118mbps_aac_20s.mkv";

    for target in [
        Duration::from_secs(3),
        Duration::from_secs(5),
        Duration::from_secs(10),
    ] {
        assert_h265_decode_point_before_uses_near_mkv_cue(fixture_name, target)?;
    }

    Ok(())
}

fn open_h265_fixture(fixture_name: &str) -> Result<SymphoniaDemuxer> {
    let fixture_path = h265_fixture_path(fixture_name);
    SymphoniaDemuxer::from_file(&fixture_path)
        .with_context(|| format!("open H.265 fixture {}", fixture_path.display()))
}

fn open_optional_h265_fixture(fixture_name: &str) -> Result<Option<SymphoniaDemuxer>> {
    let fixture_path = h265_fixture_path(fixture_name);
    if !fixture_path.exists() {
        eprintln!(
            "skipping H.265 fixture {fixture_name}: {} is absent",
            fixture_path.display()
        );
        return Ok(None);
    }

    SymphoniaDemuxer::from_file(&fixture_path)
        .with_context(|| format!("open H.265 fixture {}", fixture_path.display()))
        .map(Some)
}

fn first_h265_video_track(demuxer: &mut SymphoniaDemuxer) -> Option<TrackInfo> {
    demuxer
        .tracks()
        .iter()
        .find(|track| {
            track.kind == TrackKind::Video
                && VideoCodec::from_container_codec_id(&track.codec_id) == Some(VideoCodec::H265)
        })
        .cloned()
}

fn assert_h265_decode_point_before_uses_near_mkv_cue(
    fixture_name: &str,
    target: Duration,
) -> Result<()> {
    let mut demuxer = open_h265_fixture(fixture_name)?;
    let video_track = first_h265_video_track(&mut demuxer)
        .with_context(|| format!("{fixture_name}: expected H.265 video track"))?;

    let seek_result = demuxer
        .seek_with_request(DemuxSeekRequest::decode_point_before(target))
        .with_context(|| {
            format!("{fixture_name}: DecodePointBefore seek should use Matroska video cue")
        })?;
    let first_video_packet = collect_video_packets(&mut demuxer, video_track.id, 1)?
        .into_iter()
        .next()
        .with_context(|| format!("{fixture_name}: expected post-seek video packet"))?;

    assert!(
        seek_result.actual_position.as_duration() <= target,
        "{fixture_name}: DecodePointBefore actual position must not pass target"
    );
    assert_eq!(
        seek_result.actual_position.as_duration(),
        first_video_packet.pts,
        "{fixture_name}: seek actual should match verified first video packet"
    );
    assert_eq!(
        first_video_packet.keyframe,
        PacketKeyframe::Keyframe,
        "{fixture_name}: first video packet after seek should be proven HEVC decode-start"
    );
    assert!(
        seek_result.actual_position.as_duration()
            >= target.saturating_sub(Duration::from_millis(2_500)),
        "{fixture_name}: MKV DecodePointBefore должен выбрать ближайший cue перед target, \
         а не старый 5s backoff (actual={:?}, target={target:?})",
        seek_result.actual_position.as_duration(),
    );

    Ok(())
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

fn h265_fixture_path(fixture_name: &str) -> PathBuf {
    repo_root()
        .join("test-assets")
        .join("h265")
        .join(fixture_name)
}

fn repo_root() -> &'static Path {
    Path::new(env!("CARGO_MANIFEST_DIR"))
        .parent()
        .and_then(Path::parent)
        .expect("symphonia-demux crate must live under crates/")
}
