//! Поверхность внешнего транспорта: чтение запроса, проверка тела, честные ошибки.

use std::io;
use std::num::NonZeroUsize;
use std::time::Duration;

use super::{HttpExternalTransportFailure, HttpStatusCode};
use crate::{
    HttpBoundedByteRange, HttpBoundedFetchKind, HttpBoundedFetchRequest, HttpBoundedResponse,
    HttpHeader, HttpRequestTarget, HttpRetryAfter, SourceError,
};

/// Адрес с секретами в userinfo/query: ни одна ошибка не должна их напечатать.
const SECRET_TARGET: &str = "https://user:password@cdn.example.test/seg.ts?token=secret";

fn target() -> HttpRequestTarget {
    HttpRequestTarget::parse_exact(SECRET_TARGET).expect("valid target")
}

fn range_request(start: u64, length: usize) -> HttpBoundedFetchRequest {
    HttpBoundedFetchRequest::range(
        target(),
        vec![HttpHeader::new("authorization", "Bearer scoped")],
        HttpBoundedByteRange::new(start, NonZeroUsize::new(length).expect("length"))
            .expect("range"),
        HttpBoundedFetchKind::Media,
    )
}

fn full_request(maximum_body_bytes: usize) -> HttpBoundedFetchRequest {
    HttpBoundedFetchRequest::full(
        target(),
        Vec::new(),
        NonZeroUsize::new(maximum_body_bytes).expect("bound"),
        HttpBoundedFetchKind::Metadata,
    )
}

fn assert_redacted(error: &SourceError) {
    let formatted = format!("{error} {error:?}");
    assert!(!formatted.contains("password"), "{formatted}");
    assert!(!formatted.contains("secret"), "{formatted}");
    assert!(!formatted.contains("cdn.example.test"), "{formatted}");
}

#[test]
fn accessors_expose_exactly_what_the_request_was_built_with() {
    let request = range_request(100, 8);

    assert_eq!(
        request.target().expose_secret_for_request(),
        target().expose_secret_for_request()
    );
    assert_eq!(request.headers().len(), 1);
    assert_eq!(request.headers()[0].name, "authorization");
    let byte_range = request.byte_range().expect("range request keeps its range");
    assert_eq!((byte_range.start(), byte_range.length().get()), (100, 8));
    // Для диапазона лимит тела равен его длине — внешний транспорт не может запросить больше.
    assert_eq!(request.maximum_body_bytes().get(), 8);
    assert_eq!(request.kind(), HttpBoundedFetchKind::Media);

    let full = full_request(64);
    assert_eq!(full.byte_range(), None);
    assert_eq!(full.maximum_body_bytes().get(), 64);
    assert_eq!(full.kind(), HttpBoundedFetchKind::Metadata);
}

#[test]
fn external_body_of_exact_range_length_becomes_response_without_range_metadata() {
    let request = range_request(100, 4);

    let response =
        HttpBoundedResponse::from_external_body(&request, b"abcd".to_vec()).expect("exact body");

    assert!(response.range_metadata().is_none());
    assert_eq!(response.into_bytes(), b"abcd");
}

#[test]
fn external_short_range_body_is_retryable_unexpected_eof() {
    let request = range_request(100, 4);

    let error = HttpBoundedResponse::from_external_body(&request, b"ab".to_vec())
        .expect_err("short range body must fail");

    assert!(matches!(
        error,
        SourceError::UnexpectedEof {
            offset: 100,
            expected_bytes: 4,
            actual_bytes: 2,
        }
    ));
    assert!(error.is_transient_network_failure());
}

#[test]
fn external_body_above_bound_is_non_retryable_body_too_large() {
    let error = HttpBoundedResponse::from_external_body(&full_request(3), b"abcd".to_vec())
        .expect_err("oversized body must fail");

    assert!(matches!(
        error,
        SourceError::HttpBodyTooLarge {
            operation: "bounded-metadata-fetch",
            maximum_bytes: 3,
            ..
        }
    ));
    assert!(!error.is_transient_network_failure());
    assert_redacted(&error);

    // Короче лимита полный ресурс — норма: лимит верхний, а не точный.
    let response = HttpBoundedResponse::from_external_body(&full_request(3), b"ab".to_vec())
        .expect("full body under bound");
    assert_eq!(response.into_bytes(), b"ab");
}

#[test]
fn interrupted_and_timed_out_failures_are_retryable_and_redacted() {
    let context = range_request(0, 4).external_failure_context();

    let interrupted = context.failure(HttpExternalTransportFailure::Interrupted(io::Error::new(
        io::ErrorKind::ConnectionReset,
        "bridge dropped",
    )));
    assert!(matches!(
        interrupted,
        SourceError::HttpBodyRead {
            operation: "bounded-media-fetch",
            ..
        }
    ));
    assert!(interrupted.is_transient_network_failure());
    assert_redacted(&interrupted);

    let timed_out = context.failure(HttpExternalTransportFailure::TimedOut);
    assert!(matches!(
        timed_out,
        SourceError::HttpTimeout {
            operation: "bounded-media-fetch",
            ..
        }
    ));
    assert!(timed_out.is_transient_network_failure());
    assert_redacted(&timed_out);
}

#[test]
fn status_failure_keeps_status_and_retry_after_and_its_own_retryability() {
    let context = full_request(16).external_failure_context();

    let unavailable = context.failure(HttpExternalTransportFailure::Status {
        status: HttpStatusCode::SERVICE_UNAVAILABLE,
        retry_after: HttpRetryAfter::Delay(Duration::from_secs(2)),
    });
    assert!(matches!(
        unavailable,
        SourceError::HttpStatus {
            status: HttpStatusCode::SERVICE_UNAVAILABLE,
            ..
        }
    ));
    assert_eq!(
        unavailable.http_retry_after(),
        HttpRetryAfter::Delay(Duration::from_secs(2))
    );
    assert!(unavailable.is_transient_network_failure());
    assert_redacted(&unavailable);

    let gone = context.failure(HttpExternalTransportFailure::Status {
        status: HttpStatusCode::GONE,
        retry_after: HttpRetryAfter::Unavailable,
    });
    assert!(!gone.is_transient_network_failure());
}

#[test]
fn failure_context_debug_never_prints_target() {
    let formatted = format!("{:?}", range_request(0, 4).external_failure_context());

    assert!(formatted.contains("bounded-media-fetch"));
    assert!(!formatted.contains("cdn.example.test"));
    assert!(!formatted.contains("secret"));
}
