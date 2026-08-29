use std::collections::VecDeque;
use std::io::Read;
use std::num::NonZeroUsize;
use std::sync::{Arc, Mutex};
use std::time::Duration;

use bytes::Bytes;
use media_core::{DemuxReadEvent, DemuxSeekResult, Demuxer, MediaTime, TrackInfo};
use source_core::{
    ByteSource, CancellationToken, NotSeekableReason, Seekability, SourceError, SourceFingerprint,
    SourceResult, SourceValidators,
};

use super::{
    DemuxContainerRegistration, DemuxFactory, DemuxFactoryDescriptor, DemuxFactoryOpenError,
    DemuxOpenError, DemuxOpenRequest, DemuxRegistry, DemuxRegistryError,
};
use crate::{
    DemuxContainerId, DemuxFactoryId, DemuxFixtureId, DemuxHintRelationship, DemuxHints,
    DemuxIdentityError, DemuxInput, DemuxInputCapabilities, DemuxInputCapability, DemuxMimeType,
    DemuxProbeConfidence, DemuxProbeDecision, DemuxProbeMatch, DemuxProbeRejection,
    DemuxProbeRequest, DemuxSniffBudget, DemuxSourceExtension, OrderedResourceMetadata,
    OrderedResourceReadError, OrderedResourceReadOutcome, OrderedResourceStreamSource,
    OrderedSegment, OrderedSegmentDiscontinuity, OrderedSegmentKind, OrderedSegmentReadError,
    OrderedSegmentSequence, OrderedSegmentSource,
};

/// Registration identities fail-closed до попадания неоднозначных значений в registry.
#[test]
fn registry_identity_boundary_rejects_empty_and_oversized_values() {
    assert!(matches!(
        DemuxContainerId::new(""),
        Err(DemuxIdentityError::Empty {
            kind: "demux container ID"
        })
    ));

    let oversized = "x".repeat(129);
    assert!(matches!(
        DemuxFixtureId::new(oversized),
        Err(DemuxIdentityError::TooLong {
            kind: "demux fixture ID",
            max_bytes: 128
        })
    ));
}

/// Public diagnostics сохраняют exact typed kind и canonical value без alias-нормализации.
#[test]
fn registry_identity_diagnostics_are_stable_for_every_dimension() {
    let identities = [
        (
            format!(
                "{:?}",
                DemuxFactoryId::new("first-party").expect("factory ID")
            ),
            "DemuxFactoryId(\"first-party\")",
        ),
        (
            format!(
                "{:?}",
                DemuxContainerId::new("mpeg-ts").expect("container ID")
            ),
            "DemuxContainerId(\"mpeg-ts\")",
        ),
        (
            format!(
                "{:?}",
                DemuxFixtureId::new("generated-fixture").expect("fixture ID")
            ),
            "DemuxFixtureId(\"generated-fixture\")",
        ),
        (
            format!(
                "{:?}",
                DemuxSourceExtension::new("m4s").expect("source extension")
            ),
            "DemuxSourceExtension(\"m4s\")",
        ),
        (
            format!("{:?}", DemuxMimeType::new("video/mp4").expect("MIME type")),
            "DemuxMimeType(\"video/mp4\")",
        ),
    ];

    for (actual, expected) in identities {
        assert_eq!(actual, expected);
    }
    let displayed_identities = [
        DemuxFactoryId::new("first-party")
            .expect("factory ID")
            .to_string(),
        DemuxContainerId::new("mpeg-ts")
            .expect("container ID")
            .to_string(),
        DemuxFixtureId::new("generated-fixture")
            .expect("fixture ID")
            .to_string(),
        DemuxSourceExtension::new("m4s")
            .expect("source extension")
            .to_string(),
        DemuxMimeType::new("video/mp4")
            .expect("MIME type")
            .to_string(),
    ];
    assert_eq!(
        displayed_identities,
        [
            "first-party",
            "mpeg-ts",
            "generated-fixture",
            "m4s",
            "video/mp4"
        ]
    );
}

/// Runtime input diagnostics показывают только shape/metadata и не раскрывают source internals.
#[test]
fn byte_source_input_and_complete_hints_keep_typed_diagnostics() {
    let hints = DemuxHints::none()
        .with_mime_type(DemuxMimeType::new("video/mp2t").expect("MIME hint"))
        .with_container(DemuxContainerId::new("mpeg-ts").expect("container hint"));
    assert_eq!(
        hints.mime_type.as_ref().map(DemuxMimeType::as_str),
        Some("video/mp2t")
    );
    assert_eq!(
        hints.container.as_ref().map(DemuxContainerId::as_str),
        Some("mpeg-ts")
    );

    let input = DemuxInput::byte_source(Box::new(MemoryByteSource::new(b"TEST-source", true)));
    let DemuxInput::ByteSource(source) = &input else {
        panic!("constructor должен сохранить byte-source shape");
    };
    let source_diagnostics = format!("{source:?}");
    assert!(source_diagnostics.contains("seekability: Seekable"));
    assert!(source_diagnostics.contains("content_length: Some(11)"));
    assert_eq!(
        format!("{input:?}"),
        "DemuxInput { capability: SeekableBytes, .. }"
    );
}

/// Fake runtime demuxer нужен только для проверки open composition.
struct EmptyDemuxer;

impl Demuxer for EmptyDemuxer {
    fn tracks(&self) -> &[TrackInfo] {
        &[]
    }

    fn duration(&self) -> Option<Duration> {
        None
    }

    fn next_event(&mut self) -> anyhow::Result<DemuxReadEvent> {
        Ok(DemuxReadEvent::EndOfStream)
    }

    fn seek(&mut self, timestamp: Duration) -> anyhow::Result<DemuxSeekResult> {
        Ok(DemuxSeekResult {
            requested_position: MediaTime::from_duration(timestamp),
            actual_position: MediaTime::from_duration(timestamp),
            actual_track_timestamp: None,
        })
    }
}

/// Factory распознаёт exact `TEST` signature и записывает replayed input.
struct RecordingFactory {
    descriptor: DemuxFactoryDescriptor,
    probed_container: DemuxContainerId,
    opened_bytes: Arc<Mutex<Vec<u8>>>,
    maximum_open_bytes: Option<usize>,
}

impl RecordingFactory {
    fn new(factory_id: &str, container_id: &str, opened_bytes: Arc<Mutex<Vec<u8>>>) -> Self {
        let input_capabilities = DemuxInputCapabilities::only(DemuxInputCapability::SeekableBytes)
            .with(DemuxInputCapability::StreamingBytes)
            .with(DemuxInputCapability::OrderedSegments)
            .with(DemuxInputCapability::OrderedResourceStream);
        let container = DemuxContainerRegistration::new(
            DemuxContainerId::new(container_id).expect("container ID"),
            input_capabilities,
            vec![DemuxSourceExtension::new("test").expect("extension")],
            vec![],
        );
        Self::with_registrations(factory_id, vec![container], container_id, opened_bytes)
    }

    /// Строит multi-container fake, где probe намеренно выбирает один exact row.
    fn with_registrations(
        factory_id: &str,
        registrations: Vec<DemuxContainerRegistration>,
        probed_container_id: &str,
        opened_bytes: Arc<Mutex<Vec<u8>>>,
    ) -> Self {
        Self {
            descriptor: DemuxFactoryDescriptor::new(
                DemuxFactoryId::new(factory_id).expect("factory ID"),
                registrations,
                vec![DemuxFixtureId::new("registry/test-signature").expect("fixture ID")],
            ),
            probed_container: DemuxContainerId::new(probed_container_id)
                .expect("probed container ID"),
            opened_bytes,
            maximum_open_bytes: None,
        }
    }

    /// Ограничивает factory open чтением prefix-а, моделируя ранний container open.
    fn with_maximum_open_bytes(mut self, maximum_open_bytes: usize) -> Self {
        self.maximum_open_bytes = Some(maximum_open_bytes);
        self
    }
}

impl DemuxFactory for RecordingFactory {
    fn descriptor(&self) -> &DemuxFactoryDescriptor {
        &self.descriptor
    }

    fn probe(&self, request: DemuxProbeRequest<'_>) -> DemuxProbeDecision {
        if request.cancellation.is_cancelled() {
            return DemuxProbeDecision::Rejected(DemuxProbeRejection::Cancelled);
        }
        let signature = b"TEST";
        if request.sniffed_bytes.len() < signature.len()
            && signature.starts_with(request.sniffed_bytes)
        {
            return DemuxProbeDecision::Rejected(DemuxProbeRejection::Truncated {
                available_bytes: request.sniffed_bytes.len(),
                required_bytes: signature.len(),
            });
        }
        if !request.sniffed_bytes.starts_with(signature) {
            return DemuxProbeDecision::NoMatch;
        }
        let container = self
            .descriptor
            .container_registration(&self.probed_container)
            .expect("fake probe container registration");
        DemuxProbeDecision::Match(DemuxProbeMatch {
            container: container.container.clone(),
            confidence: DemuxProbeConfidence::Signature,
            hint_relationship: container.hint_relationship(request.hints),
        })
    }

    fn open(
        &self,
        request: DemuxOpenRequest,
    ) -> Result<Box<dyn Demuxer + Send>, DemuxFactoryOpenError> {
        let mut opened_bytes = Vec::new();
        match request.input {
            DemuxInput::ByteSource(mut source) => {
                let mut buffer = [0_u8; 16];
                loop {
                    let bytes_read = source
                        .read(&mut buffer, &request.cancellation)
                        .map_err(|error| DemuxFactoryOpenError::Backend(error.into()))?;
                    if bytes_read == 0 {
                        break;
                    }
                    opened_bytes.extend_from_slice(&buffer[..bytes_read]);
                }
            }
            DemuxInput::ByteStream(mut reader) => {
                reader
                    .read_to_end(&mut opened_bytes)
                    .map_err(|error| DemuxFactoryOpenError::Backend(error.into()))?;
            }
            DemuxInput::OrderedSegments(mut source) => {
                while let Some(segment) = source
                    .next_segment(&request.cancellation)
                    .map_err(|error| DemuxFactoryOpenError::Backend(error.into()))?
                {
                    opened_bytes.extend_from_slice(&segment.bytes);
                }
            }
            DemuxInput::OrderedResourceStream(mut source) => loop {
                let outcome = source
                    .next_event(
                        NonZeroUsize::new(16).expect("non-zero factory read bound"),
                        &request.cancellation,
                    )
                    .map_err(|error| DemuxFactoryOpenError::Backend(error.into()))?;
                match outcome {
                    OrderedResourceReadOutcome::Begin(_)
                    | OrderedResourceReadOutcome::EndResource => {}
                    OrderedResourceReadOutcome::Data(bytes) => {
                        opened_bytes.extend_from_slice(&bytes);
                        if self
                            .maximum_open_bytes
                            .is_some_and(|limit| opened_bytes.len() >= limit)
                        {
                            break;
                        }
                    }
                    OrderedResourceReadOutcome::EndOfInput => break,
                }
            },
        }
        *self.opened_bytes.lock().expect("opened byte log") = opened_bytes;
        Ok(Box::new(EmptyDemuxer))
    }
}

/// In-memory source моделирует и seekable, и streaming `ByteSource` shape.
struct MemoryByteSource {
    bytes: Vec<u8>,
    position: u64,
    seekable: bool,
}

impl MemoryByteSource {
    fn new(bytes: &[u8], seekable: bool) -> Self {
        Self {
            bytes: bytes.to_vec(),
            position: 0,
            seekable,
        }
    }
}

impl ByteSource for MemoryByteSource {
    fn read(&mut self, output: &mut [u8], cancellation: &CancellationToken) -> SourceResult<usize> {
        if cancellation.is_cancelled() {
            return Err(SourceError::Cancelled);
        }
        let start = usize::try_from(self.position).unwrap_or(usize::MAX);
        if start >= self.bytes.len() {
            return Ok(0);
        }
        let bytes_to_copy = output.len().min(self.bytes.len() - start);
        output[..bytes_to_copy].copy_from_slice(&self.bytes[start..start + bytes_to_copy]);
        self.position += bytes_to_copy as u64;
        Ok(bytes_to_copy)
    }

    fn seek(&mut self, offset: u64) -> SourceResult<()> {
        if !self.seekable {
            return Err(SourceError::NotSeekable {
                reason: NotSeekableReason::Unknown,
            });
        }
        self.position = offset;
        Ok(())
    }

    fn position(&self) -> u64 {
        self.position
    }

    fn seekability(&self) -> Seekability {
        if self.seekable {
            Seekability::Seekable
        } else {
            Seekability::NotSeekable {
                reason: NotSeekableReason::Unknown,
            }
        }
    }

    fn validators(&self) -> SourceValidators {
        SourceValidators::default()
    }

    fn content_length(&self) -> Option<u64> {
        Some(self.bytes.len() as u64)
    }

    fn fingerprint(&self) -> SourceFingerprint {
        SourceFingerprint::new("registry-memory")
    }
}

/// Fake segment source сохраняет exact boundaries для replay verification.
struct MemorySegmentSource {
    segments: VecDeque<OrderedSegment>,
}

/// Fake pull source сохраняет реальные max-chunk requests и режет `Bytes` без копии.
struct MemoryResourceStream {
    events: VecDeque<OrderedResourceReadOutcome>,
    requested_chunk_bounds: Arc<Mutex<Vec<usize>>>,
}

impl OrderedResourceStreamSource for MemoryResourceStream {
    fn next_event(
        &mut self,
        maximum_chunk_bytes: NonZeroUsize,
        cancellation: &CancellationToken,
    ) -> Result<OrderedResourceReadOutcome, OrderedResourceReadError> {
        if cancellation.is_cancelled() {
            return Err(OrderedResourceReadError::Cancelled);
        }
        self.requested_chunk_bounds
            .lock()
            .expect("chunk bounds")
            .push(maximum_chunk_bytes.get());
        if let Some(OrderedResourceReadOutcome::Data(bytes)) = self.events.front_mut()
            && bytes.len() > maximum_chunk_bytes.get()
        {
            return Ok(OrderedResourceReadOutcome::Data(
                bytes.split_to(maximum_chunk_bytes.get()),
            ));
        }
        Ok(self
            .events
            .pop_front()
            .unwrap_or(OrderedResourceReadOutcome::EndOfInput))
    }
}

impl OrderedSegmentSource for MemorySegmentSource {
    fn next_segment(
        &mut self,
        cancellation: &CancellationToken,
    ) -> Result<Option<OrderedSegment>, OrderedSegmentReadError> {
        if cancellation.is_cancelled() {
            Err(OrderedSegmentReadError::Cancelled)
        } else {
            Ok(self.segments.pop_front())
        }
    }
}

fn sniff_budget() -> DemuxSniffBudget {
    DemuxSniffBudget::new(
        NonZeroUsize::new(8).expect("non-zero bytes"),
        NonZeroUsize::new(2).expect("non-zero segments"),
        Duration::from_secs(1),
    )
    .expect("valid sniff budget")
}

fn registry_with_recording_factory(opened_bytes: Arc<Mutex<Vec<u8>>>) -> DemuxRegistry {
    let mut registry = DemuxRegistry::new();
    registry
        .register(Box::new(RecordingFactory::new(
            "recording",
            "test-container",
            opened_bytes,
        )))
        .expect("recording factory registration");
    registry
}

/// Content остаётся authoritative и явно показывает agree/disagree hints.
#[test]
fn hint_and_sniff_agreement_is_typed_without_overriding_content() {
    let registry = registry_with_recording_factory(Arc::new(Mutex::new(Vec::new())));
    let agreeing_hints =
        DemuxHints::none().with_extension(DemuxSourceExtension::new("test").expect("extension"));
    let agreeing = registry
        .probe_sample(
            DemuxInputCapability::StreamingBytes,
            &agreeing_hints,
            b"TESTpayload",
            &CancellationToken::never_cancelled(),
        )
        .expect("content match");
    assert_eq!(
        agreeing.matched.hint_relationship,
        DemuxHintRelationship::Agrees
    );

    let disagreeing_hints =
        DemuxHints::none().with_extension(DemuxSourceExtension::new("wrong").expect("extension"));
    let disagreeing = registry
        .probe_sample(
            DemuxInputCapability::StreamingBytes,
            &disagreeing_hints,
            b"TESTpayload",
            &CancellationToken::never_cancelled(),
        )
        .expect("content still wins");
    assert_eq!(
        disagreeing.matched.hint_relationship,
        DemuxHintRelationship::Disagrees
    );
}

/// Duplicate factory и canonical container ownership отклоняются до probing.
#[test]
fn duplicate_registration_is_rejected() {
    let opened_bytes = Arc::new(Mutex::new(Vec::new()));
    let mut registry = registry_with_recording_factory(Arc::clone(&opened_bytes));
    let duplicate_factory = registry
        .register(Box::new(RecordingFactory::new(
            "recording",
            "other-container",
            Arc::clone(&opened_bytes),
        )))
        .expect_err("duplicate factory ID");
    assert!(matches!(
        duplicate_factory,
        DemuxRegistryError::DuplicateFactory { .. }
    ));

    let duplicate_container = registry
        .register(Box::new(RecordingFactory::new(
            "other-factory",
            "test-container",
            opened_bytes,
        )))
        .expect_err("duplicate container owner");
    assert!(matches!(
        duplicate_container,
        DemuxRegistryError::DuplicateContainer { .. }
    ));

    let repeated_container_id =
        DemuxContainerId::new("repeated-container").expect("repeated container ID");
    let repeated_capabilities = DemuxInputCapabilities::only(DemuxInputCapability::StreamingBytes);
    let repeated_rows = vec![
        DemuxContainerRegistration::new(
            repeated_container_id.clone(),
            repeated_capabilities,
            vec![],
            vec![],
        ),
        DemuxContainerRegistration::new(
            repeated_container_id,
            repeated_capabilities,
            vec![],
            vec![],
        ),
    ];
    let duplicate_inside_factory = DemuxRegistry::new()
        .register(Box::new(RecordingFactory::with_registrations(
            "internally-duplicated",
            repeated_rows,
            "repeated-container",
            Arc::new(Mutex::new(Vec::new())),
        )))
        .expect_err("duplicate container row внутри одного factory");
    assert!(matches!(
        duplicate_inside_factory,
        DemuxRegistryError::DuplicateContainer { .. }
    ));
}

/// Capability соседнего container row не расширяет contract фактически matched row.
#[test]
fn registry_validates_the_exact_container_and_input_pair() {
    let seekable_only = DemuxInputCapabilities::only(DemuxInputCapability::SeekableBytes);
    let ordered_only = DemuxInputCapabilities::only(DemuxInputCapability::OrderedSegments);
    let seekable_container = DemuxContainerRegistration::new(
        DemuxContainerId::new("seekable-container").expect("seekable container ID"),
        seekable_only,
        vec![],
        vec![],
    );
    let ordered_container = DemuxContainerRegistration::new(
        DemuxContainerId::new("ordered-container").expect("ordered container ID"),
        ordered_only,
        vec![],
        vec![],
    );
    let factory = RecordingFactory::with_registrations(
        "per-container",
        vec![seekable_container, ordered_container],
        "seekable-container",
        Arc::new(Mutex::new(Vec::new())),
    );
    assert!(
        factory
            .descriptor()
            .input_capabilities()
            .contains(DemuxInputCapability::OrderedSegments),
        "aggregate нужен только для factory prefilter"
    );
    assert!(
        !factory.descriptor().containers[0].supports_input(DemuxInputCapability::OrderedSegments),
        "neighbor capability не должна протечь в matched row"
    );

    let mut registry = DemuxRegistry::new();
    registry
        .register(Box::new(factory))
        .expect("per-container factory registration");
    registry
        .probe_sample(
            DemuxInputCapability::SeekableBytes,
            &DemuxHints::none(),
            b"TEST",
            &CancellationToken::never_cancelled(),
        )
        .expect("matched container поддерживает seekable input");

    let rejected = registry
        .probe_sample(
            DemuxInputCapability::OrderedSegments,
            &DemuxHints::none(),
            b"TEST",
            &CancellationToken::never_cancelled(),
        )
        .expect_err("matched container не поддерживает ordered input");
    assert!(matches!(
        rejected,
        DemuxOpenError::ProbeRejected(DemuxProbeRejection::UnsupportedInput {
            capability: DemuxInputCapability::OrderedSegments,
        })
    ));
}

#[test]
fn required_container_rejects_a_different_proven_signature_before_factory_open() {
    let opened_bytes = Arc::new(Mutex::new(Vec::new()));
    let registry = registry_with_recording_factory(Arc::clone(&opened_bytes));
    let error = registry
        .open_required_container(
            DemuxInput::byte_source(Box::new(MemoryByteSource::new(b"TEST-payload", true))),
            DemuxHints::none(),
            sniff_budget(),
            CancellationToken::never_cancelled(),
            DemuxContainerId::new("different-container").expect("required container"),
        )
        .err()
        .unwrap_or_else(|| panic!("required container must be content-proven"));
    assert!(matches!(error, DemuxOpenError::UnexpectedContainer { .. }));
    assert!(opened_bytes.lock().expect("opened bytes mutex").is_empty());
}

/// Empty capability отклоняется на exact container row с понятной диагностикой.
#[test]
fn registration_rejects_a_container_without_input_capabilities() {
    let empty_container = DemuxContainerRegistration::new(
        DemuxContainerId::new("empty-container").expect("empty container ID"),
        DemuxInputCapabilities::default(),
        vec![],
        vec![],
    );
    let factory = RecordingFactory::with_registrations(
        "empty-capabilities",
        vec![empty_container],
        "empty-container",
        Arc::new(Mutex::new(Vec::new())),
    );
    let mut registry = DemuxRegistry::new();
    let error = registry
        .register(Box::new(factory))
        .expect_err("empty container capabilities");
    assert!(matches!(
        error,
        DemuxRegistryError::MissingInputCapabilities { container, .. }
            if container.as_str() == "empty-container"
    ));
}

/// Per-container filtering не меняет прежний tie contract разных owners.
#[test]
fn equally_strong_matches_remain_ambiguous() {
    let opened_bytes = Arc::new(Mutex::new(Vec::new()));
    let mut registry = DemuxRegistry::new();
    registry
        .register(Box::new(RecordingFactory::new(
            "first-factory",
            "first-container",
            Arc::clone(&opened_bytes),
        )))
        .expect("first factory");
    registry
        .register(Box::new(RecordingFactory::new(
            "second-factory",
            "second-container",
            opened_bytes,
        )))
        .expect("second factory");

    let error = registry
        .probe_sample(
            DemuxInputCapability::StreamingBytes,
            &DemuxHints::none(),
            b"TEST",
            &CancellationToken::never_cancelled(),
        )
        .expect_err("equal signature matches must stay ambiguous");
    assert!(matches!(error, DemuxOpenError::AmbiguousMatch { .. }));
}

/// Seekable source cursor возвращается к исходной позиции после bounded sniff.
#[test]
fn seekable_byte_input_is_replayed_from_original_cursor() {
    let opened_bytes = Arc::new(Mutex::new(Vec::new()));
    let registry = registry_with_recording_factory(Arc::clone(&opened_bytes));
    let input = DemuxInput::byte_source(Box::new(MemoryByteSource::new(b"TESTseekable", true)));
    registry
        .open(
            input,
            DemuxHints::none(),
            sniff_budget(),
            CancellationToken::never_cancelled(),
        )
        .expect("seekable open");
    assert_eq!(
        &*opened_bytes.lock().expect("opened bytes"),
        b"TESTseekable"
    );
}

/// Non-seekable byte source и plain stream получают bounded prefix replay.
#[test]
fn streaming_byte_inputs_replay_sniffed_prefix() {
    for input in [
        DemuxInput::byte_source(Box::new(MemoryByteSource::new(b"TESTsource-stream", false))),
        DemuxInput::byte_stream(Box::new(std::io::Cursor::new(b"TESTplain-stream".to_vec()))),
    ] {
        let opened_bytes = Arc::new(Mutex::new(Vec::new()));
        let registry = registry_with_recording_factory(Arc::clone(&opened_bytes));
        registry
            .open(
                input,
                DemuxHints::none(),
                sniff_budget(),
                CancellationToken::never_cancelled(),
            )
            .expect("streaming open");
        assert!(
            opened_bytes
                .lock()
                .expect("opened bytes")
                .starts_with(b"TEST")
        );
    }
}

/// Ordered segment sniff сохраняет init/media bytes и boundaries для factory open.
#[test]
fn ordered_segment_input_replays_consumed_segments() {
    let opened_bytes = Arc::new(Mutex::new(Vec::new()));
    let registry = registry_with_recording_factory(Arc::clone(&opened_bytes));
    let segments = VecDeque::from([
        OrderedSegment {
            sequence: OrderedSegmentSequence::new(0),
            kind: OrderedSegmentKind::Initialization,
            discontinuity: OrderedSegmentDiscontinuity::Continuous,
            bytes: Bytes::from_static(b"TEST"),
        },
        OrderedSegment {
            sequence: OrderedSegmentSequence::new(1),
            kind: OrderedSegmentKind::Media,
            discontinuity: OrderedSegmentDiscontinuity::Continuous,
            bytes: Bytes::from_static(b"media"),
        },
    ]);
    registry
        .open(
            DemuxInput::ordered_segments(Box::new(MemorySegmentSource { segments })),
            DemuxHints::none(),
            sniff_budget(),
            CancellationToken::never_cancelled(),
        )
        .expect("segmented open");
    assert_eq!(&*opened_bytes.lock().expect("opened bytes"), b"TESTmedia");
}

/// Segment resource может быть больше sniff prefix, но factory обязана получить его целиком после probe.
#[test]
fn ordered_segment_larger_than_sniff_prefix_is_replayed_without_truncation() {
    let opened_bytes = Arc::new(Mutex::new(Vec::new()));
    let registry = registry_with_recording_factory(Arc::clone(&opened_bytes));
    let oversized_segment = Bytes::from_static(b"TEST-segment-larger-than-eight-byte-sniff-budget");
    let segments = VecDeque::from([OrderedSegment {
        sequence: OrderedSegmentSequence::new(0),
        kind: OrderedSegmentKind::Media,
        discontinuity: OrderedSegmentDiscontinuity::Continuous,
        bytes: oversized_segment.clone(),
    }]);

    registry
        .open(
            DemuxInput::ordered_segments(Box::new(MemorySegmentSource { segments })),
            DemuxHints::none(),
            sniff_budget(),
            CancellationToken::never_cancelled(),
        )
        .expect("oversized segmented resource must open from bounded prefix");

    assert_eq!(
        &*opened_bytes.lock().expect("opened bytes"),
        oversized_segment.as_ref()
    );
}

/// Registry sniff читает только bounded body prefix и replay-ит его в тот же resource.
#[test]
fn ordered_resource_stream_replays_prefix_without_full_body_materialization() {
    let opened_bytes = Arc::new(Mutex::new(Vec::new()));
    let requested_chunk_bounds = Arc::new(Mutex::new(Vec::new()));
    let registry = registry_with_recording_factory(Arc::clone(&opened_bytes));
    let resource_bytes = Bytes::from_static(b"TEST-streamed-resource-body-beyond-sniff-prefix");
    let events = VecDeque::from([
        OrderedResourceReadOutcome::Begin(OrderedResourceMetadata {
            sequence: OrderedSegmentSequence::new(7),
            kind: OrderedSegmentKind::Media,
            discontinuity: OrderedSegmentDiscontinuity::StartsNewTimeline,
        }),
        OrderedResourceReadOutcome::Data(resource_bytes.clone()),
        OrderedResourceReadOutcome::EndResource,
        OrderedResourceReadOutcome::EndOfInput,
    ]);

    registry
        .open(
            DemuxInput::ordered_resource_stream(Box::new(MemoryResourceStream {
                events,
                requested_chunk_bounds: Arc::clone(&requested_chunk_bounds),
            })),
            DemuxHints::none(),
            sniff_budget(),
            CancellationToken::never_cancelled(),
        )
        .expect("streamed resource must open from replayed prefix");

    assert_eq!(
        &*opened_bytes.lock().expect("opened bytes"),
        resource_bytes.as_ref()
    );
    let bounds = requested_chunk_bounds.lock().expect("chunk bounds");
    assert_eq!(&bounds[..2], &[8, 8]);
    assert!(bounds.iter().all(|bound| *bound <= 16));
}

/// Winning factory может открыться по sniff prefix, не вытягивая body до EndResource.
#[test]
fn ordered_resource_stream_factory_open_does_not_wait_for_body_eof() {
    let opened_bytes = Arc::new(Mutex::new(Vec::new()));
    let requested_chunk_bounds = Arc::new(Mutex::new(Vec::new()));
    let mut registry = DemuxRegistry::new();
    registry
        .register(Box::new(
            RecordingFactory::new(
                "prefix-recording",
                "test-container",
                Arc::clone(&opened_bytes),
            )
            .with_maximum_open_bytes(8),
        ))
        .expect("prefix recording factory");
    let events = VecDeque::from([
        OrderedResourceReadOutcome::Begin(OrderedResourceMetadata {
            sequence: OrderedSegmentSequence::new(0),
            kind: OrderedSegmentKind::Media,
            discontinuity: OrderedSegmentDiscontinuity::Continuous,
        }),
        OrderedResourceReadOutcome::Data(Bytes::from_static(
            b"TEST-body-that-must-remain-unpulled-during-open",
        )),
        OrderedResourceReadOutcome::EndResource,
        OrderedResourceReadOutcome::EndOfInput,
    ]);

    registry
        .open(
            DemuxInput::ordered_resource_stream(Box::new(MemoryResourceStream {
                events,
                requested_chunk_bounds: Arc::clone(&requested_chunk_bounds),
            })),
            DemuxHints::none(),
            sniff_budget(),
            CancellationToken::never_cancelled(),
        )
        .expect("prefix-only factory open");

    assert_eq!(&*opened_bytes.lock().expect("opened bytes"), b"TEST-bod");
    assert_eq!(
        &*requested_chunk_bounds.lock().expect("chunk bounds"),
        &[8, 8],
        "factory должен получить registry replay без следующего source pull"
    );
}

/// Пустой body chunk является protocol failure, а не временным EOF или spin.
#[test]
fn ordered_resource_stream_rejects_empty_data_during_sniff() {
    let registry = registry_with_recording_factory(Arc::new(Mutex::new(Vec::new())));
    let error = match registry.open(
        DemuxInput::ordered_resource_stream(Box::new(MemoryResourceStream {
            events: VecDeque::from([
                OrderedResourceReadOutcome::Begin(OrderedResourceMetadata {
                    sequence: OrderedSegmentSequence::new(0),
                    kind: OrderedSegmentKind::Media,
                    discontinuity: OrderedSegmentDiscontinuity::Continuous,
                }),
                OrderedResourceReadOutcome::Data(Bytes::new()),
            ]),
            requested_chunk_bounds: Arc::new(Mutex::new(Vec::new())),
        })),
        DemuxHints::none(),
        sniff_budget(),
        CancellationToken::never_cancelled(),
    ) {
        Ok(_) => panic!("empty Data must fail before factory selection"),
        Err(error) => error,
    };

    assert!(matches!(
        error,
        DemuxOpenError::ProbeRejected(DemuxProbeRejection::InputFailure { .. })
    ));
}

/// Truncation, cancellation и no-match сохраняют разные terminal outcomes.
#[test]
fn truncated_cancelled_and_no_match_are_distinct() {
    let registry = registry_with_recording_factory(Arc::new(Mutex::new(Vec::new())));
    let truncated = registry
        .probe_sample(
            DemuxInputCapability::StreamingBytes,
            &DemuxHints::none(),
            b"TE",
            &CancellationToken::never_cancelled(),
        )
        .expect_err("truncated signature");
    assert!(matches!(
        truncated,
        DemuxOpenError::ProbeRejected(DemuxProbeRejection::Truncated { .. })
    ));

    let no_match = registry
        .probe_sample(
            DemuxInputCapability::StreamingBytes,
            &DemuxHints::none(),
            b"NOPE",
            &CancellationToken::never_cancelled(),
        )
        .expect_err("no match");
    assert!(matches!(no_match, DemuxOpenError::NoMatch));

    let cancellation = CancellationToken::new();
    cancellation.cancel();
    let cancelled = registry
        .probe_sample(
            DemuxInputCapability::StreamingBytes,
            &DemuxHints::none(),
            b"TEST",
            &cancellation,
        )
        .expect_err("cancelled probe");
    assert!(matches!(
        cancelled,
        DemuxOpenError::ProbeRejected(DemuxProbeRejection::Cancelled)
    ));
}
