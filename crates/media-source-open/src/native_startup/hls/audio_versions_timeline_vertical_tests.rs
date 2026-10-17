//! Сквозные сценарии backlog #20 (`user/ac3-eac3-support/`): версии звука HLS, нарезанные
//! не так, как видео. Loopback HTTP → production native HLS admission → catalog → MPEG-TS
//! demux → production audio decoder → PCM.
//!
//! - «невыровненная, но совместимая» версия (другое число сегментов, сумма `EXTINF` отличается
//!   меньше чем на сегмент) открывается, играет и переключается по времени;
//! - несовместимая не-default версия (другой монтаж) удаляется из списка, остальное играет;
//! - несовместимая default-версия даёт точную ошибку, а не «manifest metadata conflicts».

use std::collections::HashMap;
use std::time::Duration;

use media_core::MediaTime;
use web_media_core::CodecFamily;
use web_media_hls::{HlsCatalogBuildError, HlsCatalogSiblingRejectionReason, HlsVodStartIntent};

use super::NativeHlsAttempt;
use super::ac3_vertical_tests::{
    LoopbackOrigin, MINIMUM_TONE_RMS, audio_packets, decode_to_pcm, media_playlist,
};
use super::audio_versions_vertical_tests::{
    AAC_STEREO_SEGMENT, EAC3_SURROUND_SEGMENT, VIDEO_ONLY_SEGMENT, audio_axis,
    dolby_and_aac_master, index_of, only_audio_track, prepare, source_for, try_prepare,
};
use crate::native_web_source::hls::NativeHlsOpenIntent;
use crate::web_media_stream_model::component_variants::{
    ComponentVariantActionResolution, ComponentVariantSelectionAction,
};

/// Та же AAC-версия, порезанная на 3 сегмента по кадрам AAC (см. fixtures README).
const AAC_SEGMENTS: [&[u8]; 3] = [
    include_bytes!("../../../tests/fixtures/hls-audio-versions/audio-aac-2ch-seg0.ts"),
    include_bytes!("../../../tests/fixtures/hls-audio-versions/audio-aac-2ch-seg1.ts"),
    include_bytes!("../../../tests/fixtures/hls-audio-versions/audio-aac-2ch-seg2.ts"),
];

/// Позиция переключения на E-AC-3 (внутри первого AAC-сегмента).
const FIRST_SWITCH_POSITION: Duration = Duration::from_millis(200);
/// Позиция обратного переключения: по времени попадает во второй AAC-сегмент, у видео это
/// всё ещё первый (единственный) сегмент — сопоставление обязано идти по времени.
const SECOND_SWITCH_POSITION: Duration = Duration::from_millis(300);

/// AAC: 3 сегмента 0.256 + 0.256 + 0.021333 = 0.533 с (видео — 1 сегмент 0.5 с).
fn aac_three_segment_playlist() -> Vec<u8> {
    b"#EXTM3U\n#EXT-X-VERSION:3\n#EXT-X-TARGETDURATION:1\n#EXT-X-MEDIA-SEQUENCE:0\n\
#EXT-X-PLAYLIST-TYPE:VOD\n\
#EXTINF:0.256000,\naac-0.ts\n#EXTINF:0.256000,\naac-1.ts\n#EXTINF:0.021333,\naac-2.ts\n\
#EXT-X-ENDLIST\n"
        .to_vec()
}

/// E-AC-3 с `EXTINF`, округлённым до целых кадров 32 мс (16 кадров = 0.512 с).
fn eac3_frame_rounded_playlist() -> Vec<u8> {
    b"#EXTM3U\n#EXT-X-VERSION:3\n#EXT-X-TARGETDURATION:1\n#EXT-X-MEDIA-SEQUENCE:0\n\
#EXT-X-PLAYLIST-TYPE:VOD\n#EXTINF:0.512,\neac3-0.ts\n#EXT-X-ENDLIST\n"
        .to_vec()
}

/// Плейлист «другого монтажа»: 4 × 0.5 с = 2 с против 0.5 с видео (разница 1.5 с больше
/// допуска в одну target duration = 1 с). Сегмент повторяется: каталогу важна шкала manifest-а.
fn other_cut_playlist(segment: &str) -> Vec<u8> {
    let entries = (0..4)
        .map(|_| format!("#EXTINF:0.5,\n{segment}\n"))
        .collect::<String>();
    format!(
        "#EXTM3U\n#EXT-X-VERSION:3\n#EXT-X-TARGETDURATION:1\n#EXT-X-MEDIA-SEQUENCE:0\n\
         #EXT-X-PLAYLIST-TYPE:VOD\n{entries}#EXT-X-ENDLIST\n"
    )
    .into_bytes()
}

/// Origin с master-ом из `audio_versions_vertical_tests` и заданными audio playlist-ами.
fn origin_with(aac_playlist: Vec<u8>, eac3_playlist: Vec<u8>) -> LoopbackOrigin {
    LoopbackOrigin::start(HashMap::from([
        ("/master.m3u8", dolby_and_aac_master()),
        ("/video.m3u8", media_playlist("video-0.ts")),
        ("/video-0.ts", VIDEO_ONLY_SEGMENT.to_vec()),
        ("/audio-aac.m3u8", aac_playlist),
        ("/aac-whole.ts", AAC_STEREO_SEGMENT.to_vec()),
        ("/aac-0.ts", AAC_SEGMENTS[0].to_vec()),
        ("/aac-1.ts", AAC_SEGMENTS[1].to_vec()),
        ("/aac-2.ts", AAC_SEGMENTS[2].to_vec()),
        ("/audio-eac3.m3u8", eac3_playlist),
        ("/eac3-0.ts", EAC3_SURROUND_SEGMENT.to_vec()),
    ]))
}

/// Декодирует установленную версию звука и возвращает (кодек, каналы, RMS).
fn play_installed_audio(prepared: &mut super::PreparedNativeHlsMedia) -> (String, u32, f32) {
    let track = only_audio_track(prepared);
    let packets = audio_packets(prepared.demuxer.as_mut(), track.id.get());
    assert!(
        !packets.is_empty(),
        "{} packets reach demux",
        track.codec_id
    );
    let (_, channels, rms, _) = decode_to_pcm(
        &track.codec_id,
        track.id.get(),
        track.sample_rate,
        track.channels,
        &packets,
    );
    (track.codec_id, channels, rms)
}

/// Переключает установленную версию на `codec` через то же действие, что меню «♪».
fn switch_audio_version(
    source: &crate::native_web_source::hls::NativeHlsUrl,
    current: &super::PreparedNativeHlsMedia,
    codec: CodecFamily,
    position: Duration,
) -> super::PreparedNativeHlsMedia {
    let (template, axis) = audio_axis(current);
    let action = ComponentVariantSelectionAction {
        variant_index: index_of(&axis, codec),
        ..template
    };
    let ComponentVariantActionResolution::SemanticReopen(semantic) = current
        .source_state
        .stream_configuration()
        .resolve_component_variant_action(action)
        .expect("fresh action resolves")
    else {
        panic!("a non-active version must request a reopen");
    };
    let Some(NativeHlsOpenIntent::SemanticSelection(selection)) =
        current.source_state.switch_intent_for_component(semantic)
    else {
        panic!("native HLS turns the component choice into a semantic reopen");
    };
    prepare(
        source,
        Some(&selection),
        HlsVodStartIntent::RestoreOrBeginning(MediaTime::from_duration(position)),
    )
}

#[test]
fn unaligned_but_compatible_versions_open_play_and_switch_by_time() {
    let origin = origin_with(aac_three_segment_playlist(), eac3_frame_rounded_playlist());
    let source = source_for(&origin);
    // До исправления #20: AAC (3 сегмента, 0.533 с) не «выровнен» с видео (1 сегмент, 0.5 с)
    // → нет compatibility edge → всё открытие падало.
    let mut initial = prepare(&source, None, HlsVodStartIntent::Beginning);
    let (_, axis) = audio_axis(&initial);
    assert_eq!(axis.variants.len(), 2, "both versions stay in the list");
    assert_eq!(
        axis.variants[axis.active_index].codec,
        Some(CodecFamily::Aac)
    );
    let (codec, channels, rms) = play_installed_audio(&mut initial);
    assert_eq!((codec.as_str(), channels), ("A_AAC", 2));
    assert!(rms > MINIMUM_TONE_RMS, "AAC plays, rms={rms}");

    let mut eac3 =
        switch_audio_version(&source, &initial, CodecFamily::Eac3, FIRST_SWITCH_POSITION);
    let (codec, channels, rms) = play_installed_audio(&mut eac3);
    assert_eq!((codec.as_str(), channels), ("A_EAC3", 6));
    assert!(
        rms > MINIMUM_TONE_RMS,
        "E-AC-3 plays after the switch, rms={rms}"
    );

    // Обратно на AAC в момент, который у AAC приходится на второй сегмент.
    let mut aac_again =
        switch_audio_version(&source, &eac3, CodecFamily::Aac, SECOND_SWITCH_POSITION);
    let (codec, channels, rms) = play_installed_audio(&mut aac_again);
    assert_eq!((codec.as_str(), channels), ("A_AAC", 2));
    assert!(
        rms > MINIMUM_TONE_RMS,
        "AAC plays after switching back, rms={rms}"
    );
}

#[test]
fn incompatible_sibling_version_is_dropped_and_the_default_still_plays() {
    let origin = origin_with(
        media_playlist("aac-whole.ts"),
        other_cut_playlist("eac3-0.ts"),
    );
    let source = source_for(&origin);
    let mut prepared = prepare(&source, None, HlsVodStartIntent::Beginning);
    let (_, axis) = audio_axis(&prepared);
    assert_eq!(
        axis.variants
            .iter()
            .map(|variant| variant.codec)
            .collect::<Vec<_>>(),
        vec![Some(CodecFamily::Aac)],
        "the E-AC-3 version from another cut is removed from the version list"
    );
    let (codec, channels, rms) = play_installed_audio(&mut prepared);
    assert_eq!((codec.as_str(), channels), ("A_AAC", 2));
    assert!(rms > MINIMUM_TONE_RMS, "remaining version plays, rms={rms}");
}

#[test]
fn incompatible_default_version_fails_with_a_precise_error() {
    let origin = origin_with(
        other_cut_playlist("aac-whole.ts"),
        eac3_frame_rounded_playlist(),
    );
    let source = source_for(&origin);
    let error = match try_prepare(&source, None, HlsVodStartIntent::Beginning) {
        Err(error) => error,
        Ok(NativeHlsAttempt::Prepared(_)) => {
            panic!("default audio from another cut must not open silently")
        }
        Ok(_) => panic!("native HLS master must not fall back"),
    };
    let rejection = error
        .chain()
        .find_map(|cause| cause.downcast_ref::<HlsCatalogBuildError>())
        .unwrap_or_else(|| panic!("typed HLS catalog rejection expected, got {error:#}"));
    assert!(
        matches!(
            rejection,
            HlsCatalogBuildError::ProviderDefaultRejected {
                reason: HlsCatalogSiblingRejectionReason::AudioTimelineDurationMismatch,
            }
        ),
        "unexpected rejection {rejection:?}"
    );
    let message = format!("{error:#}");
    assert!(
        message.contains("audio version duration differs from the video"),
        "the error names what conflicts: {message}"
    );
    assert!(
        !message.contains("manifest metadata conflicts with probed media"),
        "the old misleading text is gone: {message}"
    );
}
