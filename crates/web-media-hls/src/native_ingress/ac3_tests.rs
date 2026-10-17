//! Native HLS admission с AC-3/E-AC-3: CODECS `ac-3`/`ec-3`, EXT-X-MEDIA renditions, JOC.

use super::*;
use source_core::HttpRequestTarget;

fn target() -> HttpRequestTarget {
    HttpRequestTarget::parse_exact("https://media.example.test/master.m3u8").expect("valid target")
}

fn policy() -> NativeHlsSelectionPolicy {
    NativeHlsSelectionPolicy::new(
        PreferredHeightPolicy::NoPreference,
        vec![CodecFamily::H264, CodecFamily::Vp9, CodecFamily::Av1],
    )
    .expect("valid policy")
}

fn admit_catalog(manifest: &str) -> NativeHlsCatalogAdmission {
    admit_native_hls_catalog(
        manifest.as_bytes(),
        &target(),
        HlsParserLimits::default(),
        &policy(),
    )
    .expect("master with AC-3 family audio must be admitted natively")
}

/// Группа и evidence выбранного alternate audio.
fn resolved_group(admission: &NativeHlsCatalogAdmission) -> (String, HlsAudioRenditionEvidence) {
    let HlsAudioLayoutIntent::NativeGroupResolved { group_id, evidence } =
        admission.runtime_intent().audio
    else {
        panic!("alternate audio group must be resolved natively");
    };
    (group_id.to_string(), evidence)
}

/// Master только с E-AC-3 5.1 rendition больше не требует extractor-а; CHANNELS=6
/// становится evidence раскладки, CODECS сохраняются для runtime intent-а.
#[test]
fn eac3_rendition_master_is_admitted_with_channel_evidence() {
    let manifest = "#EXTM3U\n\
#EXT-X-MEDIA:TYPE=AUDIO,GROUP-ID=\"surround\",NAME=\"Main\",LANGUAGE=\"en\",DEFAULT=YES,CHANNELS=\"6\",URI=\"eac3.m3u8\"\n\
#EXT-X-STREAM-INF:BANDWIDTH=5800000,RESOLUTION=1920x1080,CODECS=\"avc1.640028,ec-3\",AUDIO=\"surround\"\n\
video.m3u8\n";
    let admission = admit_catalog(manifest);
    let (group, evidence) = resolved_group(&admission);
    assert_eq!(group, "surround");
    assert_eq!(evidence.channel_count.map(NonZeroU16::get), Some(6));
    assert_eq!(
        admission.runtime_intent().codecs.as_deref(),
        Some("avc1.640028,ec-3")
    );
    assert_eq!(
        admission.runtime_intent().main_track_layout,
        HlsMainTrackLayoutIntent::VideoOnly
    );
}

/// Atmos `CHANNELS="16/JOC"`: raw число остаётся identity-evidence rendition-а
/// (им же пользуется extractor), но не evidence раскладки (`speaker_channel_count`).
#[test]
fn joc_rendition_keeps_identity_count_but_not_speaker_evidence() {
    let manifest = "#EXTM3U\n\
#EXT-X-MEDIA:TYPE=AUDIO,GROUP-ID=\"atmos\",NAME=\"Main\",DEFAULT=YES,CHANNELS=\"16/JOC\",URI=\"atmos.m3u8\"\n\
#EXT-X-STREAM-INF:BANDWIDTH=5800000,RESOLUTION=1920x1080,CODECS=\"avc1.640028,ec-3\",AUDIO=\"atmos\"\n\
video.m3u8\n";
    let admission = admit_catalog(manifest);
    let (_, evidence) = resolved_group(&admission);
    assert_eq!(evidence.channel_count.map(NonZeroU16::get), Some(16));
}

/// Apple-style master: тот же 1080p H.264 с AAC stereo и с E-AC-3 Atmos (больший
/// BANDWIDTH). Прежний выбор (AAC) сохраняется: Dolby variant — только fallback.
#[test]
fn aac_variant_keeps_default_over_higher_bandwidth_eac3_variant() {
    let manifest = "#EXTM3U\n\
#EXT-X-MEDIA:TYPE=AUDIO,GROUP-ID=\"aac\",NAME=\"Main\",DEFAULT=YES,CHANNELS=\"2\",URI=\"aac.m3u8\"\n\
#EXT-X-MEDIA:TYPE=AUDIO,GROUP-ID=\"atmos\",NAME=\"Main\",DEFAULT=YES,CHANNELS=\"16/JOC\",URI=\"atmos.m3u8\"\n\
#EXT-X-STREAM-INF:BANDWIDTH=5800000,RESOLUTION=1920x1080,CODECS=\"avc1.640028,ec-3\",AUDIO=\"atmos\"\n\
video-atmos.m3u8\n\
#EXT-X-STREAM-INF:BANDWIDTH=5200000,RESOLUTION=1920x1080,CODECS=\"avc1.640028,mp4a.40.2\",AUDIO=\"aac\"\n\
video-aac.m3u8\n";
    let admission = admit_catalog(manifest);
    let (group, _) = resolved_group(&admission);
    assert_eq!(group, "aac");
    assert_eq!(admission.current_master_variant_index(), Some(1));

    // Low-load путь даёт тот же выбор и не считает пару равной (иначе extractor).
    let low_load = admit_native_hls(
        manifest.as_bytes(),
        &target(),
        HlsParserLimits::default(),
        &policy(),
        None,
    )
    .expect("tier must break the tie without extractor");
    assert_eq!(
        low_load.runtime_intent().codecs.as_deref(),
        Some("avc1.640028,mp4a.40.2")
    );
}

/// Audio-only variant `CODECS="ac-3"` (радио) становится native audio-only кандидатом.
#[test]
fn ac3_audio_only_variant_is_native_radio() {
    let manifest = "#EXTM3U\n\
#EXT-X-STREAM-INF:BANDWIDTH=192000,CODECS=\"ac-3\"\n\
radio.m3u8\n";
    let admission = admit_catalog(manifest);
    assert_eq!(
        admission.runtime_intent().main_track_layout,
        HlsMainTrackLayoutIntent::AudioOnly
    );
    assert_eq!(admission.runtime_intent().codecs.as_deref(), Some("ac-3"));
}

/// Неизвестный соседний Dolby codec (`ac-4`) по-прежнему не admission-ится natively.
#[test]
fn ac4_variant_still_requires_extractor_material() {
    let manifest = "#EXTM3U\n\
#EXT-X-STREAM-INF:BANDWIDTH=192000,CODECS=\"ac-4.02.01.01\"\n\
radio.m3u8\n";
    assert!(matches!(
        admit_native_hls_catalog(
            manifest.as_bytes(),
            &target(),
            HlsParserLimits::default(),
            &policy(),
        ),
        Err(NativeHlsAdmissionError::ExtractorMaterialRequired)
    ));
}
