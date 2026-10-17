//! Пересечение demux-дорожки AC-3/E-AC-3 с decoder capability snapshot.

use audio_core::{AudioDecodeCapabilitySnapshot, AudioDecodeCodecFamily};
use media_core::{TrackId, TrackInfo, TrackKind, TrackLabels};
use web_media_core::{CodecFamily, CodecKind};
use web_media_hls::{HlsCatalogCapabilityProofPort, HlsCatalogCapabilityRejection};

use super::AppCatalogCapabilityProbe;

/// Аудиодорожка в форме, которую отдают MPEG-TS/ISO-BMFF demuxer-ы.
fn audio_track(codec_id: &str, channels: u32) -> TrackInfo {
    TrackInfo {
        id: TrackId::new(2),
        kind: TrackKind::Audio,
        codec_id: codec_id.to_owned(),
        codec_private: None,
        time_base: None,
        duration: None,
        sample_rate: Some(48_000),
        channels: Some(channels),
        video: None,
        labels: TrackLabels::default(),
    }
}

/// Probe без video capability: эти тесты касаются только audio intersection.
fn probe(audio: AudioDecodeCapabilitySnapshot) -> AppCatalogCapabilityProbe {
    AppCatalogCapabilityProbe::new(capability_core::SystemCapabilities::empty(1), audio)
}

/// Доступная exact family даёт descriptor с той же family и числом каналов дорожки.
#[test]
fn available_dolby_family_produces_exact_descriptor() {
    let probe = probe(
        AudioDecodeCapabilitySnapshot::empty()
            .with_available_family(AudioDecodeCodecFamily::Ac3)
            .with_available_family(AudioDecodeCodecFamily::Eac3),
    );
    for (codec_id, family, channels) in [
        ("A_AC3", CodecFamily::Ac3, 2),
        ("A_EAC3", CodecFamily::Eac3, 6),
    ] {
        let descriptor = probe
            .audio_descriptor(&audio_track(codec_id, channels))
            .unwrap_or_else(|| panic!("{codec_id} must be accepted"));
        assert_eq!(descriptor.codec().kind(), CodecKind::Known(family));
        assert_eq!(
            descriptor.channels().map(web_media_core::ChannelCount::get),
            Some(u16::try_from(channels).expect("small channel count"))
        );
        assert_eq!(
            descriptor
                .sample_rate()
                .map(web_media_core::SampleRate::hertz),
            Some(48_000)
        );
    }
}

/// Snapshot без нужной family (в том числе только с соседней Dolby family) отвергает
/// дорожку typed-отказом HLS proof port-а; прежние families не затронуты.
#[test]
fn unavailable_dolby_family_is_rejected_and_neighbours_unchanged() {
    let mut ac3_only = probe(
        AudioDecodeCapabilitySnapshot::empty()
            .with_available_family(AudioDecodeCodecFamily::Ac3)
            .with_available_family(AudioDecodeCodecFamily::Aac),
    );
    assert_eq!(
        ac3_only.prove_audio(&audio_track("A_EAC3", 6)).map(|_| ()),
        Err(HlsCatalogCapabilityRejection::Unsupported)
    );
    assert!(ac3_only.prove_audio(&audio_track("A_AC3", 2)).is_ok());
    assert!(ac3_only.prove_audio(&audio_track("A_AAC", 2)).is_ok());

    let mut empty = probe(AudioDecodeCapabilitySnapshot::empty());
    assert_eq!(
        empty.prove_audio(&audio_track("A_AC3", 2)).map(|_| ()),
        Err(HlsCatalogCapabilityRejection::Unsupported)
    );
}
