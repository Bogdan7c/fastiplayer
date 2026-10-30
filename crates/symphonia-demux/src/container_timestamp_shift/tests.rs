use std::time::Duration;

use symphonia::core::codecs::CodecParameters;
use symphonia::core::codecs::audio::AudioCodecParameters;
use symphonia::core::codecs::audio::well_known::{CODEC_ID_OPUS, CODEC_ID_VORBIS};
use symphonia::core::formats::Track;
use symphonia::core::formats::well_known::{FORMAT_ID_MKV, FORMAT_ID_OGG};
use symphonia::core::units::{TimeBase, Timestamp};

use super::ContainerTimestampShift;

/// OpusHead с `pre_skip = 312` (как пишет ffmpeg/libopus), mono, family 0.
fn opus_head(pre_skip: u16) -> Vec<u8> {
    let mut header = b"OpusHead".to_vec();
    header.push(1); // version
    header.push(1); // channel count
    header.extend_from_slice(&pre_skip.to_le_bytes());
    header.extend_from_slice(&48_000_u32.to_le_bytes()); // input sample rate
    header.extend_from_slice(&0_u16.to_le_bytes()); // output gain
    header.push(0); // channel mapping family
    header
}

fn audio_track(
    codec: symphonia::core::codecs::audio::AudioCodecId,
    extra_data: Option<Vec<u8>>,
    time_base: TimeBase,
) -> Track {
    let mut codec_params = AudioCodecParameters::new();
    codec_params.for_codec(codec).with_sample_rate(48_000);
    if let Some(extra_data) = extra_data {
        codec_params.with_extra_data(extra_data.into_boxed_slice());
    }
    let mut track = Track::new(7);
    track
        .with_codec_params(CodecParameters::Audio(codec_params))
        .with_time_base(time_base);
    track
}

fn opus_time_base() -> TimeBase {
    TimeBase::try_new(1, 48_000).expect("valid time base")
}

#[test]
fn ogg_opus_track_shifts_by_opus_head_pre_skip() {
    let track = audio_track(CODEC_ID_OPUS, Some(opus_head(312)), opus_time_base());

    let shift = ContainerTimestampShift::for_container_track(FORMAT_ID_OGG, &track);

    // Гранула первого пакета даёт 0 → медиа-время −312 отсчётов; пакет на 960 → 648.
    assert_eq!(
        shift.container_to_media(Timestamp::new(0)),
        Timestamp::new(-312)
    );
    assert_eq!(
        shift.container_to_media(Timestamp::new(960)),
        Timestamp::new(648)
    );
    // Seek-цель 1 с медиа-времени ищется на 1 с + 6.5 мс по гранулам.
    assert_eq!(
        shift.media_seek_time_to_container(Duration::from_secs(1), Some(opus_time_base())),
        Duration::from_micros(1_006_500)
    );
}

/// Matroska/WebM Opus несёт тот же OpusHead, но патч уже вычел CodecDelay из pts:
/// повторный сдвиг дал бы двойную коррекцию.
#[test]
fn matroska_opus_track_is_not_shifted_again() {
    let track = audio_track(
        CODEC_ID_OPUS,
        Some(opus_head(312)),
        TimeBase::try_new(1, 1_000).expect("valid"),
    );

    let shift = ContainerTimestampShift::for_container_track(FORMAT_ID_MKV, &track);

    assert_eq!(shift, ContainerTimestampShift::NONE);
    assert_eq!(
        shift.container_to_media(Timestamp::new(-6)),
        Timestamp::new(-6)
    );
}

#[test]
fn ogg_tracks_without_opus_or_readable_pre_skip_are_not_shifted() {
    let vorbis = audio_track(CODEC_ID_VORBIS, Some(opus_head(312)), opus_time_base());
    let missing_head = audio_track(CODEC_ID_OPUS, None, opus_time_base());
    let truncated_head = audio_track(
        CODEC_ID_OPUS,
        Some(b"OpusHead\x01\x01\x38".to_vec()),
        opus_time_base(),
    );
    let foreign_time_base = audio_track(
        CODEC_ID_OPUS,
        Some(opus_head(312)),
        TimeBase::try_new(1, 1_000).expect("valid"),
    );

    for track in [vorbis, missing_head, truncated_head, foreign_time_base] {
        assert_eq!(
            ContainerTimestampShift::for_container_track(FORMAT_ID_OGG, &track),
            ContainerTimestampShift::NONE
        );
    }
}

#[test]
fn unshifted_track_keeps_timestamps_and_seek_targets() {
    let shift = ContainerTimestampShift::NONE;

    assert_eq!(
        shift.container_to_media(Timestamp::new(-25)),
        Timestamp::new(-25)
    );
    assert_eq!(
        shift.media_seek_time_to_container(Duration::from_millis(420), Some(opus_time_base())),
        Duration::from_millis(420)
    );
}

#[test]
fn extreme_container_timestamp_saturates_instead_of_panicking() {
    let track = audio_track(CODEC_ID_OPUS, Some(opus_head(312)), opus_time_base());
    let shift = ContainerTimestampShift::for_container_track(FORMAT_ID_OGG, &track);

    assert_eq!(
        shift.container_to_media(Timestamp::new(i64::MIN)),
        Timestamp::new(i64::MIN)
    );
}
