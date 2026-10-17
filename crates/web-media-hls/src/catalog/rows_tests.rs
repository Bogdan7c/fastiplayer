//! Сверка CHANNELS rendition-а с каналами доказанной дорожки (обычный и object-based звук).

use hls_playlist_core::{MediaRendition, MediaRenditionType};
use web_media_core::{AudioTrackDescriptor, ChannelCount, NormalizedCodec, RawCodecIdentity};

use super::merge_audio_evidence;
use crate::HlsCatalogSiblingRejectionReason;

/// Audio rendition только с CHANNELS; остальное не влияет на сверку каналов.
fn rendition(channels: &str, channel_count: u64) -> MediaRendition {
    MediaRendition {
        rendition_type: MediaRenditionType::Audio,
        group_id: "audio".into(),
        name: "Main".into(),
        uri: None,
        language: None,
        associated_language: None,
        characteristics: None,
        channel_count: std::num::NonZeroU64::new(channel_count),
        channels: Some(channels.into()),
        is_default: true,
        autoselect: true,
        forced: false,
    }
}

/// Доказанная demux-ом E-AC-3 5.1 дорожка.
fn eac3_51_proof() -> AudioTrackDescriptor {
    AudioTrackDescriptor::new(
        NormalizedCodec::parse(RawCodecIdentity::new("A_EAC3").expect("codec")),
        None,
        Some(ChannelCount::new(6).expect("channels")),
        None,
        None,
    )
}

/// `16/JOC` не конфликтует с 5.1-основой: каналы берутся из доказанной дорожки.
#[test]
fn object_based_channels_take_layout_from_proven_track() {
    let merged = merge_audio_evidence(&rendition("16/JOC", 16), &eac3_51_proof())
        .expect("object count must not conflict with decoded bed");
    assert_eq!(merged.channels().map(ChannelCount::get), Some(6));
}

/// Обычный CHANNELS по-прежнему строго сверяется: «2» против 5.1 — конфликт, «6» — ок.
#[test]
fn speaker_channels_still_conflict_with_different_proven_layout() {
    assert_eq!(
        merge_audio_evidence(&rendition("2", 2), &eac3_51_proof()).map(|_| ()),
        Err(HlsCatalogSiblingRejectionReason::ManifestEvidenceConflict)
    );
    let merged =
        merge_audio_evidence(&rendition("6", 6), &eac3_51_proof()).expect("matching speaker count");
    assert_eq!(merged.channels().map(ChannelCount::get), Some(6));
}
