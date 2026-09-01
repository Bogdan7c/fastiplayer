//! N14B: общий VOD lifecycle поверх существующей N14A HLS consumer fixture.

use std::sync::Arc;
use std::thread;
use std::time::{Duration, Instant};

use audio::decoder::{
    AudioDecoderFactory, AudioDecoderHandle, EncodedAudioPacket, ProductionAudioDecoderFactory,
};
use codec_core::VideoCodec as DecodeVideoCodec;
use media_core::{
    DemuxReadEvent, DemuxRetryHint, DemuxSeekRequest, Demuxer, MediaTime, TrackInfo, TrackKind,
};
use player_core::{PreparedDemuxSeekOutcome, PreparedDemuxSeekRequestId, PreparedInitialPosition};
use playlist_core::{
    AddItemsOutcome, AutomaticEndedIntent, AutomaticNavigationOutcome, AutomaticStopReason,
    CachedPlaylistMetadata, ManualNavigationIntent, ManualNavigationOutcome,
    ManualNavigationPreview, PlaylistItemDraft, PlaylistMediaKind, PlaylistQueue, RepeatMode,
    SecretUrlLocator, TraversalCurrentMutationOutcome,
};
use web_media_hls::HlsVodStartIntent;

use super::super::*;
use super::native_hls_vertical::{
    ControlledHlsServer, alternate_component_selection, assert_decoder_render_audio,
    fixture_routes, native_request_parts, native_settings, prepare_native,
};
use crate::media_open::{NativeHlsUrl, SafeMediaLabel};
use crate::startup_media::native_hls::PreparedNativeHlsMedia;
use crate::web_media_open::content_probe_tests::direct_progressive::ZeroProcessSpy;
use crate::web_media_open::content_probe_tests::direct_progressive_webm::OffscreenWgpuHarness;
use crate::web_media_open::content_probe_tests::direct_progressive_webm::{
    decode_packet, open_decoder,
};
use crate::web_media_open::content_probe_tests::{
    assert_pcm_advances_clock, audio_packet_timing, decoder_config_from_track,
};
use crate::web_media_stream_model::component_variants::{
    ComponentVariantActionError, ComponentVariantSelectionAction, WebMediaComponentVariantAxisKind,
    WebMediaComponentVariantProjection, WebMediaInstalledComponentVariantPresentation,
};

/// Все ожидания worker receipt используют тот же bounded budget, что и N14A consumer proof.
const LIFECYCLE_DEADLINE: Duration = Duration::from_secs(10);
/// Forward seek остаётся внутри первого hermetic media segment-а.
const FORWARD_SEEK_POSITION: Duration = Duration::from_millis(100);

/// Production-like decoder lifecycle переживает live seek и сохраняет in-band H.264 config.
pub(super) struct PersistentHlsConsumer {
    video_track: TrackInfo,
    audio_track: TrackInfo,
    video_decoder: Box<video_backend_api::VideoBackendDecoderThreadHandle>,
    renderer_provider: video_backend_api::PresentFrameResourceProviderHandle,
    audio_decoder: AudioDecoderHandle,
}

impl PersistentHlsConsumer {
    /// Открывает те же FFmpeg/AAC owners один раз на весь live media instance.
    pub(super) fn new(demuxer: &dyn Demuxer, wgpu_harness: &OffscreenWgpuHarness) -> Self {
        let video_track = demuxer
            .tracks()
            .iter()
            .find(|track| track.kind == TrackKind::Video)
            .cloned()
            .expect("HLS live должен публиковать video track");
        let audio_track = demuxer
            .tracks()
            .iter()
            .find(|track| track.kind == TrackKind::Audio)
            .cloned()
            .expect("HLS live должен публиковать audio track");
        let (video_decoder, renderer_provider) =
            open_decoder(&video_track, wgpu_harness.queue(), DecodeVideoCodec::H264);
        let audio_decoder = ProductionAudioDecoderFactory::default()
            .create_decoder(decoder_config_from_track(&audio_track))
            .expect("production AAC decoder должен принять HLS live track");
        Self {
            video_track,
            audio_track,
            video_decoder,
            renderer_provider,
            audio_decoder,
        }
    }

    /// Повторяет production seek flush; следующий accepted AU обязан быть самодостаточным.
    pub(super) fn flush_for_seek(&mut self) {
        self.video_decoder
            .flush()
            .expect("production video decoder flush должен пройти");
        self.audio_decoder
            .reset()
            .expect("production audio decoder reset должен пройти");
    }

    /// Требует один новый submitted frame и один новый nonzero PCM batch.
    pub(super) fn consume(
        &mut self,
        demuxer: &mut dyn Demuxer,
        wgpu_harness: &mut OffscreenWgpuHarness,
    ) {
        let materializer = render_wgpu_video::HostPlanarWgpuFrameMaterializer::new(
            wgpu_harness.device(),
            wgpu_harness.queue(),
            self.renderer_provider.clone(),
        );
        let deadline = Instant::now() + LIFECYCLE_DEADLINE;
        let mut decoded_video_frame = None;
        let mut decoded_audio = false;
        while decoded_video_frame.is_none() || !decoded_audio {
            match demuxer.next_event().expect("читать HLS live demux event") {
                DemuxReadEvent::Packet(packet) if packet.track_id == self.video_track.id => {
                    for frame in decode_packet(self.video_decoder.as_ref(), packet) {
                        if decoded_video_frame.is_none() {
                            decoded_video_frame = Some(frame);
                        } else {
                            self.video_decoder.release_frame(frame.resource_handle);
                        }
                    }
                }
                DemuxReadEvent::Packet(packet) if packet.track_id == self.audio_track.id => {
                    let encoded_packet = EncodedAudioPacket::new(
                        packet.track_id.get(),
                        audio_packet_timing(&packet),
                        &packet.data,
                    );
                    let decoded_samples = self
                        .audio_decoder
                        .decode(&encoded_packet)
                        .expect("production AAC decoder должен декодировать HLS live packet");
                    if !decoded_samples.is_empty() {
                        assert_pcm_advances_clock(
                            &decoded_samples,
                            self.audio_decoder.sample_rate(),
                            self.audio_decoder.channels(),
                        );
                        decoded_audio = true;
                    }
                }
                DemuxReadEvent::Packet(_)
                | DemuxReadEvent::TracksChanged(_)
                | DemuxReadEvent::MediaMetadataChanged(_) => {}
                DemuxReadEvent::TemporarilyUnavailable(_) if Instant::now() < deadline => {
                    thread::sleep(DemuxRetryHint::MIN_RETRY_AFTER);
                }
                DemuxReadEvent::TemporarilyUnavailable(hint) => {
                    panic!("HLS live consumer readiness timeout: {hint:?}");
                }
                DemuxReadEvent::EndOfStream => {
                    panic!("active HLS live не должен публиковать queue-driving EOF")
                }
            }
            assert!(
                Instant::now() < deadline,
                "HLS live consumer timeout: video={}, audio={decoded_audio}",
                decoded_video_frame.is_some(),
            );
        }
        assert!(wgpu_harness.submit_and_release(
            &materializer,
            &self.renderer_provider,
            decoded_video_frame.expect("loop требует decoded frame"),
        ));
    }
}

/// Строит две durable queue rows одного stable root без временных endpoint-ов.
fn queue_for_stable_root(root_url: &str) -> (PlaylistQueue, Vec<playlist_core::PlaylistItemId>) {
    let mut queue = PlaylistQueue::new();
    let drafts: Vec<_> = ["native HLS primary", "native HLS alternate"]
        .into_iter()
        .map(|label| {
            PlaylistItemDraft::url(
                SecretUrlLocator::from_reopenable_url(root_url)
                    .expect("HLS fixture root обязан быть reopenable"),
                CachedPlaylistMetadata::new(label, PlaylistMediaKind::Video),
            )
        })
        .collect();
    let AddItemsOutcome::Added(item_ids) = queue.append_batch(drafts).expect("append HLS rows")
    else {
        panic!("две HLS rows не могут дать empty append");
    };
    let item_ids = item_ids.into_vec();
    assert!(matches!(
        queue
            .set_traversal_current(item_ids[0])
            .expect("set initial queue current"),
        TraversalCurrentMutationOutcome::Set(_)
    ));
    (queue, item_ids)
}

/// Требует concrete queue target и сохраняет opaque preview до consumer success.
fn expect_manual_target(
    outcome: ManualNavigationOutcome,
    expected_item_id: playlist_core::PlaylistItemId,
) -> ManualNavigationPreview {
    let ManualNavigationOutcome::OpenItem { item_id, preview } = outcome else {
        panic!("manual navigation должна вернуть concrete target");
    };
    assert_eq!(item_id, expected_item_id);
    preview
}

/// Коммитит queue current только после того, как соответствующий media consumer уже доказан.
fn commit_navigation_after_consumer(
    queue: &mut PlaylistQueue,
    preview: ManualNavigationPreview,
    expected_item_id: playlist_core::PlaylistItemId,
) {
    let token = queue
        .prepare_manual_navigation(preview)
        .expect("consumer-successful navigation должна пройти queue preflight");
    assert_eq!(token.target_item_id(), expected_item_id);
    let commit = queue.commit_manual_navigation(token);
    assert_eq!(commit.traversal_current().item_id(), expected_item_id);
}

/// Worker receipt недостаточен: после каждого успешного seek снова требуются frame и PCM.
fn assert_receipted_seek_reaches_consumers(
    prepared: &mut PreparedNativeHlsMedia,
    request_id: u64,
    requested_position: Duration,
    wgpu_harness: &mut OffscreenWgpuHarness,
) {
    let request_id = PreparedDemuxSeekRequestId::new(request_id);
    prepared
        .seek_port
        .enqueue_seek(request_id, DemuxSeekRequest::accurate(requested_position))
        .expect("native HLS VOD seek должен войти в worker");
    let deadline = Instant::now() + LIFECYCLE_DEADLINE;
    loop {
        if let Some(receipt) = prepared.seek_port.poll_seek_receipt() {
            assert_eq!(receipt.request_id, request_id);
            let PreparedDemuxSeekOutcome::Succeeded(result) = receipt.outcome else {
                panic!(
                    "native HLS VOD seek завершился неуспешно: {:?}",
                    receipt.outcome
                );
            };
            assert_eq!(result.requested_position.as_duration(), requested_position);
            break;
        }
        assert!(
            Instant::now() < deadline,
            "native HLS VOD seek receipt timeout"
        );
        thread::sleep(Duration::from_millis(1));
    }
    assert_decoder_render_audio(prepared.demuxer.as_mut(), wgpu_harness);
}

/// Сохраняет exact старое UI action, чтобы проверить оба generation fence после reopen.
fn stale_component_action(
    prepared: &PreparedNativeHlsMedia,
    alternate_index: usize,
) -> ComponentVariantSelectionAction {
    let configuration = prepared.source_state.stream_configuration();
    let WebMediaComponentVariantProjection::Installed(
        WebMediaInstalledComponentVariantPresentation::Coupled {
            catalog_generation, ..
        },
    ) = configuration.component_variant_projection()
    else {
        panic!("HLS lifecycle требует installed coupled catalog");
    };
    ComponentVariantSelectionAction {
        parent_generation: configuration.generation(),
        catalog_generation,
        axis: WebMediaComponentVariantAxisKind::Coupled,
        variant_index: alternate_index,
    }
}

/// Закрепляет VOD seek, switch, queue, graceful close/restart/restore и stale fencing.
#[test]
fn n14b_lifecycle_hls_vod_seek_switch_queue_restart_restore_and_stale_fence() {
    let server = ControlledHlsServer::start(fixture_routes());
    let process_spy = Arc::new(ZeroProcessSpy::default());
    let mut settings = native_settings();
    process_spy.install_as_attempt_owner(&mut settings);
    let stable_root = server.target("/master.m3u8");
    let source = NativeHlsUrl::new(
        stable_root.clone(),
        SafeMediaLabel::from_service_safe_label("controlled native HLS master"),
    );
    let stable_source_identity = source.source_identity();
    let (mut queue, item_ids) = queue_for_stable_root(stable_root.expose_secret_for_request());
    let mut wgpu_harness = OffscreenWgpuHarness::new();

    let mut initial = prepare_native(&source, None, &settings, HlsVodStartIntent::Beginning);
    assert_eq!(server.request_count("/master.m3u8"), 1);
    assert_decoder_render_audio(initial.demuxer.as_mut(), &mut wgpu_harness);
    assert_receipted_seek_reaches_consumers(
        &mut initial,
        1,
        FORWARD_SEEK_POSITION,
        &mut wgpu_harness,
    );
    assert_receipted_seek_reaches_consumers(&mut initial, 2, Duration::ZERO, &mut wgpu_harness);

    let (initial_index, alternate_index, alternate_selection) =
        alternate_component_selection(&initial.source_state);
    assert_eq!(initial_index, 1, "provider default должна быть fMP4 row");
    assert_eq!(alternate_index, 0, "switch должен выбрать TS row");
    let old_component_action = stale_component_action(&initial, alternate_index);
    let expected_ts_selection = alternate_selection.clone();
    let next_preview = expect_manual_target(
        queue.begin_manual_navigation(ManualNavigationIntent::next(RepeatMode::StopAtEnd)),
        item_ids[1],
    );
    let initial_intent = WebMediaSourceIntent::native_hls(
        source.clone(),
        web_media_core::WebMediaPresentationKind::Vod,
        initial.source_state,
    );
    assert_eq!(
        initial_intent.recovery(),
        web_media_core::WebMediaRecoveryStrategy::RefreshRootManifestAndRematch
    );
    let WebMediaSelectionSwitchResolution::Ready(switch_request) = initial_intent
        .selection_switch_request(
            WebMediaSelectionSwitchIntent::ComponentSemantic(alternate_selection),
            settings.clone(),
        )
    else {
        panic!("fresh component action обязан запустить same-item switch");
    };
    let (switch_source, switch_selection, switch_settings) = native_request_parts(switch_request);
    let mut switched = prepare_native(
        &switch_source,
        Some(&switch_selection),
        &switch_settings,
        HlsVodStartIntent::RestoreOrBeginning(MediaTime::from_duration(FORWARD_SEEK_POSITION)),
    );
    assert!(matches!(
        switched.vod_initial_position(),
        Some(PreparedInitialPosition::PositionedAt { .. })
    ));
    assert_decoder_render_audio(switched.demuxer.as_mut(), &mut wgpu_harness);
    commit_navigation_after_consumer(&mut queue, next_preview, item_ids[1]);
    assert_eq!(
        queue.automatic_navigation(AutomaticEndedIntent::new(RepeatMode::StopAtEnd)),
        AutomaticNavigationOutcome::Stop(AutomaticStopReason::EndOfQueue {
            current_item_id: item_ids[1],
        })
    );

    let web_media_core::WebMediaSelectionShape::Components(switched_components) =
        switched.source_state.neutral_selection().shape()
    else {
        panic!("switched selection должна сохранить component shape");
    };
    assert_eq!(
        switched_components.semantic_rematch_request(),
        expected_ts_selection
    );
    let previous_preview = expect_manual_target(
        queue.begin_manual_navigation(ManualNavigationIntent::previous(RepeatMode::StopAtEnd)),
        item_ids[0],
    );

    let PreparedNativeHlsMedia {
        demuxer,
        seek_port,
        source_state,
        lifecycle,
    } = switched;
    let switched_intent = WebMediaSourceIntent::native_hls(
        switch_source.clone(),
        web_media_core::WebMediaPresentationKind::Vod,
        source_state,
    );
    let reopen_request = switched_intent
        .controlled_reopen_request(
            switch_settings.network_config.clone(),
            switch_settings.demux_config,
            Some(switch_settings.clone()),
        )
        .expect("native controlled reopen требует semantic rematch");
    drop(demuxer);
    drop(seek_port);
    drop(lifecycle);

    let (reopen_source, reopen_selection, reopen_settings) = native_request_parts(reopen_request);
    let mut reopened = prepare_native(
        &reopen_source,
        Some(&reopen_selection),
        &reopen_settings,
        HlsVodStartIntent::RestoreOrBeginning(MediaTime::from_duration(FORWARD_SEEK_POSITION)),
    );
    assert_eq!(server.request_count("/master.m3u8"), 3);
    assert!(matches!(
        reopened.vod_initial_position(),
        Some(PreparedInitialPosition::PositionedAt { .. })
    ));
    assert!(matches!(
        reopened
            .source_state
            .stream_configuration()
            .resolve_component_variant_action(old_component_action),
        Err(ComponentVariantActionError::StaleParentGeneration { .. })
            | Err(ComponentVariantActionError::StaleCatalogGeneration { .. })
    ));
    assert_decoder_render_audio(reopened.demuxer.as_mut(), &mut wgpu_harness);
    commit_navigation_after_consumer(&mut queue, previous_preview, item_ids[0]);
    let web_media_core::WebMediaSelectionShape::Components(reopened_components) =
        reopened.source_state.neutral_selection().shape()
    else {
        panic!("reopened selection должна сохранить component shape");
    };
    assert_eq!(
        reopened_components.semantic_rematch_request(),
        expected_ts_selection
    );
    assert_eq!(
        reopened
            .source_state
            .neutral_selection()
            .parent()
            .exact()
            .source(),
        stable_source_identity
    );
    assert_eq!(
        process_spy.invocation_count(),
        0,
        "seek/switch/queue/restart/restore/stale transitions не запускают extractor"
    );
}
