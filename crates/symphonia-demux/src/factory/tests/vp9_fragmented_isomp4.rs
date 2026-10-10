//! VP9 в fragmented MP4 (формат HLS/DASH-вариантов YouTube, например itag 616).
//!
//! Раньше sample entry `vp09` принимался, но codec id трека оставался неизвестным
//! (`unknown_video`), поэтому keyframe-проба не запускалась, а HLS VOD не находил
//! initial seek anchor и не открывался.

use media_core::{PacketKeyframe, TrackKind};

use super::fragmented_isomp4::{next_packet, open_local};
use super::ordered_segments::decode_base64;

/// Генерируется командой
/// `ffmpeg -f lavfi -i testsrc=size=64x64:rate=10 -frames:v 4 -c:v libvpx-vp9 -pix_fmt yuv420p
///  -b:v 50k -g 2 -an -movflags frag_keyframe+empty_moov+default_base_moof -f mp4`:
/// четыре кадра, keyframe-ы на кадрах 0 и 2.
const VP9_FRAGMENTED_MP4_BASE64: &str = include_str!("fixtures/vp9-fragmented.mp4.base64");

#[test]
fn vp9_fragmented_mp4_track_is_recognised_as_vp9_with_profile() {
    let (_fixture, demuxer) = open_local(&decode_base64(VP9_FRAGMENTED_MP4_BASE64));

    let tracks = demuxer.tracks();
    let video = tracks
        .iter()
        .find(|track| track.kind == TrackKind::Video)
        .expect("VP9 fMP4 содержит video track");
    assert_eq!(video.codec_id, "V_VP9");
    assert_eq!(
        video.video.as_ref().and_then(|metadata| metadata.profile),
        Some(codec_core::VideoProfile::Vp9(
            codec_core::Vp9Profile::Profile0
        )),
        "profile обязан приходить из vpcC"
    );
}

#[test]
fn vp9_fragmented_mp4_packets_expose_keyframes() {
    let (_fixture, mut demuxer) = open_local(&decode_base64(VP9_FRAGMENTED_MP4_BASE64));

    let keyframes = (0..4)
        .map(|_| next_packet(demuxer.as_mut()).keyframe)
        .collect::<Vec<_>>();

    assert_eq!(
        keyframes,
        vec![
            PacketKeyframe::Keyframe,
            PacketKeyframe::NotKeyframe,
            PacketKeyframe::Keyframe,
            PacketKeyframe::NotKeyframe,
        ],
        "без распознанного VP9 codec id все packets были бы Unknown, и HLS VOD не нашёл бы anchor"
    );
}
