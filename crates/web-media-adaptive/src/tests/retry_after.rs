//! Functional regression для production bounded fetch → retry → success boundary.

use super::*;

/// Реальный blocking fetch не повторяет `429` раньше валидного delta-seconds hint-а.
#[test]
fn blocking_fetch_waits_for_retry_after_before_second_request() {
    let request_instants = Arc::new(Mutex::new(Vec::new()));
    let server_request_instants = Arc::clone(&request_instants);
    let server = LocalServer::start(move |request_index, _request| {
        server_request_instants
            .lock()
            .expect("request instants mutex")
            .push(Instant::now());
        if request_index == 0 {
            response(
                "429 Too Many Requests",
                &[("Retry-After", "1".to_owned())],
                b"",
            )
        } else {
            response("200 OK", &[], b"ok")
        }
    });
    let target = server.target("/rate-limited-manifest");
    let mut adaptive_context = context(
        &target,
        CancellationToken::new(),
        same_origin_redirects(),
        None,
        None,
    );
    adaptive_context.retry = AdaptiveRetryPolicy::new(
        NonZeroU8::new(2).expect("attempt budget"),
        Duration::from_millis(5),
        Duration::from_millis(20),
        Duration::from_secs(2),
    )
    .expect("retry policy");

    let fetched = adaptive_context
        .fetch_resource_blocking(AdaptiveResourceFetchRequest::full(
            SourceGeneration::new(1),
            target,
            NonZeroUsize::new(64).expect("manifest bound"),
            AdaptiveResourcePurpose::Manifest,
            AdaptiveResourceQueryApplication::BypassScopedQuery,
        ))
        .expect("retry-after fetch must recover");

    assert_eq!(fetched.into_bytes(), b"ok");
    let observed_instants = request_instants.lock().expect("request instants mutex");
    assert_eq!(observed_instants.len(), 2);
    assert!(
        observed_instants[1].duration_since(observed_instants[0]) >= Duration::from_millis(900),
        "second request must not precede the one-second server hint"
    );
}
