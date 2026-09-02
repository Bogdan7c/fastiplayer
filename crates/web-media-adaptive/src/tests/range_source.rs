use std::num::{NonZeroU64, NonZeroUsize};

use source_core::{ByteSource, CancellationToken, HttpRepresentationChange, SourceError};
use web_media_transport_api::{RedirectHopLimit, RedirectPolicy, SourceGeneration};

use super::{LocalServer, context, response};
use crate::{
    AdaptiveRangeByteSource, AdaptiveRangeSourceConfig, AdaptiveRangeSourceOpenError,
    AdaptiveResourceQueryApplication,
};

fn redirect_policy() -> RedirectPolicy {
    RedirectPolicy::same_origin(RedirectHopLimit::new(3).expect("valid hop limit"))
}

fn range_config() -> AdaptiveRangeSourceConfig {
    AdaptiveRangeSourceConfig::new(
        NonZeroUsize::new(4).expect("non-zero page"),
        AdaptiveResourceQueryApplication::ApplyScopedReplacement,
    )
}

#[test]
fn range_source_proves_total_reads_partial_tail_and_never_uses_full_get() {
    let resource = b"abcde";
    let server = LocalServer::start(move |_index, request| {
        let range = request
            .headers
            .lines()
            .find_map(|line| {
                line.strip_prefix("range: ")
                    .or_else(|| line.strip_prefix("Range: "))
            })
            .expect("every request is Range");
        let (start, end) = match range {
            "bytes=0-0" => (0, 0),
            "bytes=0-3" => (0, 3),
            "bytes=4-4" => (4, 4),
            unexpected => panic!("unexpected Range {unexpected}"),
        };
        response(
            "206 Partial Content",
            &[
                ("Content-Range", format!("bytes {start}-{end}/5")),
                ("ETag", "\"stable\"".to_owned()),
            ],
            &resource[start..=end],
        )
    });
    let target = server.target("/representation.bin");
    let cancellation = CancellationToken::new();
    let mut source = AdaptiveRangeByteSource::open(
        context(&target, cancellation.clone(), redirect_policy(), None, None),
        target,
        SourceGeneration::new(1),
        range_config(),
    )
    .expect("Range source opens");
    assert_eq!(source.content_length(), Some(5));
    let mut first = [0_u8; 8];
    assert_eq!(
        source.read(&mut first, &cancellation).expect("first read"),
        4
    );
    assert_eq!(&first[..4], b"abcd");
    source.seek(4).expect("seek to tail");
    let mut tail = [0_u8; 8];
    assert_eq!(source.read(&mut tail, &cancellation).expect("tail read"), 1);
    assert_eq!(tail[0], b'e');
    assert_eq!(source.read(&mut tail, &cancellation).expect("EOF"), 0);
    assert_eq!(server.request_count(), 3);
    assert!(server.requests().iter().all(|request| {
        request
            .headers
            .to_ascii_lowercase()
            .contains("range: bytes=")
    }));
}

#[test]
fn exposed_prefix_bounds_consumer_reads_but_preserves_physical_range_identity() {
    let resource = b"abcde";
    let server = LocalServer::start(move |_index, request| {
        let range = request
            .headers
            .lines()
            .find_map(|line| {
                line.strip_prefix("range: ")
                    .or_else(|| line.strip_prefix("Range: "))
            })
            .expect("every request is Range");
        let (start, end) = match range {
            "bytes=0-0" => (0, 0),
            "bytes=0-2" => (0, 2),
            unexpected => panic!("unexpected Range {unexpected}"),
        };
        response(
            "206 Partial Content",
            &[
                ("Content-Range", format!("bytes {start}-{end}/5")),
                ("ETag", "\"stable\"".to_owned()),
            ],
            &resource[start..=end],
        )
    });
    let target = server.target("/representation.bin");
    let cancellation = CancellationToken::new();
    let mut source = AdaptiveRangeByteSource::open(
        context(&target, cancellation.clone(), redirect_policy(), None, None),
        target.clone(),
        SourceGeneration::new(1),
        range_config().with_exposed_content_length(NonZeroU64::new(3).expect("prefix")),
    )
    .expect("bounded Range source opens");

    assert_eq!(source.content_length(), Some(3));
    let mut output = [0_u8; 8];
    assert_eq!(source.read(&mut output, &cancellation).expect("prefix"), 3);
    assert_eq!(&output[..3], b"abc");
    assert_eq!(source.read(&mut output, &cancellation).expect("EOF"), 0);
    assert_eq!(server.request_count(), 2);

    let oversized_prefix = AdaptiveRangeByteSource::open(
        context(&target, cancellation, redirect_policy(), None, None),
        target,
        SourceGeneration::new(1),
        range_config().with_exposed_content_length(NonZeroU64::new(6).expect("prefix")),
    )
    .expect_err("logical prefix cannot exceed the proven physical representation");
    assert!(matches!(
        oversized_prefix,
        AdaptiveRangeSourceOpenError::ExposedContentLengthExceedsResource
    ));
    assert_eq!(server.request_count(), 3);
}

#[test]
fn range_source_rejects_200_and_missing_total_during_probe() {
    let non_range = LocalServer::start(|_, _| response("200 OK", &[], b"x"));
    let target = non_range.target("/no-range.bin");
    let error = AdaptiveRangeByteSource::open(
        context(
            &target,
            CancellationToken::new(),
            redirect_policy(),
            None,
            None,
        ),
        target,
        SourceGeneration::new(1),
        range_config(),
    )
    .expect_err("200 must not publish fake seekability");
    assert!(matches!(error, AdaptiveRangeSourceOpenError::Transport(_)));

    let missing_total = LocalServer::start(|_, _| {
        response(
            "206 Partial Content",
            &[("Content-Range", "bytes 0-0/*".to_owned())],
            b"x",
        )
    });
    let target = missing_total.target("/missing-total.bin");
    let error = AdaptiveRangeByteSource::open(
        context(
            &target,
            CancellationToken::new(),
            redirect_policy(),
            None,
            None,
        ),
        target,
        SourceGeneration::new(1),
        range_config(),
    )
    .expect_err("unknown total must be rejected");
    assert!(matches!(
        error,
        AdaptiveRangeSourceOpenError::MissingTotalLength
    ));
}

#[test]
fn range_source_fences_total_and_validator_changes() {
    let server = LocalServer::start(|index, _| {
        let (total, etag) = if index == 0 {
            (5, "\"v1\"")
        } else {
            (6, "\"v2\"")
        };
        response(
            "206 Partial Content",
            &[
                ("Content-Range", format!("bytes 0-0/{total}")),
                ("ETag", etag.to_owned()),
            ],
            b"x",
        )
    });
    let target = server.target("/changing.bin");
    let cancellation = CancellationToken::new();
    let mut source = AdaptiveRangeByteSource::open(
        context(&target, cancellation.clone(), redirect_policy(), None, None),
        target,
        SourceGeneration::new(1),
        range_config(),
    )
    .expect("probe succeeds");
    let mut output = [0_u8; 1];
    let error = source
        .read(&mut output, &cancellation)
        .expect_err("changed total must fail");
    assert!(matches!(
        error,
        SourceError::HttpRepresentationChanged {
            reason: HttpRepresentationChange::TotalLength
        }
    ));
}

#[test]
fn range_source_rejects_stale_generation_and_cancel_before_network() {
    let server = LocalServer::start(|_, _| {
        response(
            "206 Partial Content",
            &[("Content-Range", "bytes 0-0/1".to_owned())],
            b"x",
        )
    });
    let target = server.target("/generation.bin");
    let error = AdaptiveRangeByteSource::open(
        context(
            &target,
            CancellationToken::new(),
            redirect_policy(),
            None,
            None,
        ),
        target.clone(),
        SourceGeneration::new(2),
        range_config(),
    )
    .expect_err("stale generation fails");
    assert!(matches!(error, AdaptiveRangeSourceOpenError::Transport(_)));

    let cancellation = CancellationToken::new();
    cancellation.cancel();
    let error = AdaptiveRangeByteSource::open(
        context(&target, cancellation, redirect_policy(), None, None),
        target,
        SourceGeneration::new(1),
        range_config(),
    )
    .expect_err("cancelled generation fails");
    assert!(matches!(
        error,
        AdaptiveRangeSourceOpenError::Transport(crate::AdaptiveTransportError::Cancelled)
    ));
    assert_eq!(server.request_count(), 0);
}
