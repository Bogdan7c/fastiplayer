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
        NonZeroUsize::new(4).expect("non-zero latency page"),
        NonZeroUsize::new(2).expect("cached pages"),
        AdaptiveResourceQueryApplication::ApplyScopedReplacement,
    )
    .expect("valid Range page policy")
}

/// Latency-first страница не может незаметно превратить bounded read в более крупный запрос.
#[test]
fn range_source_rejects_latency_page_larger_than_throughput_page() {
    let error = AdaptiveRangeSourceConfig::new(
        NonZeroUsize::new(2).expect("non-zero throughput page"),
        NonZeroUsize::new(4).expect("non-zero latency page"),
        NonZeroUsize::new(2).expect("non-zero cached pages"),
        AdaptiveResourceQueryApplication::ApplyScopedReplacement,
    )
    .expect_err("latency page larger than the maximum must be rejected");

    assert!(matches!(
        error,
        SourceError::InvalidConfig {
            field: "adaptive_range_source.latency_first_read_bytes",
            ..
        }
    ));
}

/// Последовательные container reads и обратный seek внутри страницы не должны платить новый RTT.
#[test]
fn range_source_reuses_bounded_read_ahead_for_packets_and_nearby_seek() {
    let resource = b"abcdefgh";
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
            "bytes=0-1" => (0, 1),
            "bytes=0-3" => (0, 3),
            "bytes=6-7" => (6, 7),
            unexpected => panic!("unexpected Range {unexpected}"),
        };
        response(
            "206 Partial Content",
            &[
                ("Content-Range", format!("bytes {start}-{end}/8")),
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
        AdaptiveRangeSourceConfig::new(
            NonZeroUsize::new(4).expect("throughput page"),
            NonZeroUsize::new(2).expect("latency page"),
            NonZeroUsize::new(2).expect("cached pages"),
            AdaptiveResourceQueryApplication::ApplyScopedReplacement,
        )
        .expect("valid two-stage Range page policy"),
    )
    .expect("Range source opens");

    let mut first_packet = [0_u8; 2];
    assert_eq!(
        source
            .read(&mut first_packet, &cancellation)
            .expect("first packet"),
        2
    );
    assert_eq!(&first_packet, b"ab");

    let mut second_packet = [0_u8; 2];
    assert_eq!(
        source
            .read(&mut second_packet, &cancellation)
            .expect("second packet from read-ahead"),
        2
    );
    assert_eq!(&second_packet, b"cd");
    assert_eq!(server.request_count(), 3);

    source.seek(2).expect("seek inside cached page");
    let mut nearby_seek_packet = [0_u8; 2];
    assert_eq!(
        source
            .read(&mut nearby_seek_packet, &cancellation)
            .expect("nearby seek packet from read-ahead"),
        2
    );
    assert_eq!(&nearby_seek_packet, b"cd");
    assert_eq!(server.request_count(), 3);

    source.seek(6).expect("seek outside cached page");
    let mut next_page_packet = [0_u8; 2];
    assert_eq!(
        source
            .read(&mut next_page_packet, &cancellation)
            .expect("packet from next page"),
        2
    );
    assert_eq!(&next_page_packet, b"gh");
    assert_eq!(server.request_count(), 4);
}

/// Fresh transactional replacement переиспользует только полностью завершённые VOD Range pages.
#[test]
fn fresh_range_source_replays_completed_probe_and_page_without_network() {
    let resource = b"abcdefgh";
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
            unexpected => panic!("unexpected Range {unexpected}"),
        };
        response(
            "206 Partial Content",
            &[
                ("Content-Range", format!("bytes {start}-{end}/8")),
                ("ETag", "\"stable\"".to_owned()),
            ],
            &resource[start..=end],
        )
    });
    let target = server.target("/representation.bin");
    let cancellation = CancellationToken::new();
    let shared_context = context(&target, cancellation.clone(), redirect_policy(), None, None);

    let mut first_source = AdaptiveRangeByteSource::open(
        shared_context.clone(),
        target.clone(),
        SourceGeneration::new(1),
        range_config(),
    )
    .expect("first Range source opens");
    let mut first_page = [0_u8; 4];
    assert_eq!(
        first_source
            .read(&mut first_page, &cancellation)
            .expect("first source page"),
        4
    );
    assert_eq!(&first_page, b"abcd");
    assert_eq!(server.request_count(), 2);

    let mut replacement_source = AdaptiveRangeByteSource::open(
        shared_context,
        target,
        SourceGeneration::new(1),
        range_config(),
    )
    .expect("replacement Range source opens from completed probe replay");
    let mut replayed_page = [0_u8; 4];
    assert_eq!(
        replacement_source
            .read(&mut replayed_page, &cancellation)
            .expect("replacement page replay"),
        4
    );
    assert_eq!(&replayed_page, b"abcd");
    assert_eq!(server.request_count(), 2);
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
        let (content_range, etag, body) = if index == 0 {
            ("bytes 0-0/5", "\"v1\"", &b"x"[..])
        } else {
            ("bytes 0-3/6", "\"v2\"", &b"xxxx"[..])
        };
        response(
            "206 Partial Content",
            &[
                ("Content-Range", content_range.to_owned()),
                ("ETag", etag.to_owned()),
            ],
            body,
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
