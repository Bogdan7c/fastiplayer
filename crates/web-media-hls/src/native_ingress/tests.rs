//! Тесты admission native HLS: выбор master-варианта, форма дорожек (A13), presentation.

use super::*;
use source_core::HttpRequestTarget;
use web_media_core::{PreferredVideoHeight, RawCodecIdentity};

fn target() -> HttpRequestTarget {
    HttpRequestTarget::parse_exact("https://media.example.test/master.m3u8").expect("valid target")
}

fn policy(height: Option<u32>) -> NativeHlsSelectionPolicy {
    let height = height.map_or(PreferredHeightPolicy::NoPreference, |height| {
        PreferredHeightPolicy::Prefer(
            PreferredVideoHeight::new(height).expect("valid preferred height"),
        )
    });
    NativeHlsSelectionPolicy::new(
        height,
        vec![CodecFamily::H264, CodecFamily::Vp9, CodecFamily::Av1],
    )
    .expect("valid policy")
}

fn admit(
    manifest: &str,
    policy: &NativeHlsSelectionPolicy,
    expected: Option<&NativeHlsSemanticSelection>,
) -> Result<NativeHlsSemanticSelection, NativeHlsAdmissionError> {
    admit_native_hls(
        manifest.as_bytes(),
        &target(),
        HlsParserLimits::default(),
        policy,
        expected,
    )
}

#[test]
fn x36_like_master_uses_codec_then_preferred_height_without_sibling_probe() {
    let manifest = "#EXTM3U\n\
#EXT-X-STREAM-INF:BANDWIDTH=2500000,RESOLUTION=1280x720,CODECS=\"avc1.64001f,mp4a.40.2\"\n\
720.m3u8\n\
#EXT-X-STREAM-INF:BANDWIDTH=5000000,RESOLUTION=1920x1080,CODECS=\"avc1.640028,mp4a.40.2\"\n\
1080.m3u8\n";
    let selected = admit(manifest, &policy(Some(720)), None).expect("master admitted");
    assert_eq!(
        selected.runtime_intent().resolution,
        Some((
            NonZeroU32::new(1280).unwrap(),
            NonZeroU32::new(720).unwrap()
        ))
    );
}

#[test]
fn exact_reopen_fails_closed_when_semantic_row_disappears() {
    let initial = "#EXTM3U\n\
#EXT-X-STREAM-INF:BANDWIDTH=2500000,RESOLUTION=1280x720,CODECS=\"avc1.64001f,mp4a.40.2\"\n\
720.m3u8\n";
    let changed = "#EXTM3U\n\
#EXT-X-STREAM-INF:BANDWIDTH=5000000,RESOLUTION=1920x1080,CODECS=\"avc1.640028,mp4a.40.2\"\n\
1080.m3u8\n";
    let selected = admit(initial, &policy(None), None).expect("initial admitted");
    assert!(matches!(
        admit(changed, &policy(None), Some(&selected)),
        Err(NativeHlsAdmissionError::ExtractorMaterialRequired)
    ));
}

#[test]
fn catalog_admission_keeps_valid_ambiguous_master_native_with_fresh_exact_index() {
    let manifest = "#EXTM3U\n\
#EXT-X-STREAM-INF:BANDWIDTH=100000,RESOLUTION=16x16,CODECS=\"avc1.42c00a,mp4a.40.2\"\n\
ts.m3u8\n\
#EXT-X-STREAM-INF:BANDWIDTH=200000,RESOLUTION=16x16,CODECS=\"avc1.42c00a,mp4a.40.2\"\n\
fmp4.m3u8\n";
    assert!(matches!(
        admit(manifest, &policy(None), None),
        Err(NativeHlsAdmissionError::ExtractorMaterialRequired)
    ));

    let admitted = admit_native_hls_catalog(
        manifest.as_bytes(),
        &target(),
        HlsParserLimits::default(),
        &policy(None),
    )
    .expect("The full catalog path does not need to delegate a valid master extractor.");

    assert_eq!(admitted.current_master_variant_index(), Some(1));
    assert_eq!(
        admitted.runtime_intent().main_track_layout,
        HlsMainTrackLayoutIntent::MuxedAv
    );
}

#[test]
fn alternate_audio_group_uses_unique_provider_default_without_hiding_siblings() {
    let manifest = "#EXTM3U\n\
#EXT-X-MEDIA:TYPE=AUDIO,GROUP-ID=\"audio\",NAME=\"English\",LANGUAGE=\"en\",DEFAULT=YES,URI=\"en.m3u8\"\n\
#EXT-X-MEDIA:TYPE=AUDIO,GROUP-ID=\"audio\",NAME=\"Deutsch\",LANGUAGE=\"de\",URI=\"de.m3u8\"\n\
#EXT-X-STREAM-INF:BANDWIDTH=200000,RESOLUTION=16x16,CODECS=\"avc1.42c00a,mp4a.40.2\",AUDIO=\"audio\"\n\
video.m3u8\n";
    let admitted = admit_native_hls_catalog(
        manifest.as_bytes(),
        &target(),
        HlsParserLimits::default(),
        &policy(None),
    )
    .expect("unique DEFAULT rendition should open alternate-audio catalog");

    let HlsAudioLayoutIntent::NativeGroupResolved { group_id, evidence } =
        &admitted.runtime_intent().audio
    else {
        panic!("alternate audio must be preserved group/evidence shape");
    };
    assert_eq!(group_id.as_ref(), "audio");
    assert_eq!(evidence.name.as_deref(), Some("English"));
    assert_eq!(evidence.language.as_deref(), Some("en"));
}

#[test]
fn catalog_admission_keeps_media_presentation_typed() {
    let vod = "#EXTM3U\n#EXT-X-TARGETDURATION:1\n#EXTINF:1,\nsegment.ts\n#EXT-X-ENDLIST\n";
    let admitted_vod = admit_native_hls_catalog(
        vod.as_bytes(),
        &target(),
        HlsParserLimits::default(),
        &policy(None),
    )
    .expect("media VOD must remain native without fake master ordinal-?");
    assert_eq!(admitted_vod.current_master_variant_index(), None);
    assert_eq!(
        admitted_vod.presentation_evidence(),
        NativeHlsPresentationEvidence::TopMedia(HlsCatalogPresentation::Vod)
    );
    assert_eq!(
        admitted_vod.runtime_intent().main_track_layout,
        HlsMainTrackLayoutIntent::VideoWithOptionalAudioOrAudioOnly
    );

    // Завершённый EVENT (A13-1) - это VOD: презентацию определяет ENDLIST, а не тип плейлиста.
    let finished_event = "#EXTM3U\n#EXT-X-PLAYLIST-TYPE:EVENT\n#EXT-X-TARGETDURATION:1\n\
                          #EXTINF:1,\nsegment.ts\n#EXT-X-ENDLIST\n";
    let admitted_event = admit_native_hls_catalog(
        finished_event.as_bytes(),
        &target(),
        HlsParserLimits::default(),
        &policy(None),
    )
    .expect("Completed EVENT must remain native");
    assert_eq!(
        admitted_event.presentation_evidence(),
        NativeHlsPresentationEvidence::TopMedia(HlsCatalogPresentation::Vod)
    );

    for live_manifest in [
        "#EXTM3U\n#EXT-X-TARGETDURATION:1\n#EXTINF:1,\nsegment.ts\n",
        "#EXTM3U\n#EXT-X-PLAYLIST-TYPE:EVENT\n#EXT-X-TARGETDURATION:1\n#EXTINF:1,\nsegment.ts\n",
    ] {
        let admitted_live = admit_native_hls_catalog(
            live_manifest.as_bytes(),
            &target(),
            HlsParserLimits::default(),
            &policy(None),
        )
        .expect("supported sliding/event media must remain native");
        assert_eq!(
            admitted_live.presentation_evidence(),
            NativeHlsPresentationEvidence::TopMedia(HlsCatalogPresentation::Live)
        );
    }
}

#[test]
fn separate_audio_identity_includes_exact_group_and_rendition() {
    let manifest = "#EXTM3U\n\
#EXT-X-MEDIA:TYPE=AUDIO,GROUP-ID=\"stereo\",NAME=\"English\",LANGUAGE=\"en\",DEFAULT=YES,AUTOSELECT=YES,URI=\"audio.m3u8\"\n\
#EXT-X-STREAM-INF:BANDWIDTH=2500000,RESOLUTION=1280x720,CODECS=\"avc1.64001f,mp4a.40.2\",AUDIO=\"stereo\"\n\
video.m3u8\n";
    let selected = admit(manifest, &policy(None), None).expect("master admitted");
    assert!(matches!(
        selected.runtime_intent().audio,
        HlsAudioLayoutIntent::NativeGroupResolved { ref group_id, ref evidence }
            if group_id.as_ref() == "stereo"
                && evidence.name.as_deref() == Some("English")
                && evidence.language.as_deref() == Some("en")
    ));
    assert_eq!(
        selected.runtime_intent().main_track_layout,
        HlsMainTrackLayoutIntent::VideoOnly
    );
}

#[test]
fn missing_master_evidence_falls_back_but_live_media_stays_native() {
    let missing = "#EXTM3U\n#EXT-X-STREAM-INF:BANDWIDTH=1\nchild.m3u8\n";
    assert!(matches!(
        admit(missing, &policy(None), None),
        Err(NativeHlsAdmissionError::ExtractorMaterialRequired)
    ));

    let live = "#EXTM3U\n#EXT-X-TARGETDURATION:4\n#EXTINF:4,\nseg.ts\n";
    assert!(admit(live, &policy(None), None).is_ok());
}

/// A13-2: variant без audio-кодека и без AUDIO group - видео без звука, остаётся native;
/// форму потом доказывает probe. Variant с AUDIO group без audio-кодека по-прежнему
/// требует extractor (внешний звук нельзя угадать).
#[test]
fn master_variant_without_audio_codec_is_native_video_with_optional_audio() {
    let video_only = "#EXTM3U\n\
#EXT-X-STREAM-INF:BANDWIDTH=2500000,RESOLUTION=1280x720,CODECS=\"avc1.64001f\"\n\
720.m3u8\n";
    let admitted = admit(video_only, &policy(None), None).expect("video-only master native");
    assert_eq!(
        admitted.runtime_intent().main_track_layout,
        HlsMainTrackLayoutIntent::VideoWithOptionalAudio
    );
    assert_eq!(admitted.runtime_intent().audio, HlsAudioLayoutIntent::Muxed);

    let group_without_audio_codec = "#EXTM3U\n\
#EXT-X-MEDIA:TYPE=AUDIO,GROUP-ID=\"a\",NAME=\"en\",DEFAULT=YES,URI=\"a.m3u8\"\n\
#EXT-X-STREAM-INF:BANDWIDTH=2500000,RESOLUTION=1280x720,CODECS=\"avc1.64001f\",AUDIO=\"a\"\n\
720.m3u8\n";
    assert!(matches!(
        admit(group_without_audio_codec, &policy(None), None),
        Err(NativeHlsAdmissionError::ExtractorMaterialRequired)
    ));
}

/// A13-3: master из variant-ов только с audio-кодеком (радио) - native AudioOnly, лучший
/// bandwidth; в смешанном master видео по-прежнему выигрывает; неразрешимые формы
/// (AUDIO group, неизвестный токен CODECS, два кодека) остаются за extractor-ом.
#[test]
fn master_of_audio_only_variants_is_native_radio_but_video_still_wins() {
    let radio = "#EXTM3U\n\
#EXT-X-STREAM-INF:BANDWIDTH=64000,CODECS=\"mp4a.40.2\"\nlow.m3u8\n\
#EXT-X-STREAM-INF:BANDWIDTH=128000,CODECS=\"mp4a.40.2\"\nhigh.m3u8\n";
    let admitted = admit_native_hls_catalog(
        radio.as_bytes(),
        &target(),
        HlsParserLimits::default(),
        &policy(None),
    )
    .expect("radio master native");
    assert_eq!(admitted.current_master_variant_index(), Some(1));
    let intent = admitted.runtime_intent();
    assert_eq!(
        intent.main_track_layout,
        HlsMainTrackLayoutIntent::AudioOnly
    );
    assert_eq!(intent.resolution, None);
    assert_eq!(intent.codecs.as_deref(), Some("mp4a.40.2"));

    let mixed = "#EXTM3U\n\
#EXT-X-STREAM-INF:BANDWIDTH=900000,CODECS=\"mp4a.40.2\"\naudio.m3u8\n\
#EXT-X-STREAM-INF:BANDWIDTH=2500000,RESOLUTION=1280x720,CODECS=\"avc1.64001f,mp4a.40.2\"\n720.m3u8\n";
    let admitted = admit_native_hls_catalog(
        mixed.as_bytes(),
        &target(),
        HlsParserLimits::default(),
        &policy(None),
    )
    .expect("mixed master native");
    assert_eq!(admitted.current_master_variant_index(), Some(1));
    assert_eq!(
        admitted.runtime_intent().main_track_layout,
        HlsMainTrackLayoutIntent::MuxedAv
    );

    for unresolved in [
        "#EXT-X-STREAM-INF:BANDWIDTH=64000,CODECS=\"mp4a.40.2\",AUDIO=\"a\"\nx.m3u8\n",
        "#EXT-X-STREAM-INF:BANDWIDTH=64000,CODECS=\"mp4a.40.2,mystery\"\nx.m3u8\n",
        "#EXT-X-STREAM-INF:BANDWIDTH=64000\nx.m3u8\n",
    ] {
        let manifest = format!(
            "#EXTM3U\n#EXT-X-MEDIA:TYPE=AUDIO,GROUP-ID=\"a\",NAME=\"en\",URI=\"a.m3u8\"\n{unresolved}"
        );
        assert!(
            matches!(
                admit(&manifest, &policy(None), None),
                Err(NativeHlsAdmissionError::ExtractorMaterialRequired)
            ),
            "{unresolved}"
        );
    }
}

#[test]
fn non_hls_and_malformed_hls_remain_distinct() {
    assert!(matches!(
        admit("<html>login</html>", &policy(None), None),
        Err(NativeHlsAdmissionError::StrictlyNotHls)
    ));
    assert!(matches!(
        admit("#EXTM3U\n#EXT-X-TARGETDURATION:nope\n", &policy(None), None),
        Err(NativeHlsAdmissionError::Parse(_))
    ));
}

#[test]
fn test_codec_parser_fixture_stays_known_h264() {
    let codec =
        NormalizedCodec::parse(RawCodecIdentity::new("avc1.640028").expect("valid codec identity"));
    assert_eq!(codec.kind(), CodecKind::Known(CodecFamily::H264));
}

#[test]
fn hdr_policy_never_silently_changes_declared_dynamic_range() {
    let manifest = "#EXTM3U\n\
#EXT-X-STREAM-INF:BANDWIDTH=2500000,RESOLUTION=1920x1080,CODECS=\"avc1.640028,mp4a.40.2\",VIDEO-RANGE=SDR\n\
sdr.m3u8\n\
#EXT-X-STREAM-INF:BANDWIDTH=2500000,RESOLUTION=1920x1080,CODECS=\"avc1.640028,mp4a.40.2\",VIDEO-RANGE=PQ\n\
hdr.m3u8\n";
    assert!(matches!(
        admit(manifest, &policy(None), None),
        Err(NativeHlsAdmissionError::ExtractorMaterialRequired)
    ));

    let hdr_policy =
        policy(None).with_dynamic_range_policy(NativeHlsDynamicRangePolicy::PreferHdrWhenAvailable);
    assert!(matches!(
        admit(manifest, &hdr_policy, None),
        Err(NativeHlsAdmissionError::ExtractorMaterialRequired)
    ));
}
