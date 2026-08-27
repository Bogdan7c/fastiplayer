//! Hermetic runtime evidence для already-fetched native HLS top manifest-а.

#[allow(dead_code)] // Shared fixture экспортирует больше builders, чем нужно этому focused binary.
mod support;

use std::num::NonZeroU32;

use source_core::CancellationToken;
use support::{TestQueries, TestServer, adaptive_context, demux_registry, open_policy, response};
use web_media_adaptive::{
    AdaptiveHttpContext, AdaptiveResourceFetchRequest, AdaptiveResourcePurpose,
    AdaptiveResourceQueryApplication,
};
use web_media_hls::{
    HlsAudioLayoutIntent, HlsComponentContainerIntent, HlsContainerEvidence, HlsFetchedTopManifest,
    HlsMainTrackLayoutIntent, HlsManifestInput, HlsRequestOverrides, HlsRequiredContainer,
    HlsVariantSelectionIntent, HlsVodOpenError, HlsVodOpenRequest, prepare_hls_vod,
};
use web_media_transport_api::SourceGeneration;

fn fetch_top_manifest(
    http: &AdaptiveHttpContext,
    generation: SourceGeneration,
    selected_url: &source_core::HttpRequestTarget,
) -> web_media_adaptive::AdaptiveFetchedResource {
    let request = AdaptiveResourceFetchRequest::full(
        generation,
        selected_url.clone(),
        http.maximum_resource_bytes(AdaptiveResourcePurpose::Manifest),
        AdaptiveResourcePurpose::Manifest,
        AdaptiveResourceQueryApplication::BypassScopedQuery,
    )
    .with_secret_forwarding(http.resource_secret_forwarding_for(selected_url));
    http.fetch_resource_blocking(request)
        .expect("bounded top manifest fetch")
}

#[test]
fn fetched_master_skips_top_request_and_uses_effective_redirect_base() {
    let master = b"#EXTM3U\n\
#EXT-X-STREAM-INF:BANDWIDTH=2500000,RESOLUTION=1280x720,CODECS=\"avc1.64001f,mp4a.40.2\"\n\
variant.m3u8\n";
    let variant = b"#EXTM3U\n\
#EXT-X-TARGETDURATION:4\n\
#EXTINF:4,\n\
segment.ts\n\
#EXT-X-ENDLIST\n";
    let server = TestServer::start(move |_, request| {
        if request
            .request_line
            .starts_with("GET /original/master.m3u8 ")
        {
            return response(
                "302 Found",
                &[("Location", "/redirected/master.m3u8".to_owned())],
                b"",
            );
        }
        if request
            .request_line
            .starts_with("GET /redirected/master.m3u8 ")
        {
            return response("200 OK", &[], master);
        }
        if request
            .request_line
            .starts_with("GET /redirected/variant.m3u8 ")
        {
            return response("200 OK", &[], variant);
        }
        panic!("unexpected HLS request: {}", request.request_line);
    });
    let selected_url = server.target("/original/master.m3u8");
    let generation = SourceGeneration::new(1);
    let http = adaptive_context(
        &selected_url,
        CancellationToken::new(),
        generation,
        TestQueries::default(),
    );
    let fetched_top = fetch_top_manifest(&http, generation, &selected_url);
    let fetched_top = HlsFetchedTopManifest::new(selected_url, fetched_top, &http);
    let request = HlsVodOpenRequest {
        http,
        generation,
        manifest: HlsManifestInput::FetchedTop(fetched_top),
        selection: HlsVariantSelectionIntent {
            resolution: Some((
                NonZeroU32::new(1280).expect("width"),
                NonZeroU32::new(720).expect("height"),
            )),
            codecs: Some("avc1.64001f,mp4a.40.2".into()),
            audio: HlsAudioLayoutIntent::Muxed,
            main_track_layout: HlsMainTrackLayoutIntent::MuxedAv,
        },
        overrides: HlsRequestOverrides::new(None),
        containers: HlsComponentContainerIntent {
            main: HlsContainerEvidence::Exact(HlsRequiredContainer::TransportStream),
            alternate_audio: None,
        },
        demux_registry: demux_registry(),
        policy: open_policy(),
    };

    let _opened = prepare_hls_vod(request).expect("fetched master opens without top GET");
    let requests = server.requests();
    assert_eq!(requests.len(), 3, "top manifest must not be fetched twice");
    assert!(
        requests[0]
            .request_line
            .starts_with("GET /original/master.m3u8 ")
    );
    assert!(
        requests[1]
            .request_line
            .starts_with("GET /redirected/master.m3u8 ")
    );
    assert!(
        requests[2]
            .request_line
            .starts_with("GET /redirected/variant.m3u8 ")
    );
}

#[test]
fn fetched_manifest_generation_mismatch_fails_before_network() {
    let playlist = b"#EXTM3U\n#EXT-X-TARGETDURATION:4\n#EXTINF:4,\nsegment.ts\n#EXT-X-ENDLIST\n";
    let server = TestServer::start(move |request_index, request| {
        assert_eq!(
            request_index, 0,
            "stale manifest must not trigger another GET"
        );
        assert!(
            request
                .request_line
                .starts_with("GET /original/master.m3u8 ")
        );
        response("200 OK", &[], playlist)
    });
    let selected_url = server.target("/original/master.m3u8");
    let generation = SourceGeneration::new(7);
    let fetched_generation = SourceGeneration::new(6);
    let fetched_http = adaptive_context(
        &selected_url,
        CancellationToken::new(),
        fetched_generation,
        TestQueries::default(),
    );
    let fetched_top = fetch_top_manifest(&fetched_http, fetched_generation, &selected_url);
    let fetched_top = HlsFetchedTopManifest::new(selected_url.clone(), fetched_top, &fetched_http);
    let request = HlsVodOpenRequest {
        http: adaptive_context(
            &selected_url,
            CancellationToken::new(),
            generation,
            TestQueries::default(),
        ),
        generation,
        manifest: HlsManifestInput::FetchedTop(fetched_top),
        selection: HlsVariantSelectionIntent {
            resolution: None,
            codecs: None,
            audio: HlsAudioLayoutIntent::Muxed,
            main_track_layout: HlsMainTrackLayoutIntent::MuxedAv,
        },
        overrides: HlsRequestOverrides::new(None),
        containers: HlsComponentContainerIntent {
            main: HlsContainerEvidence::Exact(HlsRequiredContainer::TransportStream),
            alternate_audio: None,
        },
        demux_registry: demux_registry(),
        policy: open_policy(),
    };

    let error = prepare_hls_vod(request).expect_err("stale provenance must fail closed");
    assert!(matches!(
        error,
        HlsVodOpenError::FetchedManifestGenerationMismatch
    ));
    assert_eq!(server.requests().len(), 1);
}
