use std::collections::HashMap;

use codec_core::{
    Av1Profile, BitDepth, ChromaSubsampling, ColorPrimaries, ColorRange, H264Profile, HdrMetadata,
    MatrixCoefficients, TransferFunction, VideoColorMetadata, VideoDisplayOrientation,
    VideoProfile, Vp9Profile,
};
use media_core::{TrackId, TrackKind, VideoPacketFraming, VideoTrackMetadata};
use symphonia::core::codecs::CodecParameters;
use symphonia::core::codecs::audio::AudioCodecParameters;
use symphonia::core::codecs::audio::well_known as audio_codec;
use symphonia::core::codecs::subtitle::SubtitleCodecParameters;
use symphonia::core::codecs::subtitle::well_known as subtitle_codec;
use symphonia::core::codecs::video::well_known::extra_data::VIDEO_EXTRA_DATA_ID_AV1_DECODER_CONFIG;
use symphonia::core::codecs::video::well_known::profiles as video_profile;
use symphonia::core::codecs::video::{VideoCodecParameters, VideoExtraData};
use symphonia::core::formats::Track;
use symphonia::core::units::{Duration as SymphoniaDuration, TimeBase};

use super::{
    TrackEntryKind, UnsupportedTrackKind, build_track_entry, h264_profile_from_symphonia,
    map_tracks, map_tracks_with_video_metadata, parse_opus_head,
    take_matroska_video_track_for_mapping, tracks_may_need_matroska_video_metadata,
};
use crate::matroska_metadata::MatroskaVideoTrack;

fn null_video_track(track_id: u32) -> Track {
    let mut track = Track::new(track_id);
    track.with_time_base(TimeBase::try_new(1, 1_000).expect("valid time base"));
    track
}

fn audio_track_with_opus_head(track_id: u32) -> Track {
    let mut opus_head = [0_u8; 19];
    opus_head[0..8].copy_from_slice(b"OpusHead");
    opus_head[8] = 1;
    opus_head[9] = 2;
    opus_head[12..16].copy_from_slice(&44_100_u32.to_le_bytes());

    let mut audio_params = AudioCodecParameters::new();
    audio_params.with_sample_rate(44_100);
    audio_params.with_extra_data(opus_head.to_vec().into_boxed_slice());

    let mut track = Track::new(track_id);
    track.with_codec_params(CodecParameters::Audio(audio_params));
    track
}

fn audio_track_with_codec(
    track_id: u32,
    codec: symphonia::core::codecs::audio::AudioCodecId,
) -> Track {
    let mut audio_params = AudioCodecParameters::new();
    audio_params.for_codec(codec);

    let mut track = Track::new(track_id);
    track.with_codec_params(CodecParameters::Audio(audio_params));
    track
}

fn audio_track_with_unknown_codec(track_id: u32) -> Track {
    let audio_params = AudioCodecParameters::new();

    let mut track = Track::new(track_id);
    track.with_codec_params(CodecParameters::Audio(audio_params));
    track
}

fn subtitle_track(track_id: u32) -> Track {
    let mut subtitle_params = SubtitleCodecParameters::new();
    subtitle_params.for_codec(subtitle_codec::CODEC_ID_WEBVTT);

    let mut track = Track::new(track_id);
    track.with_codec_params(CodecParameters::Subtitle(subtitle_params));
    track
}

fn vp9_video_track(track_id: u32) -> Track {
    let mut video_params = VideoCodecParameters::default();
    video_params.for_codec(symphonia::core::codecs::video::well_known::CODEC_ID_VP9);

    let mut track = Track::new(track_id);
    track.with_codec_params(CodecParameters::Video(video_params));
    track.with_time_base(TimeBase::try_new(1, 1_000).expect("valid time base"));
    track
}

/// Строит MP4-подобный AV1 track с typed `av1C` extra data.
fn av1_video_track(track_id: u32, configuration_header: [u8; 4]) -> Track {
    let mut video_params = VideoCodecParameters::default();
    video_params.for_codec(symphonia::core::codecs::video::well_known::CODEC_ID_AV1);
    video_params.add_extra_data(VideoExtraData {
        id: VIDEO_EXTRA_DATA_ID_AV1_DECODER_CONFIG,
        data: Vec::from(configuration_header).into_boxed_slice(),
    });

    let mut track = Track::new(track_id);
    track.with_codec_params(CodecParameters::Video(video_params));
    track.with_time_base(TimeBase::try_new(1, 1_000).expect("valid time base"));
    track
}

fn video_track_with_unknown_codec(track_id: u32) -> Track {
    let video_params = VideoCodecParameters::default();

    let mut track = Track::new(track_id);
    track.with_codec_params(CodecParameters::Video(video_params));
    track.with_time_base(TimeBase::try_new(1, 1_000).expect("valid time base"));
    track
}

fn vp9_video_track_with_dimensions(track_id: u32, width: u16, height: u16) -> Track {
    let mut video_params = VideoCodecParameters::default();
    video_params.for_codec(symphonia::core::codecs::video::well_known::CODEC_ID_VP9);
    video_params.with_width(width);
    video_params.with_height(height);
    video_params
        .with_profile(symphonia::core::codecs::video::well_known::profiles::CODEC_PROFILE_VP9_2);

    let mut track = Track::new(track_id);
    track.with_codec_params(CodecParameters::Video(video_params));
    track.with_time_base(TimeBase::try_new(1, 1_000).expect("valid time base"));
    track
}

fn vp9_video_track_with_timing(
    track_id: u32,
    duration: Option<SymphoniaDuration>,
    num_frames: Option<u64>,
) -> Track {
    let mut track = vp9_video_track(track_id);
    if let Some(duration) = duration {
        track.with_duration(duration);
    }
    if let Some(num_frames) = num_frames {
        track.with_num_frames(num_frames);
    }
    track
}

fn video_track_metadata(width: u32, height: Option<u32>) -> VideoTrackMetadata {
    VideoTrackMetadata {
        packet_framing: VideoPacketFraming::Unspecified,
        coded_width: Some(width),
        coded_height: height,
        profile: None,
        bit_depth: None,
        chroma: None,
        color: None,
        orientation: VideoDisplayOrientation::Identity,
    }
}

fn matroska_video_track(metadata: VideoTrackMetadata) -> MatroskaVideoTrack {
    MatroskaVideoTrack {
        codec_id: Some("V_VP9".to_string()),
        metadata: Some(metadata),
    }
}

fn hdr_video_track_metadata() -> VideoTrackMetadata {
    VideoTrackMetadata {
        packet_framing: VideoPacketFraming::Unspecified,
        coded_width: None,
        coded_height: None,
        profile: None,
        bit_depth: None,
        chroma: None,
        color: Some(VideoColorMetadata::container(
            ColorRange::Limited,
            MatrixCoefficients::Bt2020,
            ColorPrimaries::Bt2020,
            TransferFunction::Pq,
            Some(HdrMetadata {
                color_primaries: ColorPrimaries::Bt2020,
                transfer_function: TransferFunction::Pq,
                max_luminance_nits: Some(1_000.0),
                min_luminance_nits: Some(0.001),
                max_content_light_level_nits: Some(1_000),
                max_frame_average_light_level_nits: Some(400),
            }),
        )),
        orientation: VideoDisplayOrientation::Identity,
    }
}

fn mp4_hdr_color_metadata() -> VideoColorMetadata {
    VideoColorMetadata::container(
        ColorRange::Full,
        MatrixCoefficients::Bt2020,
        ColorPrimaries::Bt2020,
        TransferFunction::Pq,
        Some(HdrMetadata {
            color_primaries: ColorPrimaries::Bt2020,
            transfer_function: TransferFunction::Pq,
            max_luminance_nits: Some(4_000.0),
            min_luminance_nits: Some(0.005),
            max_content_light_level_nits: Some(2_000),
            max_frame_average_light_level_nits: Some(800),
        }),
    )
}

#[test]
fn unknown_track_without_video_metadata_is_not_assumed_to_be_video() {
    let track = null_video_track(1);

    let entry = build_track_entry(&track, None);

    assert_eq!(
        entry.kind,
        TrackEntryKind::Unsupported(UnsupportedTrackKind::Unknown)
    );
    assert_eq!(entry.codec_id, "unsupported_track");
}

#[test]
fn audio_only_track_does_not_become_video() {
    let mut metadata_by_track = HashMap::new();
    let mapping = map_tracks(
        &[audio_track_with_codec(2, audio_codec::CODEC_ID_AAC)],
        &mut metadata_by_track,
    );

    assert_eq!(mapping.tracks.len(), 1);
    assert_eq!(mapping.tracks[0].kind, TrackKind::Audio);
    assert_eq!(mapping.tracks[0].codec_id, "A_AAC");
    assert!(mapping.tracks[0].video.is_none());
    assert_eq!(
        mapping
            .track_map
            .get(&2)
            .and_then(|entry| entry.supported_kind()),
        Some(TrackKind::Audio)
    );
}

#[test]
fn audio_only_tracks_do_not_request_matroska_video_pre_scan() {
    let audio_track = audio_track_with_codec(2, audio_codec::CODEC_ID_AAC);

    assert!(!tracks_may_need_matroska_video_metadata(&[audio_track]));
}

#[test]
fn subtitle_track_is_skipped_in_public_tracks() {
    let mut metadata_by_track = HashMap::new();
    let mapping = map_tracks(&[subtitle_track(9)], &mut metadata_by_track);

    assert!(mapping.tracks.is_empty());
    assert_eq!(
        mapping.track_map.get(&9).map(|entry| entry.kind),
        Some(TrackEntryKind::Unsupported(UnsupportedTrackKind::Subtitle))
    );
}

#[test]
fn explicit_matroska_video_codec_id_wins_over_symphonia_null_codec() {
    let track = null_video_track(1);
    let matroska_video_track = MatroskaVideoTrack {
        codec_id: Some("v_vp9".to_string()),
        metadata: None,
    };

    let entry = build_track_entry(&track, Some(&matroska_video_track));

    assert_eq!(entry.supported_kind(), Some(TrackKind::Video));
    assert_eq!(entry.codec_id, "V_VP9");
}

#[test]
fn symphonia_video_codec_maps_to_container_codec_id() {
    let track = vp9_video_track(1);

    let entry = build_track_entry(&track, None);

    assert_eq!(entry.supported_kind(), Some(TrackKind::Video));
    assert_eq!(entry.codec_id, "V_VP9");
}

#[test]
fn symphonia_h264_baseline_profile_preserves_exact_neutral_profile() {
    assert_eq!(
        h264_profile_from_symphonia(video_profile::CODEC_PROFILE_H264_BASELINE),
        Some(H264Profile::Baseline)
    );
    assert_eq!(
        h264_profile_from_symphonia(video_profile::CODEC_PROFILE_H264_CONSTRAINED_BASELINE),
        Some(H264Profile::ConstrainedBaseline)
    );
}

#[test]
fn unknown_audio_codec_is_not_masked_as_opus() {
    let track = audio_track_with_unknown_codec(2);

    let entry = build_track_entry(&track, None);

    assert_eq!(entry.supported_kind(), Some(TrackKind::Audio));
    assert_eq!(entry.codec_id, "unknown_audio");
}

#[test]
fn unknown_video_codec_is_not_masked_as_vp9() {
    let track = video_track_with_unknown_codec(1);

    let entry = build_track_entry(&track, None);

    assert_eq!(entry.supported_kind(), Some(TrackKind::Video));
    assert_eq!(entry.codec_id, "unknown_video");
}

#[test]
fn opus_head_original_input_rate_does_not_override_playback_rate() {
    let track = audio_track_with_opus_head(2);

    let entry = build_track_entry(&track, None);

    assert_eq!(entry.supported_kind(), Some(TrackKind::Audio));
    assert_eq!(entry.sample_rate, Some(48_000));
    assert_eq!(entry.channels, Some(2));
}

#[test]
fn opus_head_accepts_unspecified_original_input_rate() {
    let mut opus_head = [0_u8; 19];
    opus_head[0..8].copy_from_slice(b"OpusHead");
    opus_head[8] = 1;
    opus_head[9] = 1;

    assert_eq!(parse_opus_head(&opus_head), Some((48_000, 1)));
}

#[test]
fn video_metadata_exact_track_id_match_is_used_first() {
    let mut metadata_by_track = HashMap::from([(
        TrackId::new(7),
        matroska_video_track(video_track_metadata(3840, None)),
    )]);

    let video_track = take_matroska_video_track_for_mapping(
        TrackId::new(7),
        TrackEntryKind::Supported(TrackKind::Video),
        2,
        &mut metadata_by_track,
    )
    .expect("exact video track metadata должна быть найдена");
    let metadata = video_track.metadata.expect("video metadata должна быть");

    assert_eq!(metadata.coded_width, Some(3840));
    assert!(metadata_by_track.is_empty());
}

#[test]
fn single_matroska_video_metadata_entry_can_fallback_to_symphonia_track_id() {
    let mut metadata_by_track = HashMap::from([(
        TrackId::new(1),
        matroska_video_track(video_track_metadata(3840, Some(2160))),
    )]);

    let video_track = take_matroska_video_track_for_mapping(
        TrackId::new(0),
        TrackEntryKind::Supported(TrackKind::Video),
        2,
        &mut metadata_by_track,
    )
    .expect("single video track metadata fallback должен сработать");
    let metadata = video_track.metadata.expect("video metadata должна быть");

    assert_eq!(metadata.coded_height, Some(2160));
    assert!(metadata_by_track.is_empty());
}

#[test]
fn multiple_unmatched_video_metadata_entries_do_not_fallback() {
    let mut metadata_by_track = HashMap::from([
        (
            TrackId::new(1),
            matroska_video_track(VideoTrackMetadata::empty()),
        ),
        (
            TrackId::new(2),
            matroska_video_track(VideoTrackMetadata::empty()),
        ),
    ]);

    let metadata = take_matroska_video_track_for_mapping(
        TrackId::new(0),
        TrackEntryKind::Supported(TrackKind::Video),
        2,
        &mut metadata_by_track,
    );

    assert!(metadata.is_none());
    assert_eq!(metadata_by_track.len(), 2);
}

#[test]
fn map_tracks_preserves_matroska_video_metadata() {
    let mut metadata_by_track = HashMap::from([(
        TrackId::new(1),
        matroska_video_track(video_track_metadata(1920, Some(1080))),
    )]);

    let mapping = map_tracks(&[vp9_video_track(1)], &mut metadata_by_track);

    assert_eq!(mapping.tracks.len(), 1);
    assert_eq!(mapping.tracks[0].codec_id, "V_VP9");
    assert_eq!(
        mapping.tracks[0]
            .video
            .as_ref()
            .and_then(|metadata| metadata.coded_height),
        Some(1080)
    );
    assert!(mapping.track_map.contains_key(&1));
}

#[test]
fn map_tracks_preserves_display_orientation_metadata() {
    let mut metadata_by_track = HashMap::new();
    let display_orientations_by_track =
        HashMap::from([(TrackId::new(1), VideoDisplayOrientation::Rotate270Clockwise)]);

    let mapping = map_tracks_with_video_metadata(
        &[vp9_video_track(1)],
        &mut metadata_by_track,
        &display_orientations_by_track,
        &HashMap::new(),
        &HashMap::new(),
    );
    let video_metadata = mapping.tracks[0]
        .video
        .as_ref()
        .expect("orientation сама по себе является video metadata");

    assert_eq!(
        video_metadata.orientation,
        VideoDisplayOrientation::Rotate270Clockwise
    );
}

#[test]
fn avc3_metadata_preserves_in_band_parameter_set_framing() {
    let mut video_params = VideoCodecParameters::default();
    video_params
        .for_codec(symphonia::core::codecs::video::well_known::CODEC_ID_H264)
        .add_extra_data(VideoExtraData {
            data: vec![0x01, 0x4d, 0x40, 0x1f, 0xff, 0xe0, 0x00].into_boxed_slice(),
            ..Default::default()
        });
    let mut track = Track::new(7);
    track.with_codec_params(CodecParameters::Video(video_params));
    let packet_framings_by_track = HashMap::from([(
        TrackId::new(7),
        VideoPacketFraming::LengthPrefixedWithInBandParameterSets,
    )]);

    let mapping = map_tracks_with_video_metadata(
        &[track],
        &mut HashMap::new(),
        &HashMap::new(),
        &HashMap::new(),
        &packet_framings_by_track,
    );

    assert_eq!(mapping.tracks[0].codec_id, "V_MPEG4/ISO/AVC");
    assert_eq!(
        mapping.tracks[0]
            .video
            .as_ref()
            .expect("H.264 track должен иметь video metadata")
            .packet_framing,
        VideoPacketFraming::LengthPrefixedWithInBandParameterSets
    );
}

#[test]
fn avc3_metadata_cannot_relabel_non_h264_video_framing() {
    let track_id = TrackId::new(8);
    let packet_framings_by_track = HashMap::from([(
        track_id,
        VideoPacketFraming::LengthPrefixedWithInBandParameterSets,
    )]);

    let mapping = map_tracks_with_video_metadata(
        &[vp9_video_track(track_id.get())],
        &mut HashMap::new(),
        &HashMap::new(),
        &HashMap::new(),
        &packet_framings_by_track,
    );

    assert_eq!(mapping.tracks[0].codec_id, "V_VP9");
    assert!(
        mapping.tracks[0].video.is_none(),
        "чужой avc3 tag не должен даже создавать video metadata для VP9"
    );
}

#[test]
fn symphonia_video_dimensions_are_used_without_matroska_metadata() {
    let mut metadata_by_track = HashMap::new();
    let track = vp9_video_track_with_dimensions(1, 1920, 1080);

    let mapping = map_tracks(&[track], &mut metadata_by_track);
    let video_metadata = mapping.tracks[0]
        .video
        .as_ref()
        .expect("Symphonia video metadata должна попасть в neutral model");

    assert_eq!(video_metadata.coded_width, Some(1920));
    assert_eq!(video_metadata.coded_height, Some(1080));
    assert_eq!(
        video_metadata.profile,
        Some(VideoProfile::Vp9(Vp9Profile::Profile2))
    );
}

/// Не теряет 8-bit/4:2:0 поля из `av1C` реального SDR fixture-а.
#[test]
fn mp4_av1c_sdr_metadata_reaches_neutral_track() {
    let mut metadata_by_track = HashMap::new();
    let track = av1_video_track(1, [0x81, 0x0d, 0x0c, 0x00]);

    let mapping = map_tracks(&[track], &mut metadata_by_track);
    let video_metadata = mapping.tracks[0]
        .video
        .as_ref()
        .expect("SDR AV1 track должен содержать neutral video metadata");

    assert_eq!(
        video_metadata.profile,
        Some(VideoProfile::Av1(Av1Profile::Main))
    );
    assert_eq!(video_metadata.bit_depth, Some(BitDepth::Eight));
    assert_eq!(video_metadata.chroma, Some(ChromaSubsampling::Yuv420));
}

/// Не теряет 10-bit/4:2:0 поля `av1C` при отдельной MP4 HDR color metadata.
#[test]
fn mp4_av1c_hdr_metadata_reaches_neutral_track() {
    let mut matroska_metadata_by_track = HashMap::new();
    let display_orientations_by_track = HashMap::new();
    let expected_color_metadata = mp4_hdr_color_metadata();
    let color_metadata_by_track =
        HashMap::from([(TrackId::new(1), expected_color_metadata.clone())]);
    let track = av1_video_track(1, [0x81, 0x0d, 0x4c, 0x00]);

    let mapping = map_tracks_with_video_metadata(
        &[track],
        &mut matroska_metadata_by_track,
        &display_orientations_by_track,
        &color_metadata_by_track,
        &HashMap::new(),
    );
    let video_metadata = mapping.tracks[0]
        .video
        .as_ref()
        .expect("HDR AV1 track должен содержать neutral video metadata");

    assert_eq!(
        video_metadata.profile,
        Some(VideoProfile::Av1(Av1Profile::Main))
    );
    assert_eq!(video_metadata.bit_depth, Some(BitDepth::Ten));
    assert_eq!(video_metadata.chroma, Some(ChromaSubsampling::Yuv420));
    assert_eq!(video_metadata.color, Some(expected_color_metadata));
}

#[test]
fn matroska_hdr_fallback_is_merged_with_symphonia_video_metadata() {
    let mut metadata_by_track = HashMap::from([(
        TrackId::new(1),
        matroska_video_track(hdr_video_track_metadata()),
    )]);
    let track = vp9_video_track_with_dimensions(1, 1920, 1080);

    let mapping = map_tracks(&[track], &mut metadata_by_track);
    let video_metadata = mapping.tracks[0]
        .video
        .as_ref()
        .expect("video metadata должна быть объединена");
    let color = video_metadata
        .color
        .as_ref()
        .expect("Matroska HDR fallback должен сохранить color metadata");

    assert_eq!(video_metadata.coded_width, Some(1920));
    assert_eq!(video_metadata.coded_height, Some(1080));
    assert_eq!(color.transfer, TransferFunction::Pq);
    assert_eq!(
        color
            .hdr_metadata
            .as_ref()
            .and_then(|metadata| metadata.max_content_light_level_nits),
        Some(1_000)
    );
}

#[test]
fn mp4_color_metadata_wins_over_matroska_color_fallback() {
    let mut metadata_by_track = HashMap::from([(
        TrackId::new(1),
        matroska_video_track(hdr_video_track_metadata()),
    )]);
    let color_metadata_by_track = HashMap::from([(TrackId::new(1), mp4_hdr_color_metadata())]);
    let track = vp9_video_track_with_dimensions(1, 1920, 1080);

    let mapping = map_tracks_with_video_metadata(
        &[track],
        &mut metadata_by_track,
        &HashMap::new(),
        &color_metadata_by_track,
        &HashMap::new(),
    );
    let video_metadata = mapping.tracks[0]
        .video
        .as_ref()
        .expect("MP4 color metadata должна создать video metadata");
    let color = video_metadata
        .color
        .as_ref()
        .expect("MP4 color metadata должна попасть в neutral model");

    assert_eq!(video_metadata.coded_width, Some(1920));
    assert_eq!(video_metadata.coded_height, Some(1080));
    assert_eq!(color.range, ColorRange::Full);
    assert_eq!(
        color
            .hdr_metadata
            .as_ref()
            .and_then(|metadata| metadata.max_content_light_level_nits),
        Some(2_000)
    );
}

#[test]
fn single_unknown_track_can_use_matroska_video_fallback() {
    let mut metadata_by_track = HashMap::from([(
        TrackId::new(1),
        matroska_video_track(video_track_metadata(3840, Some(2160))),
    )]);

    let mapping = map_tracks(&[null_video_track(0)], &mut metadata_by_track);

    assert_eq!(mapping.tracks.len(), 1);
    assert_eq!(mapping.tracks[0].kind, TrackKind::Video);
    assert_eq!(
        mapping.tracks[0]
            .video
            .as_ref()
            .and_then(|metadata| metadata.coded_width),
        Some(3840)
    );
}

#[test]
fn single_unknown_video_candidate_can_fallback_when_audio_track_exists() {
    let mut metadata_by_track = HashMap::from([(
        TrackId::new(1),
        matroska_video_track(video_track_metadata(1920, Some(1080))),
    )]);
    let audio_track = audio_track_with_codec(2, audio_codec::CODEC_ID_AAC);
    let unknown_video_track = null_video_track(0);

    let mapping = map_tracks(&[audio_track, unknown_video_track], &mut metadata_by_track);

    assert_eq!(mapping.tracks.len(), 2);
    assert_eq!(mapping.tracks[0].kind, TrackKind::Audio);
    assert_eq!(mapping.tracks[1].kind, TrackKind::Video);
    assert_eq!(
        mapping.tracks[1]
            .video
            .as_ref()
            .and_then(|metadata| metadata.coded_height),
        Some(1080)
    );
}

#[test]
fn duration_uses_track_duration_not_num_frames() {
    let mut metadata_by_track = HashMap::new();
    let num_frames_only_track = vp9_video_track_with_timing(1, None, Some(48_000));

    let mapping = map_tracks(&[num_frames_only_track], &mut metadata_by_track);

    assert_eq!(mapping.tracks[0].duration, None);
    assert_eq!(mapping.duration, None);

    let zero_duration_track = vp9_video_track_with_timing(1, Some(SymphoniaDuration::new(0)), None);
    let mut zero_duration_metadata_by_track = HashMap::new();
    let zero_duration_mapping =
        map_tracks(&[zero_duration_track], &mut zero_duration_metadata_by_track);

    assert_eq!(
        zero_duration_mapping.tracks[0].duration,
        Some(std::time::Duration::ZERO)
    );
    assert_eq!(
        zero_duration_mapping.duration,
        Some(std::time::Duration::ZERO)
    );

    let timed_track = vp9_video_track_with_timing(1, Some(SymphoniaDuration::new(2_500)), Some(99));
    let mut empty_metadata_by_track = HashMap::new();
    let mapping = map_tracks(&[timed_track], &mut empty_metadata_by_track);

    assert_eq!(
        mapping.tracks[0].duration,
        Some(std::time::Duration::from_millis(2_500))
    );
    assert_eq!(
        mapping.duration,
        Some(std::time::Duration::from_millis(2_500))
    );
}

#[test]
fn unsupported_matroska_video_codec_stays_visible_to_capability_layer() {
    let track = null_video_track(1);
    let matroska_video_track = MatroskaVideoTrack {
        codec_id: Some("V_AV1".to_string()),
        metadata: None,
    };

    let entry = build_track_entry(&track, Some(&matroska_video_track));

    assert_eq!(entry.supported_kind(), Some(TrackKind::Video));
    assert_eq!(entry.codec_id, "V_AV1");
}
