//! Есть ли в полосе видео — по init-сегменту (норма памяти полосы у
//! предохранителя, «зеркало браузера»). Настоящие фикстуры и синтетика.

use crate::container::PushedContainer;

use super::test_support::{
    SyntheticIsoTrack, ebml_element, ebml_unknown_size_element, ebml_unsigned, iso_initialization,
};
use super::{LaneContent, SegmentTimingContext};

const VIDEO_MP4: &[u8] = include_bytes!("../../../fixtures/video-h264-64-4s.mp4");
const AUDIO_MP4: &[u8] = include_bytes!("../../../fixtures/audio-aac-4s.mp4");
const VIDEO_WEBM: &[u8] = include_bytes!("../../../fixtures/video-vp9-64-4fps.webm");
const MUXED_WEBM: &[u8] =
    include_bytes!("../../../../browser-bridge-protocol/fixtures/media/muxed.webm");

/// Содержимое полосы по байтам init (весь файл годится: разбор останавливается
/// на первом медиасегменте).
fn content_of(container: PushedContainer, initialization: &[u8]) -> LaneContent {
    SegmentTimingContext::from_initialization(container, initialization)
        .expect("timing context")
        .content()
}

/// Init WebM с одной дорожкой заданного `TrackType` (без типа — `None`).
fn webm_with_track_type(track_type: Option<u64>) -> Vec<u8> {
    let mut entry = ebml_unsigned(0xD7, 1);
    if let Some(track_type) = track_type {
        entry.extend(ebml_unsigned(0x83, track_type));
    }
    let tracks = ebml_element(0xAE, &entry);
    let segment_body = ebml_element(0x1654_AE6B, &tracks);
    let mut initialization = ebml_element(0x1A45_DFA3, &[0x42, 0x82, 0x84, b'w', b'e', b'b', b'm']);
    initialization.extend(ebml_unknown_size_element(0x1853_8067, &segment_body));
    initialization
}

#[test]
fn real_fragmented_mp4_video_and_audio_are_told_apart_by_hdlr() {
    assert_eq!(
        content_of(PushedContainer::FragmentedMp4, VIDEO_MP4),
        LaneContent::CarriesVideo
    );
    assert_eq!(
        content_of(PushedContainer::FragmentedMp4, AUDIO_MP4),
        LaneContent::WithoutVideo
    );
}

#[test]
fn real_webm_with_video_alone_or_muxed_with_sound_carries_video() {
    assert_eq!(
        content_of(PushedContainer::WebM, VIDEO_WEBM),
        LaneContent::CarriesVideo
    );
    assert_eq!(
        content_of(PushedContainer::WebM, MUXED_WEBM),
        LaneContent::CarriesVideo
    );
}

#[test]
fn webm_audio_track_type_is_without_video_and_an_untyped_track_counts_as_heavy() {
    assert_eq!(
        content_of(PushedContainer::WebM, &webm_with_track_type(Some(2))),
        LaneContent::WithoutVideo
    );
    assert_eq!(
        content_of(PushedContainer::WebM, &webm_with_track_type(Some(1))),
        LaneContent::CarriesVideo
    );
    // Тип не указан — неизвестно, что внутри: полоса считается тяжёлой (без
    // резерва звука), время при этом читается как раньше.
    assert_eq!(
        content_of(PushedContainer::WebM, &webm_with_track_type(None)),
        LaneContent::CarriesVideo
    );
}

#[test]
fn fragmented_mp4_without_hdlr_counts_as_heavy_and_still_has_timing() {
    let initialization = iso_initialization(&[SyntheticIsoTrack {
        track_id: 1,
        timescale: 1000,
        extends_default_duration: None,
    }]);
    assert_eq!(
        content_of(PushedContainer::FragmentedMp4, &initialization),
        LaneContent::CarriesVideo
    );
}

#[test]
fn mpeg_ts_has_no_initialization_and_counts_as_heavy() {
    assert_eq!(
        content_of(PushedContainer::MpegTs, &[]),
        LaneContent::CarriesVideo
    );
}
