//! Оркестрация единственного fetch, cancellable parse и атомарной публикации.

use std::sync::Arc;

use smooth_streaming_manifest_core::{
    SmoothManifest, SmoothManifestParseRequest, SmoothStreamKind, SmoothTime,
    parse_vod_client_manifest_cancellable,
};
use web_media_adaptive::{
    AdaptiveHttpContext, AdaptiveResourceFetchRequest, AdaptiveResourcePurpose,
    AdaptiveResourceQueryApplication, AdaptiveTransportError,
};
use web_media_core::{ComponentVariantCatalogIdentity, ExactSelectionIdentity};
use web_media_transport_api::{MediaComponentRole, MediaPresentation};

#[cfg(test)]
use crate::catalog::build_catalog;
use crate::catalog::{SmoothCatalogBuildRequest, build_provider_default_catalog};
use crate::error::{SmoothPrepareError, SmoothProfileError, SmoothTransportProfileError};
use crate::model::{SmoothAlignedSpan, SmoothPreparedCatalog, SmoothRuntimeSeed};
use crate::request::SmoothPrepareRequest;

/// Private manifest/resource owner, shared by fast default и sibling discovery.
pub(crate) struct SmoothManifestPreparation {
    pub(crate) http: AdaptiveHttpContext,
    pub(crate) effective_manifest_target: source_core::HttpRequestTarget,
    pub(crate) fragment_secret_forwarding: web_media_adaptive::AdaptiveResourceSecretForwarding,
    pub(crate) manifest: Arc<SmoothManifest>,
    pub(crate) catalog_identity: ComponentVariantCatalogIdentity,
    pub(crate) parent_semantic: web_media_core::SemanticIdentity,
    pub(crate) video_stream_ordinal: usize,
    pub(crate) audio_stream_ordinal: usize,
    pub(crate) aligned_span: SmoothAlignedSpan,
    pub(crate) preferred_height: web_media_core::PreferredHeightPolicy,
    pub(crate) policy: crate::SmoothPreparationPolicy,
}

/// Готовит быстрый provider-default seed: один Manifest fetch и ровно два init-а.
pub fn prepare_smooth_vod(
    request: SmoothPrepareRequest<'_>,
) -> Result<SmoothPreparedCatalog, SmoothPrepareError> {
    let prepared = prepare_manifest(request)?;
    let cancellation = prepared.http.cancellation().clone();
    let catalog_build = build_provider_default_catalog(SmoothCatalogBuildRequest {
        manifest: &prepared.manifest,
        catalog_identity: prepared.catalog_identity.clone(),
        parent_semantic: &prepared.parent_semantic,
        video_stream_ordinal: prepared.video_stream_ordinal,
        audio_stream_ordinal: prepared.audio_stream_ordinal,
        preferred_height: prepared.preferred_height,
        policy: &prepared.policy,
        cancellation: &|| cancellation.is_cancelled(),
    })?;
    Ok(into_prepared_catalog(prepared, catalog_build))
}

/// Выполняет общий bounded Manifest fetch/parse, не materializing quality rows.
pub(crate) fn prepare_manifest(
    mut request: SmoothPrepareRequest<'_>,
) -> Result<SmoothManifestPreparation, SmoothPrepareError> {
    validate_transport_profile(&request)?;
    let initial_target = request
        .transport
        .target()
        .as_http()
        .ok_or(SmoothPrepareError::Fetch(AdaptiveTransportError::Target(
            source_core::HttpRequestTargetError::UnsupportedScheme,
        )))?
        .clone();
    let parent_semantic = request.transport.component().semantic().clone();
    let parent_identity = ExactSelectionIdentity::new(
        request.transport.component().exact().clone(),
        parent_semantic.clone(),
    )
    .map_err(|_| {
        SmoothPrepareError::TransportProfile(SmoothTransportProfileError::InvalidComponentIdentity)
    })?;
    let catalog_identity =
        ComponentVariantCatalogIdentity::new(parent_identity, request.catalog_generation);

    let expected_generation = request.transport.source_generation();
    let (http, fetched) = match request.fetched_manifest.take() {
        Some(fetched_manifest) => {
            validate_fetched_manifest_handoff(&request, &fetched_manifest)?;
            (fetched_manifest.http, fetched_manifest.fetched)
        }
        None => {
            let http = AdaptiveHttpContext::new(
                request.transport,
                request.source_config,
                request.policy.adaptive_limits,
                request.policy.adaptive_retry,
            )
            .map_err(map_fetch_error)?;
            let fetch = AdaptiveResourceFetchRequest::full(
                expected_generation,
                initial_target,
                http.maximum_resource_bytes(AdaptiveResourcePurpose::Manifest),
                AdaptiveResourcePurpose::Manifest,
                AdaptiveResourceQueryApplication::BypassScopedQuery,
            );
            let fetched = http
                .fetch_resource_blocking(fetch)
                .map_err(map_fetch_error)?;
            (http, fetched)
        }
    };
    let effective_manifest_target = fetched.final_target().clone();
    let fragment_secret_forwarding =
        http.resource_secret_forwarding_for(&effective_manifest_target);

    let manifest = {
        let cancellation = http.cancellation().clone();
        let mut is_cancelled = || cancellation.is_cancelled();
        parse_vod_client_manifest_cancellable(
            SmoothManifestParseRequest {
                document_bytes: fetched.bytes(),
                xml_budgets: request.policy.xml_budgets,
                limits: request.policy.manifest_limits.clone(),
            },
            &mut is_cancelled,
        )
        .map_err(|error| {
            if cancellation.is_cancelled()
                || matches!(
                    error,
                    smooth_streaming_manifest_core::SmoothManifestError::Cancelled
                )
            {
                SmoothPrepareError::Cancelled
            } else {
                SmoothPrepareError::Manifest(error)
            }
        })?
    };
    let (video_stream_ordinal, audio_stream_ordinal, aligned_span) =
        validate_manifest_profile(&manifest)?;
    let manifest = Arc::new(manifest);
    Ok(SmoothManifestPreparation {
        http,
        effective_manifest_target,
        fragment_secret_forwarding,
        manifest,
        catalog_identity,
        parent_semantic,
        video_stream_ordinal,
        audio_stream_ordinal,
        aligned_span,
        preferred_height: request.preferred_height,
        policy: request.policy,
    })
}

/// Повторно применяет current caller policy к fetched direct-ingress handoff-у.
fn validate_fetched_manifest_handoff(
    request: &SmoothPrepareRequest<'_>,
    fetched_manifest: &crate::SmoothFetchedManifestInput,
) -> Result<(), SmoothPrepareError> {
    if request.transport.source_generation() != fetched_manifest.http.source_generation() {
        return Err(SmoothPrepareError::FetchedManifestGenerationMismatch);
    }
    if request.transport.target().as_http() != Some(&fetched_manifest.selected_target) {
        return Err(SmoothPrepareError::FetchedManifestTargetMismatch);
    }
    if fetched_manifest.fetched.bytes().len()
        > request.policy.adaptive_limits.maximum_manifest_bytes.get()
    {
        return Err(SmoothPrepareError::FetchedManifestTooLarge);
    }
    Ok(())
}

pub(crate) fn into_prepared_catalog(
    prepared: SmoothManifestPreparation,
    catalog_build: crate::catalog::SmoothCatalogBuild,
) -> SmoothPreparedCatalog {
    SmoothPreparedCatalog {
        catalog: catalog_build.catalog,
        provider_default_selection: catalog_build.provider_selection,
        source_generation: prepared.http.source_generation(),
        aligned_span: prepared.aligned_span,
        runtime_seed: SmoothRuntimeSeed {
            http: prepared.http,
            effective_manifest_target: prepared.effective_manifest_target,
            fragment_secret_forwarding: prepared.fragment_secret_forwarding,
            manifest: prepared.manifest,
            video_rows: catalog_build.video_rows,
            audio_rows: catalog_build.audio_rows,
        },
    }
}

/// Existing fragment/runtime tests deliberately exercise arbitrary non-default rows.
#[cfg(test)]
pub(crate) fn prepare_smooth_vod_all_for_test(
    request: SmoothPrepareRequest<'_>,
) -> Result<SmoothPreparedCatalog, SmoothPrepareError> {
    let prepared = prepare_manifest(request)?;
    let cancellation = prepared.http.cancellation().clone();
    let catalog_build = build_catalog(SmoothCatalogBuildRequest {
        manifest: &prepared.manifest,
        catalog_identity: prepared.catalog_identity.clone(),
        parent_semantic: &prepared.parent_semantic,
        video_stream_ordinal: prepared.video_stream_ordinal,
        audio_stream_ordinal: prepared.audio_stream_ordinal,
        preferred_height: prepared.preferred_height,
        policy: &prepared.policy,
        cancellation: &|| cancellation.is_cancelled(),
    })?;
    Ok(into_prepared_catalog(prepared, catalog_build))
}

/// Проверяет neutral S36P1 intent до создания HTTP session.
fn validate_transport_profile(
    request: &SmoothPrepareRequest<'_>,
) -> Result<(), SmoothPrepareError> {
    if request.transport.presentation() != MediaPresentation::Vod {
        return Err(SmoothTransportProfileError::NonVodPresentation.into());
    }
    if request.transport.component().role() != MediaComponentRole::PresentationManifest {
        return Err(SmoothTransportProfileError::NonPresentationManifestComponent.into());
    }
    if request.transport.http_range_request_limit().is_some() {
        return Err(SmoothTransportProfileError::UnexpectedRangeLimit.into());
    }
    Ok(())
}

/// Проверяет provider shape, exact zero starts и authoritative root upper bound.
pub(crate) fn validate_manifest_profile(
    manifest: &SmoothManifest,
) -> Result<(usize, usize, SmoothAlignedSpan), SmoothPrepareError> {
    let mut video_stream = None;
    let mut audio_stream = None;
    for (ordinal, stream) in manifest.streams().iter().enumerate() {
        let slot = match stream.kind() {
            SmoothStreamKind::Video => &mut video_stream,
            SmoothStreamKind::Audio => &mut audio_stream,
        };
        if slot.replace(ordinal).is_some() {
            return Err(SmoothProfileError::StreamShape.into());
        }
    }
    let (Some(video_ordinal), Some(audio_ordinal)) = (video_stream, audio_stream) else {
        return Err(SmoothProfileError::StreamShape.into());
    };
    if manifest.streams().len() != 2 {
        return Err(SmoothProfileError::StreamShape.into());
    }
    for ordinal in [video_ordinal, audio_ordinal] {
        let stream = &manifest.streams()[ordinal];
        if stream.qualities().is_empty() {
            return Err(SmoothProfileError::EmptyQualityAxis.into());
        }
        if stream.timeline().first_start().ticks() != 0 {
            return Err(SmoothProfileError::NonZeroStart.into());
        }
        if stream.timeline().last_end() > manifest.duration() {
            return Err(SmoothProfileError::ComponentExceedsRootDuration.into());
        }
    }
    let video_end_exclusive = manifest.streams()[video_ordinal].timeline().last_end();
    let audio_end_exclusive = manifest.streams()[audio_ordinal].timeline().last_end();
    if video_end_exclusive != manifest.duration() {
        return Err(SmoothProfileError::VideoDurationMismatch.into());
    }
    let root_zero = SmoothTime::new(0, manifest.duration().timescale());
    Ok((
        video_ordinal,
        audio_ordinal,
        SmoothAlignedSpan::new(
            root_zero,
            manifest.duration(),
            video_end_exclusive,
            audio_end_exclusive,
        ),
    ))
}

/// Схлопывает cancellation transport-а в единый public outcome.
fn map_fetch_error(error: AdaptiveTransportError) -> SmoothPrepareError {
    if matches!(error, AdaptiveTransportError::Cancelled) {
        SmoothPrepareError::Cancelled
    } else {
        SmoothPrepareError::Fetch(error)
    }
}

#[cfg(test)]
mod tests {
    use std::io::{Read, Write};
    use std::net::{TcpListener, TcpStream};
    use std::thread;
    use std::time::{Duration, Instant};

    use rustiplayer_config::NetworkConfig;
    use source_core::{
        CancellationToken, HttpHeader, HttpPathScope, HttpRequestTarget, SourceRuntimeConfig,
        ValidatedHttpHeaders,
    };
    use web_media_adaptive::{
        AdaptiveHttpContext, AdaptiveResourceFetchRequest, AdaptiveResourcePurpose,
        AdaptiveResourceQueryApplication,
    };
    use web_media_core::{
        CandidateFormatIdentity, CandidateIdentity, ComponentVariantCatalogGeneration,
        ExtractionGeneration, PreferredHeightPolicy, SemanticIdentity, SourceIdentity,
    };
    use web_media_transport_api::{
        MediaComponentIdentity, MediaComponentRole, MediaPresentation, RedirectHopLimit,
        RedirectPolicy, SecretRequestContext, SecretRequestScope, SourceGeneration,
        TransportOpenRequest, TransportProviderId,
    };

    use super::validate_manifest_profile;
    use crate::test_support::{
        CANONICAL_PIFF_MANIFEST, DIFFERING_CLOCKS_MANIFEST, VALID_MANIFEST, parse, policy,
    };
    use crate::{
        SmoothFetchedManifestInput, SmoothPrepareError, SmoothPrepareRequest, SmoothProfileError,
        prepare_smooth_vod,
    };

    /// Принимает ровно один request и возвращает его headers test thread-у.
    fn serve_once(
        listener: TcpListener,
        response: impl FnOnce() -> Vec<u8> + Send + 'static,
    ) -> thread::JoinHandle<String> {
        thread::spawn(move || {
            listener
                .set_nonblocking(true)
                .expect("test listener nonblocking");
            let deadline = Instant::now() + Duration::from_secs(3);
            loop {
                match listener.accept() {
                    Ok((mut stream, _)) => {
                        let request = read_request(&mut stream);
                        stream
                            .write_all(&response())
                            .expect("write local HTTP response");
                        return request;
                    }
                    Err(error) if error.kind() == std::io::ErrorKind::WouldBlock => {
                        assert!(Instant::now() < deadline, "local request timeout");
                        thread::sleep(Duration::from_millis(2));
                    }
                    Err(error) => panic!("accept local request: {error}"),
                }
            }
        })
    }

    /// Читает только bounded HTTP headers, достаточные для secret assertion.
    fn read_request(stream: &mut TcpStream) -> String {
        stream
            .set_read_timeout(Some(Duration::from_secs(3)))
            .expect("read timeout");
        let mut request = Vec::new();
        let mut chunk = [0_u8; 1_024];
        while !request.windows(4).any(|window| window == b"\r\n\r\n") {
            let count = stream.read(&mut chunk).expect("read local request");
            assert!(count > 0, "request ended before headers");
            request.extend_from_slice(&chunk[..count]);
            assert!(request.len() <= 16 * 1_024, "test request headers bounded");
        }
        String::from_utf8(request).expect("local HTTP request is ASCII")
    }

    /// Собирает minimal production request с реальным provider id от caller-а.
    fn transport_request(
        target: &HttpRequestTarget,
        redirects: RedirectPolicy,
    ) -> TransportOpenRequest {
        transport_request_with_generation(target, redirects, SourceGeneration::new(17))
    }

    /// Позволяет focused fetched-handoff tests построить stale generation.
    fn transport_request_with_generation(
        target: &HttpRequestTarget,
        redirects: RedirectPolicy,
        generation: SourceGeneration,
    ) -> TransportOpenRequest {
        let source = SourceIdentity::new(91);
        let exact = CandidateIdentity::new(
            source,
            ExtractionGeneration::new(3),
            CandidateFormatIdentity::new("smooth-local").expect("format identity"),
        );
        let semantic = SemanticIdentity::new(source, "smooth-local").expect("semantic identity");
        let component =
            MediaComponentIdentity::new(exact, semantic, MediaComponentRole::PresentationManifest)
                .expect("component");
        let scope = SecretRequestScope::from_target(
            target,
            HttpPathScope::new("/").expect("root path scope"),
        );
        let secrets = SecretRequestContext::builder(scope)
            .with_headers(
                ValidatedHttpHeaders::new(vec![HttpHeader::new(
                    "authorization",
                    "Bearer do-not-leak",
                )])
                .expect("secret header"),
            )
            .build();
        TransportOpenRequest::new(
            TransportProviderId::new("smooth-fixture").expect("provider id"),
            component,
            target.clone(),
            MediaPresentation::Vod,
            generation,
            secrets,
            redirects,
            CancellationToken::new(),
        )
        .expect("transport request")
    }

    /// Загружает root один раз через exact context, который затем передаётся preparation-у.
    fn fetched_manifest_handoff(
        target: &HttpRequestTarget,
        transport: TransportOpenRequest,
        source_config: &SourceRuntimeConfig,
        preparation_policy: &crate::SmoothPreparationPolicy,
    ) -> SmoothFetchedManifestInput {
        let http = AdaptiveHttpContext::new(
            transport,
            source_config,
            preparation_policy.adaptive_limits,
            preparation_policy.adaptive_retry,
        )
        .expect("adaptive context");
        let fetched = http
            .fetch_resource_blocking(AdaptiveResourceFetchRequest::full(
                http.source_generation(),
                target.clone(),
                http.maximum_resource_bytes(AdaptiveResourcePurpose::Manifest),
                AdaptiveResourcePurpose::Manifest,
                AdaptiveResourceQueryApplication::BypassScopedQuery,
            ))
            .expect("single manifest fetch");
        SmoothFetchedManifestInput::new(target.clone(), http, fetched)
    }

    #[test]
    fn accepts_differing_component_clocks_with_exact_root_alignment() {
        let manifest = parse(DIFFERING_CLOCKS_MANIFEST);
        let (_, _, span) =
            validate_manifest_profile(&manifest).expect("exact rational clocks align");

        assert_eq!(span.start().ticks(), 0);
        assert_eq!(span.end_exclusive(), manifest.duration());
    }

    #[test]
    fn shorter_audio_end_is_preserved_as_exact_evidence() {
        let document = VALID_MANIFEST.replacen(r#"<c d="48000"/>"#, r#"<c d="24000"/>"#, 1);
        let manifest = parse(&document);
        let (_, _, span) =
            validate_manifest_profile(&manifest).expect("short audio remains within root");

        assert_eq!(span.end_exclusive().ticks(), 20_000_000);
        assert_eq!(span.video_end_exclusive().ticks(), 20_000_000);
        assert_eq!(span.audio_end_exclusive().ticks(), 72_000);
        assert_eq!(span.audio_end_exclusive().timescale().get(), 48_000);
        assert_eq!(span.common_end_exclusive(), span.audio_end_exclusive());
    }

    #[test]
    fn rejects_video_ending_before_root_duration() {
        let document = VALID_MANIFEST.replacen(r#"<c d="10000000"/>"#, r#"<c d="1000000"/>"#, 1);
        let manifest = parse(&document);

        assert!(matches!(
            validate_manifest_profile(&manifest),
            Err(crate::SmoothPrepareError::Profile(
                SmoothProfileError::VideoDurationMismatch
            ))
        ));
    }

    #[test]
    fn canonical_piff_mismatch_is_exposed_without_tolerance() {
        let manifest = parse(CANONICAL_PIFF_MANIFEST);
        let (_, _, span) =
            validate_manifest_profile(&manifest).expect("canonical ends remain within root");

        assert_eq!(span.end_exclusive().ticks(), 7_340_000_000);
        assert_eq!(span.video_end_exclusive().ticks(), 7_340_000_000);
        assert_eq!(span.audio_end_exclusive().ticks(), 7_339_363_333);
        assert_eq!(span.common_end_exclusive(), span.audio_end_exclusive());
    }

    #[test]
    fn rejects_extra_component_stream() {
        let extra_audio = VALID_MANIFEST
            .split("<StreamIndex Type=\"audio\"")
            .nth(1)
            .expect("audio stream")
            .split("</StreamIndex>")
            .next()
            .expect("audio body");
        let document = VALID_MANIFEST
            .replace("StreamIndexCount=\"2\"", "StreamIndexCount=\"3\"")
            .replace(
                "</SmoothStreamingMedia>",
                &format!(
                    "<StreamIndex Type=\"audio\"{extra_audio}</StreamIndex>\n</SmoothStreamingMedia>"
                ),
            );
        let manifest = parse(&document);

        assert!(matches!(
            validate_manifest_profile(&manifest),
            Err(crate::SmoothPrepareError::Profile(
                SmoothProfileError::StreamShape
            ))
        ));
    }

    #[test]
    fn redirect_uses_effective_target_and_strips_cross_origin_secret() {
        let manifest_listener = TcpListener::bind("127.0.0.1:0").expect("manifest listener");
        let manifest_address = manifest_listener.local_addr().expect("manifest address");
        let manifest_target = HttpRequestTarget::parse_exact(format!(
            "http://{manifest_address}/effective/manifest.ismc"
        ))
        .expect("manifest target");
        let manifest_thread = serve_once(manifest_listener, || {
            format!(
                "HTTP/1.1 200 OK\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{}",
                VALID_MANIFEST.len(),
                VALID_MANIFEST
            )
            .into_bytes()
        });

        let redirect_listener = TcpListener::bind("127.0.0.1:0").expect("redirect listener");
        let redirect_address = redirect_listener.local_addr().expect("redirect address");
        let initial_target =
            HttpRequestTarget::parse_exact(format!("http://{redirect_address}/entry.ismc"))
                .expect("initial target");
        let redirect_location = format!("http://{manifest_address}/effective/manifest.ismc");
        let redirect_thread = serve_once(redirect_listener, move || {
            format!(
                "HTTP/1.1 302 Found\r\nLocation: {redirect_location}\r\nContent-Length: 0\r\nConnection: close\r\n\r\n"
            )
            .into_bytes()
        });

        let source_config = SourceRuntimeConfig::from_network_config(&NetworkConfig::default())
            .expect("source config");
        let prepared = prepare_smooth_vod(SmoothPrepareRequest::new(
            transport_request(
                &initial_target,
                RedirectPolicy::cross_origin_without_secrets(
                    RedirectHopLimit::new(2).expect("redirect budget"),
                ),
            ),
            &source_config,
            ComponentVariantCatalogGeneration::new(44),
            PreferredHeightPolicy::NoPreference,
            policy(64 * 1_024),
        ))
        .expect("redirected manifest preparation");

        let initial_request = redirect_thread.join().expect("redirect server");
        let effective_request = manifest_thread.join().expect("manifest server");
        assert!(
            initial_request
                .to_ascii_lowercase()
                .contains("authorization:")
        );
        assert!(
            !effective_request
                .to_ascii_lowercase()
                .contains("authorization:")
        );
        assert_eq!(
            prepared.runtime_seed.effective_manifest_target,
            manifest_target
        );
        let diagnostics = format!("{prepared:?}");
        assert!(!diagnostics.contains("entry.ismc"));
        assert!(!diagnostics.contains("do-not-leak"));
    }

    #[test]
    fn fetched_manifest_handoff_is_reused_without_second_root_request() {
        let listener = TcpListener::bind("127.0.0.1:0").expect("manifest listener");
        let address = listener.local_addr().expect("manifest address");
        let target = HttpRequestTarget::parse_exact(format!("http://{address}/Manifest"))
            .expect("manifest target");
        let server = serve_once(listener, || {
            format!(
                "HTTP/1.1 200 OK\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{}",
                VALID_MANIFEST.len(),
                VALID_MANIFEST
            )
            .into_bytes()
        });
        let source_config = SourceRuntimeConfig::from_network_config(&NetworkConfig::default())
            .expect("source config");
        let preparation_policy = policy(64 * 1_024);
        let transport = transport_request(
            &target,
            RedirectPolicy::same_origin(
                RedirectHopLimit::new(2).expect("redirect budget for fetched manifest handoff"),
            ),
        );
        let fetched_manifest = fetched_manifest_handoff(
            &target,
            transport.clone(),
            &source_config,
            &preparation_policy,
        );
        let prepared = prepare_smooth_vod(
            SmoothPrepareRequest::new(
                transport,
                &source_config,
                ComponentVariantCatalogGeneration::new(44),
                PreferredHeightPolicy::NoPreference,
                preparation_policy,
            )
            .with_fetched_manifest(fetched_manifest),
        )
        .expect("fetched manifest preparation");

        assert_eq!(prepared.catalog().identity().generation().value(), 44);
        let request = server.join().expect("manifest server");
        assert!(request.starts_with("GET /Manifest "));
    }

    #[test]
    fn fetched_manifest_handoff_rejects_foreign_generation_before_parse() {
        let listener = TcpListener::bind("127.0.0.1:0").expect("manifest listener");
        let address = listener.local_addr().expect("manifest address");
        let target = HttpRequestTarget::parse_exact(format!("http://{address}/Manifest"))
            .expect("manifest target");
        let server = serve_once(listener, || {
            format!(
                "HTTP/1.1 200 OK\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{}",
                VALID_MANIFEST.len(),
                VALID_MANIFEST
            )
            .into_bytes()
        });
        let source_config = SourceRuntimeConfig::from_network_config(&NetworkConfig::default())
            .expect("source config");
        let preparation_policy = policy(64 * 1_024);
        let fetched_transport = transport_request_with_generation(
            &target,
            RedirectPolicy::same_origin(
                RedirectHopLimit::new(2).expect("redirect budget for fetched manifest handoff"),
            ),
            SourceGeneration::new(17),
        );
        let fetched_manifest = fetched_manifest_handoff(
            &target,
            fetched_transport,
            &source_config,
            &preparation_policy,
        );
        let current_transport = transport_request_with_generation(
            &target,
            RedirectPolicy::same_origin(
                RedirectHopLimit::new(2).expect("redirect budget for fetched manifest handoff"),
            ),
            SourceGeneration::new(18),
        );
        let error = prepare_smooth_vod(
            SmoothPrepareRequest::new(
                current_transport,
                &source_config,
                ComponentVariantCatalogGeneration::new(44),
                PreferredHeightPolicy::NoPreference,
                preparation_policy,
            )
            .with_fetched_manifest(fetched_manifest),
        )
        .expect_err("foreign fetched generation должна fail closed");

        assert!(matches!(
            error,
            SmoothPrepareError::FetchedManifestGenerationMismatch
        ));
        server.join().expect("manifest server");
    }

    #[test]
    fn fetched_manifest_handoff_rejects_foreign_selected_target_before_parse() {
        let listener = TcpListener::bind("127.0.0.1:0").expect("manifest listener");
        let address = listener.local_addr().expect("manifest address");
        let fetched_target = HttpRequestTarget::parse_exact(format!("http://{address}/Manifest"))
            .expect("fetched manifest target");
        let selected_target =
            HttpRequestTarget::parse_exact(format!("http://{address}/OtherManifest"))
                .expect("selected manifest target");
        let server = serve_once(listener, || {
            format!(
                "HTTP/1.1 200 OK\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{}",
                VALID_MANIFEST.len(),
                VALID_MANIFEST
            )
            .into_bytes()
        });
        let source_config = SourceRuntimeConfig::from_network_config(&NetworkConfig::default())
            .expect("source config");
        let preparation_policy = policy(64 * 1_024);
        let source_generation = SourceGeneration::new(19);
        let fetched_transport = transport_request_with_generation(
            &fetched_target,
            RedirectPolicy::same_origin(
                RedirectHopLimit::new(2).expect("redirect budget for fetched manifest handoff"),
            ),
            source_generation,
        );
        let fetched_manifest = fetched_manifest_handoff(
            &fetched_target,
            fetched_transport,
            &source_config,
            &preparation_policy,
        );
        let current_transport = transport_request_with_generation(
            &selected_target,
            RedirectPolicy::same_origin(
                RedirectHopLimit::new(2).expect("redirect budget for fetched manifest handoff"),
            ),
            source_generation,
        );
        let error = prepare_smooth_vod(
            SmoothPrepareRequest::new(
                current_transport,
                &source_config,
                ComponentVariantCatalogGeneration::new(45),
                PreferredHeightPolicy::NoPreference,
                preparation_policy,
            )
            .with_fetched_manifest(fetched_manifest),
        )
        .expect_err("foreign fetched target должен fail closed");

        assert!(matches!(
            error,
            SmoothPrepareError::FetchedManifestTargetMismatch
        ));
        server.join().expect("manifest server");
    }
}
