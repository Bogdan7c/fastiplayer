//! Focused proofs app-owned static DASH composition boundary.

use std::num::NonZeroU8;
use std::time::Duration;

use dash_mpd_core::{DashContainer, DashMediaKind};
use fastiplayer_config::NetworkConfig;
use service_ytdlp::{YtDlpDashFragmentLocatorKind, YtDlpDashFragmentRole, YtDlpLiveIntent};
use source_core::{
    CancellationToken, HttpPathScope, HttpRequestTarget, SourceRuntimeConfig, ValidatedHttpHeaders,
};
use web_media_adaptive::{
    AdaptiveHttpContext, AdaptiveResourceQueryApplication, AdaptiveRetryPolicy,
};
use web_media_core::{
    CandidateFormatIdentity, CandidateIdentity, ContainerFamily, ExtractionGeneration,
    SemanticIdentity, SourceIdentity,
};
use web_media_dash::{DashSerializedFragmentKind, DashVodHttpContext};
use web_media_transport_api::{
    MediaComponentIdentity, MediaComponentRole, MediaPresentation, RedirectHopLimit,
    RedirectPolicy, SecretRequestContext, SecretRequestScope, TransportOpenRequest,
    TransportProviderId,
};

use super::{
    SerializedDashFragmentView, ensure_static_dash_intent, serialized_component_from_fragments,
    serialized_separate_http_context,
};

/// Минимальный validated service fragment для чистой проверки app mapper-а.
#[derive(Clone, Copy)]
struct PinnedSerializedFragment {
    /// Явная роль fragment-а.
    role: YtDlpDashFragmentRole,
    /// Absolute либо relative locator vocabulary.
    locator_kind: YtDlpDashFragmentLocatorKind,
    /// Secret-safe locator, переданный transport boundary.
    locator: &'static str,
    /// Base только для relative locator-а.
    base: Option<&'static str>,
    /// Optional finite media duration.
    duration_seconds: Option<f64>,
}

impl SerializedDashFragmentView for PinnedSerializedFragment {
    fn role(&self) -> YtDlpDashFragmentRole {
        self.role
    }

    fn locator_kind(&self) -> YtDlpDashFragmentLocatorKind {
        self.locator_kind
    }

    fn locator_for_transport(&self) -> &str {
        self.locator
    }

    fn base_url_for_relative_resolution(&self) -> Option<&str> {
        self.base
    }

    fn duration_seconds(&self) -> Option<f64> {
        self.duration_seconds
    }
}

/// Собирает независимый HTTP context без выполнения сетевого запроса.
fn adaptive_context(
    target: &str,
    cancellation: CancellationToken,
    source: SourceIdentity,
) -> AdaptiveHttpContext {
    let target = HttpRequestTarget::parse_exact(target).expect("valid test target");
    let generation = crate::web_media_adaptive_config::initial_adaptive_source_generation();
    let exact = CandidateIdentity::new(
        source,
        ExtractionGeneration::new(generation.value()),
        CandidateFormatIdentity::new("dash-app-mapper-test").expect("format identity"),
    );
    let semantic =
        SemanticIdentity::new(source, "dash-app-mapper-test").expect("semantic identity");
    let component = MediaComponentIdentity::new(exact, semantic, MediaComponentRole::Muxed)
        .expect("component identity");
    let scope =
        SecretRequestScope::from_target(&target, HttpPathScope::new("/").expect("root path scope"));
    let secrets = SecretRequestContext::builder(scope)
        .with_headers(ValidatedHttpHeaders::new(Vec::new()).expect("empty headers"))
        .build();
    let request = TransportOpenRequest::new(
        TransportProviderId::new("dash-app-mapper-test").expect("provider id"),
        component,
        target,
        MediaPresentation::Vod,
        generation,
        secrets,
        RedirectPolicy::same_origin(RedirectHopLimit::new(4).expect("non-zero redirect hop limit")),
        cancellation,
    )
    .expect("transport request");
    let source_config =
        SourceRuntimeConfig::from_network_config(&NetworkConfig::default()).expect("source config");

    AdaptiveHttpContext::new(
        request,
        &source_config,
        crate::web_media_adaptive_config::adaptive_transport_limits(&NetworkConfig::default())
            .expect("adaptive limits"),
        AdaptiveRetryPolicy::new(
            NonZeroU8::new(1).expect("attempt count"),
            Duration::from_millis(1),
            Duration::from_millis(1),
            Duration::from_millis(1),
        )
        .expect("retry policy"),
    )
    .expect("adaptive context")
}

#[test]
fn maps_pinned_fmp4_fragments_with_exact_roles_container_and_query_scope() {
    let component = serialized_component_from_fragments(
        ContainerFamily::FragmentedIsoBmff,
        MediaComponentRole::Muxed,
        [
            PinnedSerializedFragment {
                role: YtDlpDashFragmentRole::Initialization,
                locator_kind: YtDlpDashFragmentLocatorKind::AbsoluteUrl,
                locator: "https://media.example.test/init.mp4",
                base: None,
                duration_seconds: None,
            },
            PinnedSerializedFragment {
                role: YtDlpDashFragmentRole::Media,
                locator_kind: YtDlpDashFragmentLocatorKind::RelativePath,
                locator: "segment-1.m4s",
                base: Some("https://media.example.test/video/"),
                duration_seconds: Some(2.0),
            },
        ],
    )
    .expect("pinned fMP4 material");

    assert_eq!(component.container, DashContainer::IsoBmff);
    assert_eq!(component.media_kind, DashMediaKind::Muxed);
    assert_eq!(
        component.query_application,
        AdaptiveResourceQueryApplication::MergeScopedAddition
    );
    assert_eq!(component.fragments.len(), 2);
    assert_eq!(
        component.fragments[0].kind,
        DashSerializedFragmentKind::Initialization
    );
    assert_eq!(component.fragments[0].duration, None);
    assert_eq!(
        component.fragments[1].kind,
        DashSerializedFragmentKind::Media
    );
    assert_eq!(
        component.fragments[1].duration,
        Some(Duration::from_secs(2))
    );
}

#[test]
fn maps_pinned_webm_fragments_with_exact_roles_container_and_query_scope() {
    let component = serialized_component_from_fragments(
        ContainerFamily::WebM,
        MediaComponentRole::Audio,
        [
            PinnedSerializedFragment {
                role: YtDlpDashFragmentRole::Initialization,
                locator_kind: YtDlpDashFragmentLocatorKind::RelativePath,
                locator: "init.webm",
                base: Some("https://media.example.test/audio/"),
                duration_seconds: None,
            },
            PinnedSerializedFragment {
                role: YtDlpDashFragmentRole::Media,
                locator_kind: YtDlpDashFragmentLocatorKind::AbsoluteUrl,
                locator: "https://cdn.example.test/audio-1.webm",
                base: None,
                duration_seconds: Some(3.5),
            },
        ],
    )
    .expect("pinned WebM material");

    assert_eq!(component.container, DashContainer::WebM);
    assert_eq!(component.media_kind, DashMediaKind::Audio);
    assert_eq!(
        component.query_application,
        AdaptiveResourceQueryApplication::MergeScopedAddition
    );
    assert_eq!(component.fragments.len(), 2);
    assert_eq!(
        component.fragments[0].kind,
        DashSerializedFragmentKind::Initialization
    );
    assert_eq!(
        component.fragments[1].kind,
        DashSerializedFragmentKind::Media
    );
    assert_eq!(
        component.fragments[1].duration,
        Some(Duration::from_secs_f64(3.5))
    );
}

#[test]
fn rejects_dynamic_intents_at_the_pre_network_gate() {
    assert!(ensure_static_dash_intent(YtDlpLiveIntent::Unspecified).is_ok());
    assert!(ensure_static_dash_intent(YtDlpLiveIntent::NotLive).is_ok());

    for dynamic_intent in [
        YtDlpLiveIntent::Live,
        YtDlpLiveIntent::Upcoming,
        YtDlpLiveIntent::PostLive,
        YtDlpLiveIntent::Incompatible,
    ] {
        assert!(ensure_static_dash_intent(dynamic_intent).is_err());
    }
}

#[test]
fn keeps_separate_component_request_contexts_independent() {
    let video_cancellation = CancellationToken::new();
    let audio_cancellation = CancellationToken::new();
    let video = adaptive_context(
        "https://video.example.test/manifest.mpd",
        video_cancellation.clone(),
        SourceIdentity::new(11),
    );
    let audio = adaptive_context(
        "https://audio.example.test/manifest.mpd",
        audio_cancellation,
        SourceIdentity::new(12),
    );

    let DashVodHttpContext::SerializedSeparate { video, audio } =
        serialized_separate_http_context(&video, &audio)
    else {
        panic!("separate material must preserve separate HTTP contexts");
    };

    video_cancellation.cancel();

    assert!(video.cancellation().is_cancelled());
    assert!(!audio.cancellation().is_cancelled());
}

#[test]
fn invalid_fragment_mapping_stays_before_prepared_runtime_barrier() {
    let mapped_component = serialized_component_from_fragments(
        ContainerFamily::FragmentedIsoBmff,
        MediaComponentRole::Video,
        [PinnedSerializedFragment {
            role: YtDlpDashFragmentRole::Media,
            locator_kind: YtDlpDashFragmentLocatorKind::RelativePath,
            locator: "segment-without-base.m4s",
            base: None,
            duration_seconds: Some(2.0),
        }],
    );

    assert!(mapped_component.is_err());
}
