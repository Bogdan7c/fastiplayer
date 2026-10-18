//! Вспомогательный вид контекста (`for_auxiliary_resources`): сбои субтитров не трогают видео.

use super::*;

/// Контекст с записывающим наблюдателем expiry (как у видео-открытия с recovery).
fn observed_context(
    target: &HttpRequestTarget,
    cancellation: CancellationToken,
) -> (AdaptiveHttpContext, Arc<RecordingExpiryObserver>) {
    let observer = Arc::new(RecordingExpiryObserver::default());
    let context = context_with_options(
        target,
        cancellation,
        same_origin_redirects(),
        TestContextOptions {
            authorization: None,
            segment_query: None,
            key_query: None,
            presentation: MediaPresentation::Vod,
            endpoint_expiry_observer: Some(observer.clone()),
        },
    );
    (context, observer)
}

fn full_request(
    target: &HttpRequestTarget,
    purpose: AdaptiveResourcePurpose,
) -> AdaptiveResourceFetchRequest {
    AdaptiveResourceFetchRequest::full(
        SourceGeneration::new(1),
        target.clone(),
        NonZeroUsize::new(64).expect("body bound"),
        purpose,
        AdaptiveResourceQueryApplication::BypassScopedQuery,
    )
}

/// 404/410 вспомогательного ресурса сохраняют исходную ошибку, но не публикуют expiry;
/// такой же сбой видео-контекста (контроль) публикует ровно один сигнал на запрос.
#[test]
fn auxiliary_view_never_publishes_expiry_while_video_view_still_does() {
    let server = LocalServer::start(|_, _| response("404 Not Found", &[], &[]));
    let target = server.target("/subs/missing.vtt");
    let (video_context, observer) = observed_context(&target, CancellationToken::new());
    let auxiliary = video_context.for_auxiliary_resources();

    for purpose in [
        AdaptiveResourcePurpose::Manifest,
        AdaptiveResourcePurpose::MediaSegment,
    ] {
        let error = auxiliary
            .fetch_resource_blocking(full_request(&target, purpose))
            .expect_err("404 must stay an error");
        assert_eq!(error.http_status_code(), Some(404));
    }
    assert!(
        observer.recorded_signals().is_empty(),
        "auxiliary failures must not reach the video recovery observer"
    );

    let error = video_context
        .fetch_resource_blocking(full_request(&target, AdaptiveResourcePurpose::MediaSegment))
        .expect_err("video 404 must stay an error");
    assert_eq!(error.http_status_code(), Some(404));
    let signals = observer.recorded_signals();
    assert_eq!(signals.len(), 1, "video failure still arms recovery");
    assert_eq!(
        signals[0].resource_kind(),
        EndpointExpiryResourceKind::MediaSegment
    );
}

/// Успешный вспомогательный запрос шарит сессию/отмену, но не кладёт байты в кэш видео
/// и не читает его: второй запрос того же адреса снова идёт в сеть.
#[test]
fn auxiliary_view_shares_cancellation_but_not_completed_cache() {
    let server = LocalServer::start(|_, _| response("200 OK", &[], b"cue"));
    let target = server.target("/subs/ok.vtt");
    let cancellation = CancellationToken::new();
    let (video_context, observer) = observed_context(&target, cancellation.clone());
    let auxiliary = video_context.for_auxiliary_resources();

    let fetched = auxiliary
        .fetch_resource_blocking(full_request(&target, AdaptiveResourcePurpose::MediaSegment))
        .expect("auxiliary fetch succeeds");
    assert_eq!(fetched.bytes(), b"cue");
    assert!(observer.recorded_signals().is_empty());
    let video_fetched = video_context
        .fetch_resource_blocking(full_request(&target, AdaptiveResourcePurpose::MediaSegment))
        .expect("video fetch succeeds");
    assert_eq!(video_fetched.bytes(), b"cue");
    assert_eq!(
        server.requests().len(),
        2,
        "video must not be served from the auxiliary cache"
    );

    cancellation.cancel();
    assert!(
        auxiliary
            .fetch_resource_blocking(full_request(&target, AdaptiveResourcePurpose::MediaSegment))
            .is_err(),
        "auxiliary view is cancelled together with the source"
    );
}
