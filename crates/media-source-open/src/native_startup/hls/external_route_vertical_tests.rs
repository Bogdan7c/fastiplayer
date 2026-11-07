//! Внешний маршрут native HLS (мост в браузер): тот же готовый HLS, но вся сеть —
//! через поддельный транспорт в памяти, адреса — номерки `…bridge.invalid/<N>`.
//!
//! Каждый тест доводит поток до результата, который видит плеер: пакеты видео и
//! звука из демуксера, PCM из production-декодера, каталог версий звука. Журнал
//! транспорта доказывает, что ресурсы шли через «розетку», а домен `.invalid`
//! доказывает, что обычная сеть не тронута (иначе запрос упал бы на DNS).

use aes::Aes128;
use cbc::Encryptor;
use cbc::cipher::block_padding::Pkcs7;
use cbc::cipher::{BlockModeEncrypt, KeyIvInit};
use std::thread;
use std::time::{Duration, Instant};

use audio::{AudioDecodeCapabilityProvider, ProductionAudioDecoderFactory};
use fastiplayer_config::AppConfig;
use media_core::{DemuxReadEvent, Demuxer, Packet, TrackKind};
use source_core::CancellationToken;
use web_media_core::{CodecFamily, WebMediaSemanticSelectionRequest};
use web_media_hls::HlsVodStartIntent;

use super::ac3_vertical_tests::{
    MINIMUM_TONE_RMS, VERTICAL_DEADLINE, h264_fixture_capabilities, media_playlist,
};
use super::audio_versions_vertical_tests::{
    AAC_STEREO_SEGMENT, EAC3_SURROUND_SEGMENT, VIDEO_ONLY_SEGMENT, audio_axis, index_of,
    only_audio_track,
};
use super::external_route_test_transport::{
    InMemoryHlsTransport, InMemoryResource, bridge_address, bridge_source,
};
use super::{
    ExternalHlsTransport, NativeHlsAdmissionPort, NativeHlsAttempt, NativeHlsNetworkRoute,
    NativeHlsPreparationRequest, PreparedNativeHlsLifecycle, PreparedNativeHlsMedia,
    ProductionNativeHlsAdmissionPort, VodEndpointRecoveryMode,
};
use crate::ac3_test_support::decode_track_packets;
use crate::native_web_source::hls::{NativeHlsOpenIntent, NativeHlsUrl};
use crate::web_media_stream_model::component_variants::{
    ComponentVariantActionResolution, ComponentVariantSelectionAction,
};

/// H.264 64×64 + AC-3 стерео 48 кГц в одном MPEG-TS сегменте 0.5 с.
const H264_AC3_STEREO_SEGMENT: &[u8] =
    include_bytes!("../../../../mpeg-ts-demux/tests/fixtures/ac3/h264-ac3-stereo-48k.ts");
/// Фрагментированный MP4: ftyp+moov, два фрагмента moof+mdat (H.264 + E-AC-3 5.1).
const FRAGMENTED_H264_EAC3: &[u8] =
    include_bytes!("../../../../symphonia-demux/tests/fixtures/ac3/h264-eac3-5p1-fragmented.mp4");
/// Пауза между опросами демуксера, который временно не готов.
const READINESS_POLL_INTERVAL: Duration = Duration::from_millis(5);

/// Native HLS через внешний маршрут с заданным транспортом.
pub(super) fn try_prepare_through(
    transport: &std::sync::Arc<InMemoryHlsTransport>,
    source: &NativeHlsUrl,
    expected_selection: Option<&WebMediaSemanticSelectionRequest>,
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
        start: HlsVodStartIntent::Beginning,
        cancellation: CancellationToken::new(),
        network_route: NativeHlsNetworkRoute::External(ExternalHlsTransport::new(
            std::sync::Arc::clone(transport) as _,
        )),
    });
    NativeHlsAdmissionPort::prepare(&mut port)
}

/// То же, но успешное native-открытие обязательно (иначе тест падает с причиной).
pub(super) fn prepare_through(
    transport: &std::sync::Arc<InMemoryHlsTransport>,
    source: &NativeHlsUrl,
    expected_selection: Option<&WebMediaSemanticSelectionRequest>,
) -> PreparedNativeHlsMedia {
    match try_prepare_through(transport, source, expected_selection) {
        Ok(NativeHlsAttempt::Prepared(prepared)) => prepared,
        Ok(NativeHlsAttempt::RequiresExtractorFallback(trigger)) => {
            panic!("valid bridge HLS must stay native, got fallback {trigger:?}")
        }
        Err(error) => panic!("bridge native HLS preparation failed: {error:#}"),
    }
}

/// Пакеты видео и звука до конца потока.
struct DrainedPackets {
    video: Vec<Packet>,
    audio: Vec<Packet>,
}

/// Читает демуксер до EOS, переживая временную неготовность фонового чтения.
fn drain_to_end(demuxer: &mut dyn Demuxer) -> DrainedPackets {
    let deadline = Instant::now() + VERTICAL_DEADLINE;
    let mut drained = DrainedPackets {
        video: Vec::new(),
        audio: Vec::new(),
    };
    while Instant::now() < deadline {
        match demuxer.next_event().expect("bridge HLS demux event") {
            DemuxReadEvent::Packet(packet) if packet.kind == TrackKind::Video => {
                drained.video.push(packet);
            }
            DemuxReadEvent::Packet(packet) if packet.kind == TrackKind::Audio => {
                drained.audio.push(packet);
            }
            DemuxReadEvent::Packet(_)
            | DemuxReadEvent::TracksChanged(_)
            | DemuxReadEvent::MediaMetadataChanged(_) => {}
            DemuxReadEvent::TemporarilyUnavailable(_) => thread::sleep(READINESS_POLL_INTERVAL),
            DemuxReadEvent::EndOfStream => return drained,
        }
    }
    panic!("bridge HLS demux did not reach EOS before deadline");
}

/// Видео дошло до ключевого кадра, а звук — до настоящего PCM с тоном.
fn assert_video_and_audible_audio(prepared: &mut PreparedNativeHlsMedia) {
    let audio_track = only_audio_track(prepared);
    let drained = drain_to_end(prepared.demuxer.as_mut());
    assert!(
        drained
            .video
            .iter()
            .any(|packet| packet.keyframe.is_known_keyframe()),
        "video reaches the demuxer starting from a keyframe ({} packets)",
        drained.video.len()
    );
    assert!(!drained.audio.is_empty(), "audio packets reach the demuxer");
    let decoded = decode_track_packets(&audio_track, &drained.audio);
    let rms = root_mean_square(&decoded.interleaved);
    assert!(
        rms > MINIMUM_TONE_RMS,
        "the production decoder turns bridge audio into an audible tone, rms={rms}"
    );
}

fn root_mean_square(interleaved: &[f32]) -> f32 {
    (interleaved
        .iter()
        .map(|sample| sample * sample)
        .sum::<f32>()
        / interleaved.len().max(1) as f32)
        .sqrt()
}

/// Внешний маршрут честно не даёт приложению восстановление адреса VOD.
fn assert_vod_without_endpoint_recovery(prepared: &PreparedNativeHlsMedia) {
    assert!(
        matches!(
            prepared.lifecycle,
            PreparedNativeHlsLifecycle::Vod {
                endpoint_recovery: VodEndpointRecoveryMode::NotAvailable,
                ..
            }
        ),
        "bridge VOD must not pretend that the app drives endpoint recovery"
    );
}

/// Каждый запрос шёл через розетку по номеркам, корень — ровно один раз на открытие.
fn assert_only_bridge_requests(transport: &InMemoryHlsTransport, root_handle: u32, opens: usize) {
    assert!(transport.every_request_used_bridge_addresses());
    assert_eq!(
        transport.requests_for(root_handle),
        opens,
        "the root playlist is fetched exactly once per open"
    );
}

#[test]
fn bridge_ts_vod_reaches_video_and_decoded_audio_without_network() {
    let transport = InMemoryHlsTransport::with_resources([
        (
            1,
            InMemoryResource::Body(media_playlist(&bridge_address(2))),
        ),
        (2, InMemoryResource::Body(H264_AC3_STEREO_SEGMENT.to_vec())),
    ]);
    let mut prepared = prepare_through(&transport, &bridge_source(1), None);

    assert_vod_without_endpoint_recovery(&prepared);
    assert_video_and_audible_audio(&mut prepared);
    assert_only_bridge_requests(&transport, 1, 1);
    assert!(
        transport.requests_for(2) >= 1,
        "the segment is fetched by its handle"
    );
}

/// Границы top-level box-ов fMP4: init (ftyp+moov) и фрагменты (moof+mdat).
struct FragmentedLayout {
    initialization: std::ops::Range<usize>,
    fragments: Vec<std::ops::Range<usize>>,
}

/// Размечает fMP4 по 32-битным размерам box-ов; `mfra` в разметку не входит.
fn fragmented_layout(file: &[u8]) -> FragmentedLayout {
    let mut layout = FragmentedLayout {
        initialization: 0..0,
        fragments: Vec::new(),
    };
    let mut offset = 0;
    while offset + 8 <= file.len() {
        let size_bytes: [u8; 4] = file[offset..offset + 4].try_into().expect("box size");
        let size = u32::from_be_bytes(size_bytes) as usize;
        assert!(size >= 8, "fixture uses 32-bit box sizes only");
        let end = offset + size;
        match &file[offset + 4..offset + 8] {
            b"ftyp" | b"moov" => layout.initialization.end = end,
            b"moof" => layout.fragments.push(offset..end),
            b"mdat" => {
                layout
                    .fragments
                    .last_mut()
                    .expect("mdat follows its moof")
                    .end = end;
            }
            _ => {}
        }
        offset = end;
    }
    assert_eq!(layout.fragments.len(), 2, "two keyframe fragments");
    layout
}

/// `BYTERANGE` в форме HLS: `<длина>@<смещение>`.
fn byte_range(range: &std::ops::Range<usize>) -> String {
    format!("{}@{}", range.len(), range.start)
}

#[test]
fn bridge_fmp4_vod_with_byte_ranges_reaches_video_and_decoded_audio() {
    // Init и оба фрагмента — куски ОДНОГО ресурса-номерка: транспорт обязан отдать
    // ровно запрошенный диапазон, иначе ни init, ни фрагменты не разберутся.
    let layout = fragmented_layout(FRAGMENTED_H264_EAC3);
    let file = bridge_address(11);
    let media = format!(
        "#EXTM3U\n#EXT-X-VERSION:7\n#EXT-X-TARGETDURATION:1\n#EXT-X-MEDIA-SEQUENCE:0\n\
         #EXT-X-PLAYLIST-TYPE:VOD\n#EXT-X-MAP:URI=\"{file}\",BYTERANGE=\"{init}\"\n\
         #EXTINF:0.5,\n#EXT-X-BYTERANGE:{first}\n{file}\n\
         #EXTINF:0.5,\n#EXT-X-BYTERANGE:{second}\n{file}\n#EXT-X-ENDLIST\n",
        init = byte_range(&layout.initialization),
        first = byte_range(&layout.fragments[0]),
        second = byte_range(&layout.fragments[1]),
    );
    let transport = InMemoryHlsTransport::with_resources([
        (10, InMemoryResource::Body(media.into_bytes())),
        (11, InMemoryResource::Body(FRAGMENTED_H264_EAC3.to_vec())),
    ]);
    let mut prepared = prepare_through(&transport, &bridge_source(10), None);

    assert_vod_without_endpoint_recovery(&prepared);
    assert_video_and_audible_audio(&mut prepared);
    assert_only_bridge_requests(&transport, 10, 1);
    assert!(
        transport.requests_for(11) >= 3,
        "init and both fragments are separate ranged requests to one handle"
    );
}

/// Master по Apple authoring spec с абсолютными адресами-номерками (как после экстеншена).
fn bridge_audio_versions_master() -> Vec<u8> {
    format!(
        "#EXTM3U\n\
#EXT-X-MEDIA:TYPE=AUDIO,GROUP-ID=\"aac\",NAME=\"English\",LANGUAGE=\"en\",DEFAULT=YES,AUTOSELECT=YES,CHANNELS=\"2\",URI=\"{aac}\"\n\
#EXT-X-MEDIA:TYPE=AUDIO,GROUP-ID=\"ec3\",NAME=\"English\",LANGUAGE=\"en\",DEFAULT=YES,AUTOSELECT=YES,CHANNELS=\"6\",URI=\"{eac3}\"\n\
#EXT-X-STREAM-INF:BANDWIDTH=300000,RESOLUTION=64x64,CODECS=\"avc1.42c00a,mp4a.40.2\",AUDIO=\"aac\"\n{video}\n\
#EXT-X-STREAM-INF:BANDWIDTH=500000,RESOLUTION=64x64,CODECS=\"avc1.42c00a,ec-3\",AUDIO=\"ec3\"\n{video}\n",
        aac = bridge_address(22),
        eac3 = bridge_address(24),
        video = bridge_address(21),
    )
    .into_bytes()
}

#[test]
fn bridge_master_with_two_variants_lists_catalog_and_switches_audio_through_bridge() {
    let transport = InMemoryHlsTransport::with_resources([
        (20, InMemoryResource::Body(bridge_audio_versions_master())),
        (
            21,
            InMemoryResource::Body(media_playlist(&bridge_address(25))),
        ),
        (
            22,
            InMemoryResource::Body(media_playlist(&bridge_address(23))),
        ),
        (23, InMemoryResource::Body(AAC_STEREO_SEGMENT.to_vec())),
        (
            24,
            InMemoryResource::Body(media_playlist(&bridge_address(26))),
        ),
        (25, InMemoryResource::Body(VIDEO_ONLY_SEGMENT.to_vec())),
        (26, InMemoryResource::Body(EAC3_SURROUND_SEGMENT.to_vec())),
    ]);
    let source = bridge_source(20);
    let mut initial = prepare_through(&transport, &source, None);

    // Каталог из двух вариантов виден через мост; по умолчанию активна AAC.
    let (template, axis) = audio_axis(&initial);
    assert_eq!(axis.variants.len(), 2, "both master variants are listed");
    let aac_index = index_of(&axis, CodecFamily::Aac);
    let eac3_index = index_of(&axis, CodecFamily::Eac3);
    assert_eq!(axis.active_index, aac_index);
    assert_eq!(only_audio_track(&initial).codec_id, "A_AAC");
    assert_video_and_audible_audio(&mut initial);

    // Выбор второго варианта — semantic reopen через тот же внешний маршрут.
    let configuration = initial.source_state.stream_configuration();
    let ComponentVariantActionResolution::SemanticReopen(semantic) = configuration
        .resolve_component_variant_action(ComponentVariantSelectionAction {
            variant_index: eac3_index,
            ..template
        })
        .expect("fresh E-AC-3 action resolves")
    else {
        panic!("a non-active variant must request a reopen");
    };
    let Some(NativeHlsOpenIntent::SemanticSelection(reopen_selection)) =
        initial.source_state.switch_intent_for_component(semantic)
    else {
        panic!("native HLS turns the variant choice into a semantic reopen");
    };
    let mut switched = prepare_through(&transport, &source, Some(&reopen_selection));
    assert_eq!(only_audio_track(&switched).codec_id, "A_EAC3");
    assert_video_and_audible_audio(&mut switched);

    assert_only_bridge_requests(&transport, 20, 2);
    assert!(
        transport.requests_for(26) >= 1,
        "the E-AC-3 segment came through the bridge"
    );
}

/// Шифрует сегмент AES-128-CBC с PKCS#7, как это делает HLS-упаковщик.
fn encrypt_aes128_cbc(plaintext: &[u8], key: [u8; 16], iv: [u8; 16]) -> Vec<u8> {
    // Место под паддинг: PKCS#7 добавляет от 1 до 16 байтов.
    let mut buffer = vec![0_u8; plaintext.len() + 16];
    buffer[..plaintext.len()].copy_from_slice(plaintext);
    let ciphertext_length = Encryptor::<Aes128>::new((&key).into(), (&iv).into())
        .encrypt_padded::<Pkcs7>(&mut buffer, plaintext.len())
        .expect("encrypt fixture segment")
        .len();
    buffer.truncate(ciphertext_length);
    buffer
}

fn hex(bytes: &[u8]) -> String {
    bytes.iter().map(|byte| format!("{byte:02x}")).collect()
}

#[test]
fn bridge_aes128_key_is_fetched_through_bridge_and_segment_plays() {
    let key = *b"bridge-key-16byt";
    let iv = [0x5a_u8; 16];
    let media = format!(
        "#EXTM3U\n#EXT-X-VERSION:3\n#EXT-X-TARGETDURATION:1\n#EXT-X-MEDIA-SEQUENCE:0\n\
         #EXT-X-PLAYLIST-TYPE:VOD\n#EXT-X-KEY:METHOD=AES-128,URI=\"{key_address}\",IV=0x{iv}\n\
         #EXTINF:0.5,\n{segment}\n#EXT-X-ENDLIST\n",
        key_address = bridge_address(31),
        iv = hex(&iv),
        segment = bridge_address(32),
    );
    let transport = InMemoryHlsTransport::with_resources([
        (30, InMemoryResource::Body(media.into_bytes())),
        (31, InMemoryResource::Body(key.to_vec())),
        (
            32,
            InMemoryResource::Body(encrypt_aes128_cbc(H264_AC3_STEREO_SEGMENT, key, iv)),
        ),
    ]);
    let mut prepared = prepare_through(&transport, &bridge_source(30), None);

    // Расшифрованный сегмент играет — значит ключ пришёл целым именно через мост.
    assert_video_and_audible_audio(&mut prepared);
    assert!(
        transport.requests_for(31) >= 1,
        "the AES-128 key is fetched by its handle through the bridge"
    );
    assert_only_bridge_requests(&transport, 30, 1);
}
