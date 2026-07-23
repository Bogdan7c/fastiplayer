use demux_api::{
    DemuxContainerId, DemuxContainerRegistration, DemuxFactory, DemuxFactoryDescriptor,
    DemuxFactoryId, DemuxFactoryOpenError, DemuxFixtureId, DemuxInputCapabilities,
    DemuxInputCapability, DemuxMimeType, DemuxOpenRequest, DemuxProbeConfidence,
    DemuxProbeDecision, DemuxProbeMatch, DemuxProbeRejection, DemuxProbeRequest,
    DemuxSourceExtension,
};
use media_core::Demuxer;

use crate::framing::FLV_SIGNATURE;
use crate::{FlvDemuxError, FlvDemuxOptions, FlvDemuxer};

const FLV_CONTAINER_ID: &str = "flv";
const F4F_CONTAINER_ID: &str = "f4f";

/// Registry adapter с двумя exact registrations и разными input shapes.
pub struct FlvDemuxFactory {
    descriptor: DemuxFactoryDescriptor,
    options: FlvDemuxOptions,
}

impl FlvDemuxFactory {
    /// Создаёт immutable descriptor из caller-owned bounded policy.
    pub fn new(options: FlvDemuxOptions) -> Result<Self, demux_api::DemuxIdentityError> {
        let flv_capabilities = DemuxInputCapabilities::only(DemuxInputCapability::SeekableBytes)
            .with(DemuxInputCapability::StreamingBytes);
        let f4f_capabilities = DemuxInputCapabilities::only(DemuxInputCapability::OrderedSegments);
        let descriptor = DemuxFactoryDescriptor::new(
            DemuxFactoryId::new("flv-f4f-first-party")?,
            vec![
                DemuxContainerRegistration::new(
                    DemuxContainerId::new(FLV_CONTAINER_ID)?,
                    flv_capabilities,
                    vec![DemuxSourceExtension::new("flv")?],
                    vec![
                        DemuxMimeType::new("video/x-flv")?,
                        DemuxMimeType::new("video/flv")?,
                    ],
                ),
                DemuxContainerRegistration::new(
                    DemuxContainerId::new(F4F_CONTAINER_ID)?,
                    f4f_capabilities,
                    vec![DemuxSourceExtension::new("f4f")?],
                    vec![DemuxMimeType::new("video/f4f")?],
                ),
            ],
            vec![
                DemuxFixtureId::new("generated-flv-progressive-live")?,
                DemuxFixtureId::new("generated-flv-config-rollover-seek")?,
                DemuxFixtureId::new("generated-f4f-fragments-discontinuity")?,
            ],
        );
        Ok(Self {
            descriptor,
            options,
        })
    }

    fn registration(&self, container: &str) -> &DemuxContainerRegistration {
        self.descriptor
            .containers
            .iter()
            .find(|registration| registration.container.as_str() == container)
            .expect("factory constructs both exact registrations")
    }
}

impl DemuxFactory for FlvDemuxFactory {
    fn descriptor(&self) -> &DemuxFactoryDescriptor {
        &self.descriptor
    }

    fn probe(&self, request: DemuxProbeRequest<'_>) -> DemuxProbeDecision {
        if request.cancellation.is_cancelled() {
            return DemuxProbeDecision::Rejected(DemuxProbeRejection::Cancelled);
        }
        if request.sniffed_bytes.starts_with(FLV_SIGNATURE) {
            let registration = self.registration(FLV_CONTAINER_ID);
            if !registration.supports_input(request.input_capability) {
                return DemuxProbeDecision::Rejected(DemuxProbeRejection::UnsupportedInput {
                    capability: request.input_capability,
                });
            }
            if request.sniffed_bytes.len() < 9 {
                return DemuxProbeDecision::Rejected(DemuxProbeRejection::Truncated {
                    available_bytes: request.sniffed_bytes.len(),
                    required_bytes: 9,
                });
            }
            return DemuxProbeDecision::Match(DemuxProbeMatch {
                container: registration.container.clone(),
                confidence: DemuxProbeConfidence::Exact,
                hint_relationship: registration.hint_relationship(request.hints),
            });
        }
        if request.input_capability == DemuxInputCapability::OrderedSegments
            && has_f4f_box_signature(request.sniffed_bytes)
        {
            let registration = self.registration(F4F_CONTAINER_ID);
            return DemuxProbeDecision::Match(DemuxProbeMatch {
                container: registration.container.clone(),
                confidence: DemuxProbeConfidence::Signature,
                hint_relationship: registration.hint_relationship(request.hints),
            });
        }
        DemuxProbeDecision::NoMatch
    }

    fn open(
        &self,
        request: DemuxOpenRequest,
    ) -> Result<Box<dyn Demuxer + Send>, DemuxFactoryOpenError> {
        if request.cancellation.is_cancelled() {
            return Err(DemuxFactoryOpenError::Cancelled);
        }
        let is_f4f = request.selected_probe.container.as_str() == F4F_CONTAINER_ID;
        FlvDemuxer::open(request.input, is_f4f, request.cancellation, self.options)
            .map(|demuxer| Box::new(demuxer) as Box<dyn Demuxer + Send>)
            .map_err(|error| match error {
                FlvDemuxError::Cancelled => DemuxFactoryOpenError::Cancelled,
                other => DemuxFactoryOpenError::Backend(other.into()),
            })
    }
}

fn has_f4f_box_signature(bytes: &[u8]) -> bool {
    bytes.get(4..8).is_some_and(|box_type| box_type == b"afra")
}
