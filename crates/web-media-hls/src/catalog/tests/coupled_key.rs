//! Ключ coupled-строки muxed TS-варианта с полным набором метаданных.
//!
//! Регрессия: length-framed hex-ключ такого варианта занимал ~298 байт и превышал
//! `MAX_COMPONENT_VARIANT_KEY_UTF8_BYTES` (256), из-за чего обычный ffmpeg HLS не открывался
//! с ложной причиной `DescriptorBounds`. Теперь ключ — префикс + SHA-256 фиксированной длины.

use web_media_core::{
    Bitrate, ChannelCount, CoupledVariantSemanticIdentity, FrameRate, LanguageTag, SampleRate,
    VideoHeight, VideoWidth,
};

use super::*;

/// Master обычного ffmpeg-VOD: muxed H.264 High + AAC LC, все атрибуты заданы.
fn ffmpeg_master(bandwidth: u64, frame_rate: &str) -> String {
    format!(
        "#EXTM3U\n\
         #EXT-X-STREAM-INF:BANDWIDTH={bandwidth},AVERAGE-BANDWIDTH=1651843,RESOLUTION=1280x544,\
         FRAME-RATE={frame_rate},CODECS=\"avc1.64001f,mp4a.40.2\"\n\
         index.m3u8\n"
    )
}

/// Доказанное demux-ом видео: все опциональные поля заполнены, чтобы ключ был максимальным.
fn full_video(codec: &str) -> VideoTrackDescriptor {
    VideoTrackDescriptor::new(
        NormalizedCodec::parse(RawCodecIdentity::new(codec).expect("codec")),
        Some(VideoWidth::new(1280).expect("width")),
        Some(VideoHeight::new(544).expect("height")),
        Some(FrameRate::new(24, 1).expect("frame rate")),
        Some(Bitrate::new(1_500_000).expect("bitrate")),
        DynamicRange::Sdr,
    )
}

/// Доказанный AAC LC 48 кГц стерео с битрейтом и (опционально) языком.
fn full_audio(language: Option<&str>) -> AudioTrackDescriptor {
    AudioTrackDescriptor::new(
        NormalizedCodec::parse(RawCodecIdentity::new("mp4a.40.2").expect("codec")),
        Some(SampleRate::new(48_000).expect("sample rate")),
        Some(ChannelCount::new(2).expect("channels")),
        Some(Bitrate::new(130_000).expect("bitrate")),
        language.map(|tag| LanguageTag::new(tag).expect("language")),
    )
}

/// Строит каталог из одного muxed TS-варианта и возвращает semantic identity его coupled-строки.
fn coupled_identity(
    master_text: &str,
    video: VideoTrackDescriptor,
    audio: AudioTrackDescriptor,
) -> CoupledVariantSemanticIdentity {
    let master = master(master_text);
    let intent = HlsVariantSelectionIntent {
        resolution: Some((
            NonZeroU32::new(1280).expect("width"),
            NonZeroU32::new(544).expect("height"),
        )),
        codecs: None,
        audio: HlsAudioLayoutIntent::Muxed,
        main_track_layout: HlsMainTrackLayoutIntent::MuxedAv,
    };
    let mut proofs = ProofQueue::new([HlsCatalogChildProof {
        container: HlsRequiredContainer::TransportStream,
        tracks: HlsCatalogTrackProof::Muxed { video, audio },
        alignment: timeline_proof(REFERENCE_TOTAL_MS),
    }]);
    let snapshot = build_hls_catalog(
        HlsCatalogBuildRequest {
            master: &master,
            catalog_identity: catalog_identity(1, 1),
            provider_default: &intent,
            provider_default_variant_index: None,
            policy: policy(),
        },
        &mut proofs,
    )
    .expect("muxed TS variant with full metadata must build a catalog");
    let presentations = snapshot.catalog().coupled_presentations();
    assert_eq!(
        presentations.len(),
        1,
        "variant must stay a playable coupled row"
    );
    presentations[0].semantic_identity().clone()
}

/// Базовая идентичность для сравнений в тестах ниже.
fn baseline() -> CoupledVariantSemanticIdentity {
    coupled_identity(
        &ffmpeg_master(3_849_487, "24.000"),
        full_video("avc1.64001f"),
        full_audio(Some("eng")),
    )
}

#[test]
fn full_metadata_muxed_ts_variant_builds_playable_coupled_row() {
    // Само построение (раньше падало ProviderDefaultRejected/DescriptorBounds) и есть проверка:
    // каталог содержит ровно одну coupled-строку с доказанными дорожками.
    let identity = baseline();
    assert_eq!(identity.source(), SourceIdentity::new(77));
}

#[test]
fn same_inputs_produce_same_key() {
    assert_eq!(baseline(), baseline());
}

#[test]
fn bandwidth_change_changes_key() {
    let changed = coupled_identity(
        &ffmpeg_master(3_849_488, "24.000"),
        full_video("avc1.64001f"),
        full_audio(Some("eng")),
    );
    assert_ne!(baseline(), changed);
}

#[test]
fn frame_rate_change_changes_key() {
    let changed = coupled_identity(
        &ffmpeg_master(3_849_487, "25.000"),
        {
            // Proof совпадает с декларацией манифеста, иначе это ManifestEvidenceConflict.
            VideoTrackDescriptor::new(
                NormalizedCodec::parse(RawCodecIdentity::new("avc1.64001f").expect("codec")),
                Some(VideoWidth::new(1280).expect("width")),
                Some(VideoHeight::new(544).expect("height")),
                Some(FrameRate::new(25, 1).expect("frame rate")),
                Some(Bitrate::new(1_500_000).expect("bitrate")),
                DynamicRange::Sdr,
            )
        },
        full_audio(Some("eng")),
    );
    assert_ne!(baseline(), changed);
}

#[test]
fn audio_language_change_changes_key() {
    let changed = coupled_identity(
        &ffmpeg_master(3_849_487, "24.000"),
        full_video("avc1.64001f"),
        full_audio(Some("rus")),
    );
    assert_ne!(baseline(), changed);
    let absent = coupled_identity(
        &ffmpeg_master(3_849_487, "24.000"),
        full_video("avc1.64001f"),
        full_audio(None),
    );
    assert_ne!(baseline(), absent);
    assert_ne!(changed, absent);
}

#[test]
fn video_codec_change_changes_key() {
    let changed = coupled_identity(
        &ffmpeg_master(3_849_487, "24.000"),
        full_video("avc1.640028"),
        full_audio(Some("eng")),
    );
    assert_ne!(baseline(), changed);
}

/// Языки audio-строк каталога в порядке публикации.
fn rendition_languages(first: &str, second: &str) -> Vec<String> {
    let master = master(&format!(
        "#EXTM3U\n\
         #EXT-X-MEDIA:TYPE=AUDIO,GROUP-ID=\"a\",NAME=\"First\",LANGUAGE=\"{first}\",CHANNELS=\"2\",URI=\"first.m3u8\",DEFAULT=YES\n\
         #EXT-X-MEDIA:TYPE=AUDIO,GROUP-ID=\"a\",NAME=\"Second\",LANGUAGE=\"{second}\",CHANNELS=\"2\",URI=\"second.m3u8\"\n\
         #EXT-X-STREAM-INF:BANDWIDTH=1,CODECS=\"avc1.640028,mp4a.40.2\",RESOLUTION=1280x720,AUDIO=\"a\"\n\
         video.m3u8\n"
    ));
    let mut intent = separate_intent(1280, 720, "First");
    if let HlsAudioLayoutIntent::Separate(evidence) = &mut intent.audio {
        evidence.language = Some(first.into());
    }
    let mut proofs = ProofQueue::new([video_proof(), audio_proof(), audio_proof()]);
    let snapshot = build_hls_catalog(
        HlsCatalogBuildRequest {
            master: &master,
            catalog_identity: catalog_identity(1, 1),
            provider_default: &intent,
            provider_default_variant_index: None,
            policy: policy(),
        },
        &mut proofs,
    )
    .expect("two renditions");
    snapshot
        .catalog()
        .required_audio_variants()
        .expect("audio rows")
        .iter()
        .map(|row| {
            row.track()
                .language()
                .expect("language")
                .as_str()
                .to_owned()
        })
        .collect()
}

#[test]
fn audio_renditions_keep_master_declaration_order_not_hash_order() {
    // Оба порядка объявления: хотя бы один не совпал бы с порядком хэша ключей.
    assert_eq!(rendition_languages("en", "de"), ["en", "de"]);
    assert_eq!(rendition_languages("de", "en"), ["de", "en"]);
    assert_eq!(rendition_languages("fr", "ja"), ["fr", "ja"]);
    assert_eq!(rendition_languages("ja", "fr"), ["ja", "fr"]);
}
