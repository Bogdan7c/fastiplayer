//! Focused S21T contract tests с active/absent fake provider-ом.

use std::sync::{Arc, Mutex};

use source_core::{
    CancellationToken, HttpCookieSeed, HttpHeader, HttpPathScope, HttpRequestTarget, HttpScheme,
    SourceError, SourceResult, StreamingByteSource, ValidatedHttpHeaders,
};
use web_media_core::{
    CandidateFormatIdentity, CandidateIdentity, ExtractionGeneration, SemanticIdentity,
    SourceIdentity,
};

use super::*;

/// Safe observation fake provider-а без secret payload.
#[derive(Debug, Default)]
struct FakeObservation {
    /// Число open calls.
    opens: usize,
    /// Число refresh calls.
    refreshes: usize,
}

/// Active provider, возвращающий forward-only in-memory stream.
struct FakeProvider {
    /// Immutable capability descriptor.
    descriptor: ProviderDescriptor,
    /// Safe call counters.
    observation: Arc<Mutex<FakeObservation>>,
}

/// In-memory forward-only source, соблюдающий cancellation-aware boundary.
struct FakeStreamingSource {
    /// Read cursor тестового payload.
    cursor: std::io::Cursor<Vec<u8>>,
}

impl FakeStreamingSource {
    /// Создаёт deterministic byte payload.
    fn new(bytes: Vec<u8>) -> Self {
        Self {
            cursor: std::io::Cursor::new(bytes),
        }
    }
}

impl StreamingByteSource for FakeStreamingSource {
    /// Проверяет cancellation до чтения, как обязан concrete provider.
    fn read(&mut self, output: &mut [u8], cancellation: &CancellationToken) -> SourceResult<usize> {
        if cancellation.is_cancelled() {
            return Err(SourceError::Cancelled);
        }
        std::io::Read::read(&mut self.cursor, output).map_err(|source| SourceError::LocalIo {
            context: "fake streaming source read",
            source,
        })
    }
}

impl FakeProvider {
    /// Создаёт HTTP(S) fake с refresh support.
    fn new(provider_id: TransportProviderId, observation: Arc<Mutex<FakeObservation>>) -> Self {
        let descriptor = ProviderDescriptor::new(
            provider_id,
            vec![
                TransportScheme::Http(HttpScheme::Http),
                TransportScheme::Http(HttpScheme::Https),
            ],
            RefreshSupport::Supported,
        )
        .expect("valid fake provider descriptor");
        Self {
            descriptor,
            observation,
        }
    }

    /// Строит deterministic successful output без redirect-а.
    fn output(request: &TransportOpenRequest) -> ProviderOpenOutput {
        ProviderOpenOutput::new(
            request.target().clone(),
            RedirectHopCount::none(),
            request.presentation(),
            TransportInput::streaming(FakeStreamingSource::new(vec![1_u8, 2, 3])),
        )
    }
}

impl TransportProvider for FakeProvider {
    /// Возвращает immutable descriptor.
    fn descriptor(&self) -> &ProviderDescriptor {
        &self.descriptor
    }

    /// Фиксирует open без чтения raw secret payload.
    fn open(
        &self,
        request: &TransportOpenRequest,
    ) -> Result<ProviderOpenOutput, ProviderOpenError> {
        self.observation
            .lock()
            .expect("fake observation mutex is healthy")
            .opens += 1;
        Ok(Self::output(request))
    }

    /// Фиксирует refresh и возвращает replacement target.
    fn refresh(
        &self,
        request: &TransportRefreshRequest,
    ) -> Result<ProviderOpenOutput, ProviderRefreshError> {
        self.observation
            .lock()
            .expect("fake observation mutex is healthy")
            .refreshes += 1;
        Ok(Self::output(request.replacement()))
    }
}

/// Строит exact+semantic component identity одной source lineage.
fn component_identity(extraction_generation: u64, format_identity: &str) -> MediaComponentIdentity {
    let source = SourceIdentity::new(41);
    let exact = CandidateIdentity::new(
        source,
        ExtractionGeneration::new(extraction_generation),
        CandidateFormatIdentity::new(format_identity).expect("bounded format identity"),
    );
    let semantic =
        SemanticIdentity::new(source, "stable-video-main").expect("bounded semantic identity");
    MediaComponentIdentity::new(exact, semantic, MediaComponentRole::Muxed)
        .expect("same source lineage")
}

/// Строит пустой scoped context, пригодный для public media.
fn empty_secret_context(target: &HttpRequestTarget) -> SecretRequestContext {
    let path_scope = HttpPathScope::new("/").expect("root path scope");
    SecretRequestContext::builder(SecretRequestScope::from_target(target, path_scope)).build()
}

/// Строит полный typed open request.
fn open_request(
    provider_id: TransportProviderId,
    component: MediaComponentIdentity,
    source_generation: SourceGeneration,
    cancellation: CancellationToken,
) -> TransportOpenRequest {
    let target = HttpRequestTarget::parse_exact("https://media.example.test/video/main.webm")
        .expect("valid media target");
    let secrets = empty_secret_context(&target);
    TransportOpenRequest::new(
        provider_id,
        component,
        target,
        MediaPresentation::Vod,
        source_generation,
        secrets,
        RedirectPolicy::same_origin(RedirectHopLimit::new(4).expect("valid redirect hop limit")),
        cancellation,
    )
    .expect("valid open request")
}

/// Source-specific Range policy отсутствует по умолчанию и добавляется named intent-method-ом.
#[test]
fn open_request_carries_typed_http_range_request_limit() {
    let request_without_limit = open_request(
        TransportProviderId::new("range-policy").expect("valid provider ID"),
        component_identity(1, "range-policy"),
        SourceGeneration::new(1),
        CancellationToken::new(),
    );
    assert_eq!(request_without_limit.http_range_request_limit(), None);

    let limit = HttpRangeRequestLimit::new(10 * 1024 * 1024).expect("positive range limit");
    let request_with_limit = request_without_limit.with_http_range_request_limit(limit);
    assert_eq!(request_with_limit.http_range_request_limit(), Some(limit));
    assert_eq!(
        request_with_limit
            .http_range_request_limit()
            .expect("range limit")
            .maximum_bytes(),
        10 * 1024 * 1024
    );
    assert_eq!(
        HttpRangeRequestLimit::new(0),
        Err(HttpRangeRequestLimitError::Zero)
    );
}

/// Empty registry возвращает typed unavailable, active fake открывает exact component.
#[test]
fn absent_and_active_fake_provider_are_distinct_outcomes() {
    let provider_id = TransportProviderId::new("fake-http").expect("valid provider id");
    let request = open_request(
        provider_id.clone(),
        component_identity(1, "muxed-1"),
        SourceGeneration::new(10),
        CancellationToken::new(),
    );
    let registry = TransportRegistry::new();
    let error = registry
        .open(request.clone())
        .expect_err("provider is absent");
    assert_eq!(
        error,
        TransportOpenError::ProviderUnavailable {
            provider_id: provider_id.clone(),
        }
    );

    let observation = Arc::new(Mutex::new(FakeObservation::default()));
    let mut registry = TransportRegistry::new();
    registry
        .register(Box::new(FakeProvider::new(
            provider_id,
            Arc::clone(&observation),
        )))
        .expect("fake provider registration succeeds");
    let opened = registry.open(request).expect("active fake opens request");

    assert_eq!(opened.presentation(), MediaPresentation::Vod);
    assert_eq!(opened.seekability(), TransportSeekability::Streaming);
    assert_eq!(
        opened.identity().source_generation(),
        SourceGeneration::new(10)
    );
    assert_eq!(
        observation
            .lock()
            .expect("fake observation mutex is healthy")
            .opens,
        1
    );

    let mut stream = opened
        .into_input()
        .into_streaming()
        .expect("fake result is a streaming source");
    let read_cancellation = CancellationToken::new();
    read_cancellation.cancel();
    let mut output = [0_u8; 4];
    assert!(matches!(
        stream.read(&mut output, &read_cancellation),
        Err(SourceError::Cancelled)
    ));
}

/// Context выдаёт secrets только same-origin + path subtree + secure target-у.
#[test]
fn secret_context_enforces_origin_path_secure_and_purpose_scope() {
    let initial = HttpRequestTarget::parse_exact(
        "https://user:locator-secret@media.example.test/private/video/master.m3u8?token=url",
    )
    .expect("valid initial target");
    let headers = ValidatedHttpHeaders::new(vec![HttpHeader::new(
        "Authorization",
        "Bearer header-secret",
    )])
    .expect("valid auth header");
    let cookie_seed = HttpCookieSeed::builder("scoped", "seed-secret")
        .expect("valid scoped cookie pair")
        .for_domain("media.example.test")
        .expect("valid scoped cookie domain")
        .with_path("/private/video")
        .expect("valid scoped cookie path")
        .secure_only()
        .build()
        .expect("complete scoped cookie seed");
    let context = SecretRequestContext::builder(SecretRequestScope::from_target(
        &initial,
        HttpPathScope::new("/private/video").expect("valid path scope"),
    ))
    .with_headers(headers)
    .with_serialized_cookies("session=cookie-secret")
    .expect("valid serialized cookies")
    .with_scoped_cookie_seeds([cookie_seed])
    .with_request_data(b"request-data-secret".to_vec())
    .with_segment_query_override(
        SecretQueryOverride::new("segment_token=segment-secret").expect("valid segment query"),
    )
    .with_key_query_override(
        SecretQueryOverride::new("key_token=key-secret").expect("valid key query"),
    )
    .build();

    let same_scope =
        HttpRequestTarget::parse_exact("https://media.example.test/private/video/segment-1.ts")
            .expect("valid same-scope target");
    let segment_material = context
        .material_for(&same_scope, SecretRequestPurpose::MediaSegment)
        .expect("same secure scope receives material");
    assert_eq!(segment_material.headers_for_request().len(), 1);
    assert_eq!(
        segment_material.cookies_for_request(),
        Some(b"session=cookie-secret".as_slice())
    );
    assert_eq!(
        segment_material.cookie_seeds_for_request().len(),
        1,
        "готовый Cookie header и scoped seeds обязаны оставаться разными intents"
    );
    assert!(segment_material.request_data_for_request().is_none());
    assert_eq!(
        segment_material
            .query_override_for_request()
            .expect("segment override exists")
            .expose_secret_for_request(),
        "segment_token=segment-secret"
    );

    let primary_material = context
        .material_for(&initial, SecretRequestPurpose::PrimaryResource)
        .expect("initial target receives primary material");
    assert_eq!(
        primary_material.request_data_for_request(),
        Some(b"request-data-secret".as_slice())
    );
    assert!(primary_material.query_override_for_request().is_none());

    let sibling_prefix =
        HttpRequestTarget::parse_exact("https://media.example.test/private/video-copy/segment.ts")
            .expect("valid sibling target");
    assert!(
        context
            .material_for(&sibling_prefix, SecretRequestPurpose::MediaSegment)
            .is_none()
    );
    let insecure =
        HttpRequestTarget::parse_exact("http://media.example.test/private/video/segment.ts")
            .expect("valid HTTP target");
    assert!(
        context
            .material_for(&insecure, SecretRequestPurpose::MediaSegment)
            .is_none()
    );
}

/// Cross-host redirect может быть разрешён, но не получает scoped secrets.
#[test]
fn cross_host_redirect_never_forwards_secret_context() {
    let original = HttpRequestTarget::parse_exact(
        "https://origin.example.test/media/master.m3u8?token=locator-secret",
    )
    .expect("valid original target");
    let redirected = HttpRequestTarget::parse_exact("https://cdn.example.test/media/segment.ts")
        .expect("valid redirected target");
    let context = SecretRequestContext::builder(SecretRequestScope::from_target(
        &original,
        HttpPathScope::new("/media").expect("valid media scope"),
    ))
    .with_serialized_cookies("session=cross-host-secret")
    .expect("valid cookies")
    .build();
    let policy = RedirectPolicy::cross_origin_without_secrets(
        RedirectHopLimit::new(3).expect("valid CDN redirect hop limit"),
    );

    let authorization = policy
        .authorize_redirect(&original, &redirected, RedirectHopCount::none())
        .expect("cross-origin redirect is allowed without secrets");
    assert!(!authorization.permits_secret_scope_check());
    assert!(
        context
            .material_for(&redirected, SecretRequestPurpose::MediaSegment)
            .is_none()
    );
}

/// Cancellation short-circuits provider call, stale refresh не выполняется.
#[test]
fn cancellation_and_stale_refresh_are_generation_fenced() {
    let provider_id = TransportProviderId::new("fake-http").expect("valid provider id");
    let observation = Arc::new(Mutex::new(FakeObservation::default()));
    let mut registry = TransportRegistry::new();
    registry
        .register(Box::new(FakeProvider::new(
            provider_id.clone(),
            Arc::clone(&observation),
        )))
        .expect("fake provider registration succeeds");

    let cancelled = CancellationToken::new();
    cancelled.cancel();
    let cancelled_request = open_request(
        provider_id.clone(),
        component_identity(1, "muxed-old"),
        SourceGeneration::new(1),
        cancelled,
    );
    assert_eq!(
        registry
            .open(cancelled_request)
            .expect_err("request is cancelled"),
        TransportOpenError::Cancelled
    );
    assert_eq!(
        observation
            .lock()
            .expect("fake observation mutex is healthy")
            .opens,
        0
    );

    let previous = OpenedComponentIdentity::new(
        provider_id.clone(),
        component_identity(1, "muxed-old"),
        SourceGeneration::new(7),
    );
    let replacement = open_request(
        provider_id,
        component_identity(2, "muxed-new"),
        SourceGeneration::new(8),
        CancellationToken::new(),
    );
    let refresh = TransportRefreshRequest::new(previous, replacement)
        .expect("semantic identity and newer generation match");
    let stale_error = registry
        .refresh_if_current(refresh, SourceGeneration::new(9))
        .expect_err("current generation already changed");
    assert_eq!(
        stale_error,
        TransportRefreshError::StaleSourceGeneration {
            requested: SourceGeneration::new(7),
            current: SourceGeneration::new(9),
        }
    );
    assert_eq!(
        observation
            .lock()
            .expect("fake observation mutex is healthy")
            .refreshes,
        0
    );

    let previous = OpenedComponentIdentity::new(
        TransportProviderId::new("fake-http").expect("valid provider id"),
        component_identity(2, "muxed-new"),
        SourceGeneration::new(9),
    );
    let replacement = open_request(
        TransportProviderId::new("fake-http").expect("valid provider id"),
        component_identity(3, "muxed-latest"),
        SourceGeneration::new(10),
        CancellationToken::new(),
    );
    let refresh = TransportRefreshRequest::new(previous, replacement)
        .expect("active refresh contract is valid");
    let refreshed = registry
        .refresh_if_current(refresh, SourceGeneration::new(9))
        .expect("current refresh reaches active provider");
    assert_eq!(refreshed.replaced_generation(), SourceGeneration::new(9));
    assert_eq!(
        refreshed.opened().identity().source_generation(),
        SourceGeneration::new(10)
    );
    assert_eq!(
        observation
            .lock()
            .expect("fake observation mutex is healthy")
            .refreshes,
        1
    );
}

/// Debug/errors не содержат locator/header/cookie/body/query secrets.
#[test]
fn debug_and_errors_do_not_expose_request_secrets() {
    let target = HttpRequestTarget::parse_exact(
        "https://user:locator-secret@media.example.test/private/master.m3u8?token=url-secret",
    )
    .expect("valid target");
    let context = SecretRequestContext::builder(SecretRequestScope::from_target(
        &target,
        HttpPathScope::new("/private").expect("valid private scope"),
    ))
    .with_headers(
        ValidatedHttpHeaders::new(vec![HttpHeader::new(
            "Authorization",
            "Bearer header-secret",
        )])
        .expect("valid secret header"),
    )
    .with_serialized_cookies("session=cookie-secret")
    .expect("valid cookies")
    .with_scoped_cookie_seeds([HttpCookieSeed::builder("scoped", "seed-secret")
        .expect("valid scoped cookie pair")
        .for_domain("media.example.test")
        .expect("valid scoped cookie domain")
        .with_path("/private")
        .expect("valid scoped cookie path")
        .secure_only()
        .build()
        .expect("complete scoped cookie seed")])
    .with_request_data(b"body-secret".to_vec())
    .with_segment_query_override(
        SecretQueryOverride::new("signature=query-secret").expect("valid query override"),
    )
    .build();
    let request = TransportOpenRequest::new(
        TransportProviderId::new("missing-provider").expect("valid provider id"),
        component_identity(1, "format-secret"),
        target,
        MediaPresentation::Vod,
        SourceGeneration::new(1),
        context,
        RedirectPolicy::same_origin(RedirectHopLimit::new(2).expect("valid redirect hop limit")),
        CancellationToken::new(),
    )
    .expect("valid secret request");
    let debug = format!("{request:?}");
    for secret in [
        "locator-secret",
        "private",
        "url-secret",
        "header-secret",
        "cookie-secret",
        "seed-secret",
        "body-secret",
        "query-secret",
        "format-secret",
    ] {
        assert!(!debug.contains(secret), "Debug leaked secret marker");
    }

    let error = TransportRegistry::new()
        .open(request)
        .expect_err("provider is intentionally absent");
    let formatted_error = format!("{error:?} {error}");
    for secret in [
        "locator-secret",
        "private",
        "url-secret",
        "header-secret",
        "cookie-secret",
        "seed-secret",
        "body-secret",
        "query-secret",
        "format-secret",
    ] {
        assert!(
            !formatted_error.contains(secret),
            "error formatting leaked secret marker"
        );
    }
}
