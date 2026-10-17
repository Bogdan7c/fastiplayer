//! Сквозной путь «выбор другой версии звука» для HLS master-а с AAC 2.0 и E-AC-3 5.1
//! renditions (как у Dolby-раздач): loopback HTTP → production native HLS admission →
//! component catalog (две audio-строки, AAC активна по default-политике пакета E) →
//! component action на E-AC-3 → semantic reopen с позицией → MPEG-TS demux → `A_EAC3` 5.1
//! → production audio decoder → PCM. Это та же цепочка, что запускает меню «♪» в app-egui
//! (`state/audio_tracks/web_version_switch.rs` → same-item component switch).
//!
//! Фикстуры: `tests/fixtures/hls-audio-versions/` (README и `generate.sh` там же).

use std::collections::HashMap;
use std::time::Duration;

use audio::{AudioDecodeCapabilityProvider, ProductionAudioDecoderFactory};
use fastiplayer_config::AppConfig;
use media_core::{MediaTime, TrackInfo, TrackKind};
use player_core::PreparedInitialPosition;
use source_core::CancellationToken;
use web_media_core::{CodecFamily, WebMediaSemanticSelectionRequest};
use web_media_hls::HlsVodStartIntent;

use super::ac3_vertical_tests::{
    LoopbackOrigin, MINIMUM_TONE_RMS, audio_packets, decode_to_pcm, h264_fixture_capabilities,
    media_playlist,
};
use super::{
    NativeHlsAdmissionPort, NativeHlsAttempt, NativeHlsPreparationRequest,
    PreparedNativeHlsLifecycle, PreparedNativeHlsMedia, ProductionNativeHlsAdmissionPort,
};
use crate::native_web_source::hls::{NativeHlsOpenIntent, NativeHlsUrl};
use crate::safe_media_label::SafeMediaLabel;
use crate::web_media_stream_model::component_variants::{
    ComponentVariantActionError, ComponentVariantActionResolution, ComponentVariantSelectionAction,
    WebMediaAudioComponentVariantAxis, WebMediaComponentVariantAxisKind,
    WebMediaComponentVariantProjection, WebMediaInstalledComponentVariantPresentation,
};

/// H.264 64×64 без звука: main variant c отдельными audio renditions.
pub(super) const VIDEO_ONLY_SEGMENT: &[u8] =
    include_bytes!("../../../tests/fixtures/hls-audio-versions/video-only-h264.ts");
/// Версия 1: AAC-LC 2.0 48 кГц.
pub(super) const AAC_STEREO_SEGMENT: &[u8] =
    include_bytes!("../../../tests/fixtures/hls-audio-versions/audio-aac-2ch.ts");
/// Версия 2: E-AC-3 5.1 48 кГц.
pub(super) const EAC3_SURROUND_SEGMENT: &[u8] =
    include_bytes!("../../../tests/fixtures/hls-audio-versions/audio-eac3-5p1.ts");

/// Позиция, которую switch обязан сохранить (внутри единственного 0.5 с сегмента).
const RESTORED_POSITION: Duration = Duration::from_millis(200);

/// Master по Apple authoring spec: отдельная audio group на кодек и variant на каждую group.
/// Группы одного типа обязаны содержать одинаковые NAME (RFC 8216 4.3.4.2.1).
pub(super) fn dolby_and_aac_master() -> Vec<u8> {
    b"#EXTM3U\n\
#EXT-X-MEDIA:TYPE=AUDIO,GROUP-ID=\"aac\",NAME=\"English\",LANGUAGE=\"en\",DEFAULT=YES,AUTOSELECT=YES,CHANNELS=\"2\",URI=\"audio-aac.m3u8\"\n\
#EXT-X-MEDIA:TYPE=AUDIO,GROUP-ID=\"ec3\",NAME=\"English\",LANGUAGE=\"en\",DEFAULT=YES,AUTOSELECT=YES,CHANNELS=\"6\",URI=\"audio-eac3.m3u8\"\n\
#EXT-X-STREAM-INF:BANDWIDTH=300000,RESOLUTION=64x64,CODECS=\"avc1.42c00a,mp4a.40.2\",AUDIO=\"aac\"\n\
video.m3u8\n\
#EXT-X-STREAM-INF:BANDWIDTH=500000,RESOLUTION=64x64,CODECS=\"avc1.42c00a,ec-3\",AUDIO=\"ec3\"\n\
video.m3u8\n"
        .to_vec()
}

fn origin() -> LoopbackOrigin {
    LoopbackOrigin::start(HashMap::from([
        ("/master.m3u8", dolby_and_aac_master()),
        ("/video.m3u8", media_playlist("video-0.ts")),
        ("/video-0.ts", VIDEO_ONLY_SEGMENT.to_vec()),
        ("/audio-aac.m3u8", media_playlist("aac-0.ts")),
        ("/aac-0.ts", AAC_STEREO_SEGMENT.to_vec()),
        ("/audio-eac3.m3u8", media_playlist("eac3-0.ts")),
        ("/eac3-0.ts", EAC3_SURROUND_SEGMENT.to_vec()),
    ]))
}

/// Один source на весь сценарий: reopen обязан сохранить source lineage (как `clone` в app).
pub(super) fn source_for(origin: &LoopbackOrigin) -> NativeHlsUrl {
    NativeHlsUrl::new(
        origin.target("/master.m3u8"),
        SafeMediaLabel::from_service_safe_label("HLS audio versions"),
    )
}

/// Production admission без утверждений об исходе (для сценариев с ожидаемой ошибкой).
pub(super) fn try_prepare(
    source: &NativeHlsUrl,
    expected_selection: Option<&WebMediaSemanticSelectionRequest>,
    start: HlsVodStartIntent,
) -> anyhow::Result<NativeHlsAttempt<PreparedNativeHlsMedia>> {
    let mut app_config = AppConfig::default();
    app_config.yt_dlp.enabled = false;
    app_config.player.preferred_video_codec_order = vec![fastiplayer_config::VideoCodec::H264];
    let system_capabilities = h264_fixture_capabilities();
    let mut port = ProductionNativeHlsAdmissionPort::new(NativeHlsPreparationRequest {
        source,
        expected_selection,
        network_config: &app_config.network,
        web_media_config: &app_config.web_media,
        demux_config: &app_config.player.demux,
        preferred_video_codec_order: &app_config.player.preferred_video_codec_order,
        system_capabilities: &system_capabilities,
        audio_capabilities: ProductionAudioDecoderFactory::default()
            .audio_decode_capability_snapshot(),
        start,
        cancellation: CancellationToken::new(),
    });
    NativeHlsAdmissionPort::prepare(&mut port)
}

/// Production admission: первичное открытие (`None`) или semantic reopen выбранной версии.
pub(super) fn prepare(
    source: &NativeHlsUrl,
    expected_selection: Option<&WebMediaSemanticSelectionRequest>,
    start: HlsVodStartIntent,
) -> PreparedNativeHlsMedia {
    match try_prepare(source, expected_selection, start) {
        Ok(NativeHlsAttempt::Prepared(prepared)) => prepared,
        Ok(_) => panic!("native HLS master with audio renditions must stay native"),
        Err(error) => panic!("native HLS audio-version preparation failed: {error:#}"),
    }
}

/// Независимая audio axis установленного каталога вместе с generation-fence-ами.
pub(super) fn audio_axis(
    prepared: &PreparedNativeHlsMedia,
) -> (
    ComponentVariantSelectionAction,
    WebMediaAudioComponentVariantAxis,
) {
    let configuration = prepared.source_state.stream_configuration();
    let WebMediaComponentVariantProjection::Installed(
        WebMediaInstalledComponentVariantPresentation::VideoAndAudio {
            catalog_generation,
            audio,
            ..
        },
    ) = configuration.component_variant_projection()
    else {
        panic!("audio renditions must form an independent audio axis next to video");
    };
    let template = ComponentVariantSelectionAction {
        parent_generation: configuration.generation(),
        catalog_generation,
        axis: WebMediaComponentVariantAxisKind::Audio,
        variant_index: audio.active_index,
    };
    (template, audio)
}

pub(super) fn only_audio_track(prepared: &PreparedNativeHlsMedia) -> TrackInfo {
    let audio: Vec<&TrackInfo> = prepared
        .tracks()
        .iter()
        .filter(|track| track.kind == TrackKind::Audio)
        .collect();
    let [track] = audio.as_slice() else {
        panic!(
            "exactly one audio track per installed version, got {}",
            audio.len()
        );
    };
    (*track).clone()
}

pub(super) fn index_of(axis: &WebMediaAudioComponentVariantAxis, codec: CodecFamily) -> usize {
    axis.variants
        .iter()
        .position(|variant| variant.codec == Some(codec))
        .unwrap_or_else(|| panic!("catalog lists no {codec:?} version"))
}

#[test]
fn selecting_eac3_version_reopens_with_position_and_reaches_the_decoder() {
    let origin = origin();
    let source = source_for(&origin);
    let mut initial = prepare(&source, None, HlsVodStartIntent::Beginning);

    // Каталог: обе версии с кодеком и числом каналов; по умолчанию активна AAC (пакет E).
    let (template, axis) = audio_axis(&initial);
    assert_eq!(axis.variants.len(), 2, "both audio versions are listed");
    let aac_index = index_of(&axis, CodecFamily::Aac);
    let eac3_index = index_of(&axis, CodecFamily::Eac3);
    assert_eq!(axis.variants[aac_index].channels, Some(2));
    assert_eq!(axis.variants[eac3_index].channels, Some(6));
    assert_eq!(
        axis.variants[eac3_index].language_label.as_deref(),
        Some("en")
    );
    assert_eq!(
        axis.active_index, aac_index,
        "AAC stays the default version"
    );
    let aac_track = only_audio_track(&initial);
    assert_eq!(aac_track.codec_id, "A_AAC");
    assert_eq!(aac_track.channels, Some(2));
    let aac_packets = audio_packets(initial.demuxer.as_mut(), aac_track.id.get());
    let (_, aac_channels, aac_rms, _) = decode_to_pcm(
        &aac_track.codec_id,
        aac_track.id.get(),
        aac_track.sample_rate,
        aac_track.channels,
        &aac_packets,
    );
    assert_eq!(aac_channels, 2);
    assert!(
        aac_rms > MINIMUM_TONE_RMS,
        "AAC version plays, rms={aac_rms}"
    );

    // Меню «♪» публикует ровно такое действие для строки E-AC-3.
    let eac3_action = ComponentVariantSelectionAction {
        variant_index: eac3_index,
        ..template
    };
    let configuration = initial.source_state.stream_configuration();
    let ComponentVariantActionResolution::SemanticReopen(semantic) = configuration
        .resolve_component_variant_action(eac3_action)
        .expect("fresh E-AC-3 action resolves")
    else {
        panic!("a non-active version must request a reopen");
    };
    let Some(NativeHlsOpenIntent::SemanticSelection(reopen_selection)) =
        initial.source_state.switch_intent_for_component(semantic)
    else {
        panic!("native HLS turns the component choice into a semantic reopen");
    };
    // Повторный выбор активной версии — no-op без reopen-а.
    assert_eq!(
        configuration.resolve_component_variant_action(ComponentVariantSelectionAction {
            variant_index: aac_index,
            ..template
        }),
        Ok(ComponentVariantActionResolution::NoChange)
    );

    let mut switched = prepare(
        &source,
        Some(&reopen_selection),
        HlsVodStartIntent::RestoreOrBeginning(MediaTime::from_duration(RESTORED_POSITION)),
    );
    assert!(
        matches!(
            switched.lifecycle,
            PreparedNativeHlsLifecycle::Vod {
                initial_position: PreparedInitialPosition::PositionedAt { .. },
                ..
            }
        ),
        "the reopen lands at the preserved position, not at the beginning"
    );
    let (switched_template, switched_axis) = audio_axis(&switched);
    assert_eq!(
        switched_axis.variants[switched_axis.active_index].codec,
        Some(CodecFamily::Eac3),
        "E-AC-3 is now the installed version"
    );
    assert!(
        switched
            .tracks()
            .iter()
            .any(|track| track.kind == TrackKind::Video),
        "the video axis is preserved by an audio-only switch"
    );
    let eac3_track = only_audio_track(&switched);
    assert_eq!(eac3_track.codec_id, "A_EAC3");
    assert_eq!(eac3_track.channels, Some(6));
    let eac3_packets = audio_packets(switched.demuxer.as_mut(), eac3_track.id.get());
    assert!(
        !eac3_packets.is_empty(),
        "E-AC-3 packets reach the demux output"
    );
    let (sample_rate, channels, rms, _) = decode_to_pcm(
        &eac3_track.codec_id,
        eac3_track.id.get(),
        eac3_track.sample_rate,
        eac3_track.channels,
        &eac3_packets,
    );
    assert_eq!((sample_rate, channels), (48_000, 6));
    assert!(rms > MINIMUM_TONE_RMS, "E-AC-3 version plays, rms={rms}");

    // Действие из старого меню после reopen-а отвергается typed-fence-ом (текущий звук не трогаем).
    assert!(matches!(
        switched
            .source_state
            .stream_configuration()
            .resolve_component_variant_action(eac3_action),
        Err(ComponentVariantActionError::StaleParentGeneration { .. }
            | ComponentVariantActionError::StaleCatalogGeneration { .. })
    ));
    assert_ne!(
        switched_template.parent_generation,
        template.parent_generation
    );
}
