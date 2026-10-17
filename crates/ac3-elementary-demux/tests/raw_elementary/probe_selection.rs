//! Распознавание по содержимому и выбор factory в registry.

use ac3_elementary_demux::{
    AC3_ELEMENTARY_CONTAINER_ID, Ac3ElementaryDemuxFactory, Ac3ElementaryDemuxOptions,
};
use demux_api::{
    DemuxFactory, DemuxHintRelationship, DemuxHints, DemuxInputCapability, DemuxMimeType,
    DemuxOpenError, DemuxProbeConfidence, DemuxProbeDecision, DemuxProbeRejection,
    DemuxProbeRequest, DemuxSourceExtension,
};
use source_core::CancellationToken;

use crate::support::{
    FIXTURE_FRAMES, STEREO_AC3, SURROUND_EAC3, drain_packets, open_through_registry, seekable_input,
};

fn factory() -> Ac3ElementaryDemuxFactory {
    Ac3ElementaryDemuxFactory::new(Ac3ElementaryDemuxOptions::default()).expect("factory")
}

fn probe(
    bytes: &[u8],
    extension: Option<&str>,
    cancellation: &CancellationToken,
) -> DemuxProbeDecision {
    let hints = extension.map_or_else(DemuxHints::none, |extension| {
        DemuxHints::none().with_extension(DemuxSourceExtension::new(extension).expect("ext"))
    });
    factory().probe(DemuxProbeRequest {
        hints: &hints,
        sniffed_bytes: bytes,
        input_capability: DemuxInputCapability::SeekableBytes,
        cancellation,
    })
}

fn confidence(
    decision: &DemuxProbeDecision,
) -> Option<(DemuxProbeConfidence, DemuxHintRelationship)> {
    match decision {
        DemuxProbeDecision::Match(matched) => {
            assert_eq!(matched.container.as_str(), AC3_ELEMENTARY_CONTAINER_ID);
            Some((matched.confidence, matched.hint_relationship))
        }
        _ => None,
    }
}

#[test]
fn content_at_start_is_exact_with_any_or_no_extension() {
    let never = CancellationToken::never_cancelled();
    for stream in [STEREO_AC3, SURROUND_EAC3] {
        assert_eq!(
            confidence(&probe(stream, None, &never)),
            Some((DemuxProbeConfidence::Exact, DemuxHintRelationship::Absent))
        );
        for extension in ["ac3", "eac3", "ec3"] {
            assert_eq!(
                confidence(&probe(stream, Some(extension), &never)),
                Some((DemuxProbeConfidence::Exact, DemuxHintRelationship::Agrees))
            );
        }
        // Неправильное расширение не мешает содержимому.
        assert_eq!(
            confidence(&probe(stream, Some("mp3"), &never)),
            Some((
                DemuxProbeConfidence::Exact,
                DemuxHintRelationship::Disagrees
            ))
        );
    }
}

/// Мусор впереди — только с подтверждающим расширением и самой слабой уверенностью.
#[test]
fn garbage_prefix_needs_agreeing_extension() {
    let never = CancellationToken::never_cancelled();
    let mut bytes = vec![0x00_u8; 700];
    bytes.extend_from_slice(STEREO_AC3);
    assert_eq!(
        confidence(&probe(&bytes, Some("ac3"), &never)),
        Some((
            DemuxProbeConfidence::HintOnly,
            DemuxHintRelationship::Agrees
        ))
    );
    assert_eq!(probe(&bytes, None, &never), DemuxProbeDecision::NoMatch);
    assert_eq!(
        probe(&bytes, Some("bin"), &never),
        DemuxProbeDecision::NoMatch
    );
}

/// Мусор впереди + MIME сервера: все три Dolby MIME подтверждают AC-3, чужой — нет.
#[test]
fn garbage_prefix_is_confirmed_by_every_dolby_mime_type() {
    let never = CancellationToken::never_cancelled();
    let mut bytes = vec![0x00_u8; 700];
    bytes.extend_from_slice(STEREO_AC3);
    let probe_with_mime = |mime: &str| {
        let hints =
            DemuxHints::none().with_mime_type(DemuxMimeType::new(mime).expect("MIME identity"));
        factory().probe(DemuxProbeRequest {
            hints: &hints,
            sniffed_bytes: &bytes,
            input_capability: DemuxInputCapability::StreamingBytes,
            cancellation: &never,
        })
    };
    for mime in ["audio/ac3", "audio/eac3", "audio/vnd.dolby.dd-raw"] {
        assert_eq!(
            confidence(&probe_with_mime(mime)),
            Some((
                DemuxProbeConfidence::HintOnly,
                DemuxHintRelationship::Agrees
            )),
            "{mime}"
        );
    }
    assert_eq!(probe_with_mime("audio/mpeg"), DemuxProbeDecision::NoMatch);
}

#[test]
fn foreign_content_and_cancellation_are_not_matches() {
    let never = CancellationToken::never_cancelled();
    assert_eq!(
        probe(b"OggS\0\x02\0\0", Some("ac3"), &never),
        DemuxProbeDecision::NoMatch
    );
    let cancelled = CancellationToken::new();
    cancelled.cancel();
    assert_eq!(
        probe(STEREO_AC3, None, &cancelled),
        DemuxProbeDecision::Rejected(DemuxProbeRejection::Cancelled)
    );
}

#[test]
fn descriptor_registers_extensions_and_byte_inputs_only() {
    let factory = factory();
    let descriptor = factory.descriptor();
    let [registration] = descriptor.containers.as_slice() else {
        panic!("one registration");
    };
    let extensions: Vec<&str> = registration
        .extensions
        .iter()
        .map(DemuxSourceExtension::as_str)
        .collect();
    assert_eq!(extensions, ["ac3", "eac3", "ec3"]);
    assert!(registration.supports_input(DemuxInputCapability::SeekableBytes));
    assert!(registration.supports_input(DemuxInputCapability::StreamingBytes));
    assert!(!registration.supports_input(DemuxInputCapability::OrderedSegments));
}

/// Registry открывает поток без расширения и отдаёт все пакеты.
#[test]
fn registry_opens_extensionless_stream_by_content() {
    let mut demuxer = open_through_registry(seekable_input(SURROUND_EAC3), None).expect("open");
    assert_eq!(demuxer.tracks()[0].codec_id, "A_EAC3");
    assert_eq!(drain_packets(demuxer.as_mut()).len(), FIXTURE_FRAMES);
}

#[test]
fn registry_rejects_non_ac3_content_as_no_match() {
    let error = open_through_registry(seekable_input(&[0x47_u8; 4096]), Some("ac3"))
        .err()
        .expect("must not open");
    assert!(matches!(error, DemuxOpenError::NoMatch), "{error:?}");
}
