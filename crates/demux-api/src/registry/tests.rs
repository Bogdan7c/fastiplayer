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
    DemuxInput, DemuxInputCapabilities, DemuxInputCapability, DemuxProbeConfidence,
    DemuxProbeDecision, DemuxProbeMatch, DemuxProbeRejection, DemuxProbeRequest, DemuxSniffBudget,
    DemuxSourceExtension, OrderedSegment, OrderedSegmentKind, OrderedSegmentReadError,
    OrderedSegmentSequence, OrderedSegmentSource,
};

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
    opened_bytes: Arc<Mutex<Vec<u8>>>,
}

impl RecordingFactory {
    fn new(factory_id: &str, container_id: &str, opened_bytes: Arc<Mutex<Vec<u8>>>) -> Self {
        let container = DemuxContainerRegistration::new(
            DemuxContainerId::new(container_id).expect("container ID"),
            vec![DemuxSourceExtension::new("test").expect("extension")],
            vec![],
        );
        Self {
            descriptor: DemuxFactoryDescriptor::new(
                DemuxFactoryId::new(factory_id).expect("factory ID"),
                DemuxInputCapabilities::only(DemuxInputCapability::SeekableBytes)
                    .with(DemuxInputCapability::StreamingBytes)
                    .with(DemuxInputCapability::OrderedSegments),
                vec![container],
                vec![DemuxFixtureId::new("registry/test-signature").expect("fixture ID")],
            ),
            opened_bytes,
        }
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
        let container = &self.descriptor.containers[0];
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
            bytes: Bytes::from_static(b"TEST"),
        },
        OrderedSegment {
            sequence: OrderedSegmentSequence::new(1),
            kind: OrderedSegmentKind::Media,
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
