//! N14B cross-source regression: playlist traversal не оставляет состояние прошлого ingress-а.

use std::sync::Arc;

use codec_core::VideoCodec as DecodeVideoCodec;
use playlist_core::{
    AddItemsOutcome, CachedPlaylistMetadata, ManualNavigationIntent, ManualNavigationOutcome,
    ManualNavigationPreview, PlaylistItemDraft, PlaylistMediaKind, PlaylistQueue, RepeatMode,
    SecretUrlLocator, TraversalCurrentMutationOutcome,
};
use web_media_hls::HlsVodStartIntent;

use super::native_dash_vertical::{
    fixture_routes as dash_fixture_routes, native_settings as dash_settings,
    prepare_native as prepare_dash,
};
use super::native_hls_vertical::{
    ControlledHlsServer, assert_decoder_render_audio_for_codec,
    fixture_routes as hls_fixture_routes, native_settings as hls_settings,
    prepare_native as prepare_hls,
};
use super::native_smooth_vertical::{
    fixture_routes as smooth_fixture_routes, native_settings as smooth_settings,
    prepare_native as prepare_smooth, wait_for_tracks_changed as wait_for_smooth_tracks,
};
use crate::media_open::{NativeDashUrl, NativeHlsUrl, NativeSmoothUrl, SafeMediaLabel};
use crate::web_media_open::content_probe_tests::direct_progressive::ZeroProcessSpy;
use crate::web_media_open::content_probe_tests::direct_progressive_webm::OffscreenWgpuHarness;

/// Создаёт durable queue из разных reopenable roots без временных media endpoint-ов.
fn cross_source_queue(
    source_rows: &[(&str, &str)],
) -> (PlaylistQueue, Vec<playlist_core::PlaylistItemId>) {
    let mut queue = PlaylistQueue::new();
    let drafts = source_rows
        .iter()
        .map(|(label, root_url)| {
            PlaylistItemDraft::url(
                SecretUrlLocator::from_reopenable_url(*root_url)
                    .expect("каждый fixture root обязан быть reopenable"),
                CachedPlaylistMetadata::new(*label, PlaylistMediaKind::Video),
            )
        })
        .collect::<Vec<_>>();
    let AddItemsOutcome::Added(item_ids) = queue
        .append_batch(drafts)
        .expect("append cross-source playlist rows")
    else {
        panic!("непустой cross-source playlist не может дать empty append");
    };
    let item_ids = item_ids.into_vec();
    assert!(matches!(
        queue
            .set_traversal_current(item_ids[0])
            .expect("set initial cross-source current"),
        TraversalCurrentMutationOutcome::Set(_)
    ));
    (queue, item_ids)
}

/// Получает exact manual preview; current до consumer success ещё не меняется.
fn expect_navigation_target(
    outcome: ManualNavigationOutcome,
    expected_item_id: playlist_core::PlaylistItemId,
) -> ManualNavigationPreview {
    let ManualNavigationOutcome::OpenItem { item_id, preview } = outcome else {
        panic!("cross-source navigation должна вернуть concrete queue target");
    };
    assert_eq!(item_id, expected_item_id);
    preview
}

/// Завершает queue transition только после доказанного render/PCM нового source-а.
fn commit_after_consumer(
    queue: &mut PlaylistQueue,
    preview: ManualNavigationPreview,
    expected_item_id: playlist_core::PlaylistItemId,
) {
    let token = queue
        .prepare_manual_navigation(preview)
        .expect("consumer-successful cross-source navigation должна пройти preflight");
    assert_eq!(token.target_item_id(), expected_item_id);
    let commit = queue.commit_manual_navigation(token);
    assert_eq!(commit.traversal_current().item_id(), expected_item_id);
}

/// Доказывает HLS -> DASH -> Smooth -> DASH через одну очередь и один renderer consumer.
#[cfg(feature = "ffmpeg")]
#[test]
fn n14b_cross_source_playlist_reaches_consumers_before_each_queue_commit() {
    let hls_server = ControlledHlsServer::start(hls_fixture_routes());
    let dash_server = ControlledHlsServer::start(dash_fixture_routes());
    let smooth_server = ControlledHlsServer::start(smooth_fixture_routes());
    let hls_root = hls_server.target("/master.m3u8");
    let dash_root = dash_server.target("/manifest.mpd");
    let smooth_root = smooth_server.target("/vod/Manifest");
    let source_rows = [
        ("native HLS", hls_root.expose_secret_for_request()),
        ("native DASH", dash_root.expose_secret_for_request()),
        ("native Smooth", smooth_root.expose_secret_for_request()),
    ];
    let (mut queue, item_ids) = cross_source_queue(&source_rows);
    let process_spy = Arc::new(ZeroProcessSpy::default());
    let mut hls_open_settings = hls_settings();
    let mut dash_open_settings = dash_settings();
    let mut smooth_open_settings = smooth_settings();
    process_spy.install_as_attempt_owner(&mut hls_open_settings);
    process_spy.install_as_attempt_owner(&mut dash_open_settings);
    process_spy.install_as_attempt_owner(&mut smooth_open_settings);
    let mut renderer = OffscreenWgpuHarness::new();

    let hls_source = NativeHlsUrl::new(
        hls_root,
        SafeMediaLabel::from_service_safe_label("cross-source HLS"),
    );
    let mut active_hls = prepare_hls(
        &hls_source,
        None,
        &hls_open_settings,
        HlsVodStartIntent::Beginning,
    );
    assert_decoder_render_audio_for_codec(
        active_hls.demuxer.as_mut(),
        &mut renderer,
        DecodeVideoCodec::H264,
    );

    let dash_preview = expect_navigation_target(
        queue.begin_manual_navigation(ManualNavigationIntent::next(RepeatMode::StopAtEnd)),
        item_ids[1],
    );
    let dash_source = NativeDashUrl::new(
        dash_root.clone(),
        SafeMediaLabel::from_service_safe_label("cross-source DASH"),
    );
    let mut active_dash = prepare_dash(&dash_source, None, &dash_open_settings);
    assert_decoder_render_audio_for_codec(
        active_dash.demuxer.as_mut(),
        &mut renderer,
        DecodeVideoCodec::H264,
    );
    commit_after_consumer(&mut queue, dash_preview, item_ids[1]);
    drop(active_hls);

    let smooth_preview = expect_navigation_target(
        queue.begin_manual_navigation(ManualNavigationIntent::next(RepeatMode::StopAtEnd)),
        item_ids[2],
    );
    let smooth_source = NativeSmoothUrl::new(
        smooth_root,
        SafeMediaLabel::from_service_safe_label("cross-source Smooth"),
    );
    let mut active_smooth = prepare_smooth(&smooth_source, None, &smooth_open_settings);
    wait_for_smooth_tracks(active_smooth.demuxer.as_mut());
    assert_decoder_render_audio_for_codec(
        active_smooth.demuxer.as_mut(),
        &mut renderer,
        DecodeVideoCodec::H264,
    );
    commit_after_consumer(&mut queue, smooth_preview, item_ids[2]);
    drop(active_dash);

    let dash_return_preview = expect_navigation_target(
        queue.begin_manual_navigation(ManualNavigationIntent::previous(RepeatMode::StopAtEnd)),
        item_ids[1],
    );
    let mut reopened_dash = prepare_dash(&dash_source, None, &dash_open_settings);
    assert_decoder_render_audio_for_codec(
        reopened_dash.demuxer.as_mut(),
        &mut renderer,
        DecodeVideoCodec::H264,
    );
    commit_after_consumer(&mut queue, dash_return_preview, item_ids[1]);
    drop(active_smooth);

    assert_eq!(hls_server.request_count("/master.m3u8"), 1);
    assert_eq!(dash_server.request_count("/manifest.mpd"), 2);
    assert_eq!(smooth_server.request_count("/vod/Manifest"), 1);
    assert_eq!(
        process_spy.invocation_count(),
        0,
        "cross-source playlist transitions не имеют права запускать extractor"
    );
}
