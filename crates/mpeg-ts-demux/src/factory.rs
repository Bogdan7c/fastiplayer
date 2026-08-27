use demux_api::{
    DemuxContainerId, DemuxContainerRegistration, DemuxFactory, DemuxFactoryDescriptor,
    DemuxFactoryId, DemuxFactoryOpenError, DemuxFixtureId, DemuxInputCapabilities,
    DemuxInputCapability, DemuxMimeType, DemuxOpenRequest, DemuxProbeConfidence,
    DemuxProbeDecision, DemuxProbeMatch, DemuxProbeRejection, DemuxProbeRequest,
    DemuxSourceExtension,
};
use media_core::Demuxer;

use crate::{MpegTsDemuxOptions, MpegTsDemuxer};

/// Registry adapter для first-party MPEG-TS implementation.
pub struct MpegTsDemuxFactory {
    descriptor: DemuxFactoryDescriptor,
    options: MpegTsDemuxOptions,
}

impl MpegTsDemuxFactory {
    /// Создаёт factory с caller-owned bounded parser policy.
    pub fn new(options: MpegTsDemuxOptions) -> Result<Self, demux_api::DemuxIdentityError> {
        let input_capabilities = DemuxInputCapabilities::only(DemuxInputCapability::SeekableBytes)
            .with(DemuxInputCapability::StreamingBytes)
            .with(DemuxInputCapability::OrderedSegments)
            .with(DemuxInputCapability::OrderedResourceStream);
        let registration = DemuxContainerRegistration::new(
            DemuxContainerId::new("mpeg-ts")?,
            input_capabilities,
            vec![DemuxSourceExtension::new("ts")?],
            vec![DemuxMimeType::new("video/mp2t")?],
        );
        let descriptor = DemuxFactoryDescriptor::new(
            DemuxFactoryId::new("mpeg-ts-first-party")?,
            vec![registration],
            vec![
                DemuxFixtureId::new("generated-ts-h264-aac")?,
                DemuxFixtureId::new("generated-ts-audio-only")?,
                DemuxFixtureId::new("generated-ts-corruption-rollover-seek")?,
            ],
        );
        Ok(Self {
            descriptor,
            options,
        })
    }

    fn registration(&self) -> &DemuxContainerRegistration {
        &self.descriptor.containers[0]
    }
}

impl DemuxFactory for MpegTsDemuxFactory {
    fn descriptor(&self) -> &DemuxFactoryDescriptor {
        &self.descriptor
    }

    fn probe(&self, request: DemuxProbeRequest<'_>) -> DemuxProbeDecision {
        if request.cancellation.is_cancelled() {
            return DemuxProbeDecision::Rejected(DemuxProbeRejection::Cancelled);
        }
        if !self.registration().supports_input(request.input_capability) {
            return DemuxProbeDecision::Rejected(DemuxProbeRejection::UnsupportedInput {
                capability: request.input_capability,
            });
        }
        if has_m2ts_signature(request.sniffed_bytes) {
            return DemuxProbeDecision::Rejected(DemuxProbeRejection::Malformed {
                reason: "192-byte M2TS не входит в подтверждённый S29 profile".to_owned(),
            });
        }
        match find_188_sync(request.sniffed_bytes, self.options.resync_bytes.get()) {
            Some(_) => DemuxProbeDecision::Match(DemuxProbeMatch {
                container: self.registration().container.clone(),
                confidence: DemuxProbeConfidence::Signature,
                hint_relationship: self.registration().hint_relationship(request.hints),
            }),
            None if request.sniffed_bytes.first() == Some(&0x47)
                && request.sniffed_bytes.len() < 188 * 3 =>
            {
                DemuxProbeDecision::Rejected(DemuxProbeRejection::Truncated {
                    available_bytes: request.sniffed_bytes.len(),
                    required_bytes: 188 * 3,
                })
            }
            None => DemuxProbeDecision::NoMatch,
        }
    }

    fn open(
        &self,
        request: DemuxOpenRequest,
    ) -> Result<Box<dyn Demuxer + Send>, DemuxFactoryOpenError> {
        if request.cancellation.is_cancelled() {
            return Err(DemuxFactoryOpenError::Cancelled);
        }
        MpegTsDemuxer::open(request.input, request.cancellation, self.options)
            .map(|demuxer| Box::new(demuxer) as Box<dyn Demuxer + Send>)
            .map_err(|error| match error {
                crate::MpegTsDemuxError::Cancelled => DemuxFactoryOpenError::Cancelled,
                other => DemuxFactoryOpenError::Backend(other.into()),
            })
    }
}

fn find_188_sync(bytes: &[u8], resync_bound: usize) -> Option<usize> {
    let maximum_start = bytes.len().saturating_sub(188 * 2 + 1).min(resync_bound);
    (0..=maximum_start).find(|start| {
        bytes.get(*start) == Some(&0x47)
            && bytes.get(start + 188) == Some(&0x47)
            && bytes.get(start + 188 * 2) == Some(&0x47)
    })
}

fn has_m2ts_signature(bytes: &[u8]) -> bool {
    bytes.get(4) == Some(&0x47)
        && bytes.get(4 + 192) == Some(&0x47)
        && bytes.get(4 + 192 * 2) == Some(&0x47)
}
