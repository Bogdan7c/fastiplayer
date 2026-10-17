//! Регистрация сырого AC-3 / E-AC-3 в нейтральном `demux-api` registry.

use demux_api::{
    DemuxContainerId, DemuxContainerRegistration, DemuxFactory, DemuxFactoryDescriptor,
    DemuxFactoryId, DemuxFactoryOpenError, DemuxFixtureId, DemuxHintRelationship,
    DemuxInputCapabilities, DemuxInputCapability, DemuxMimeType, DemuxOpenRequest,
    DemuxProbeConfidence, DemuxProbeDecision, DemuxProbeMatch, DemuxProbeRejection,
    DemuxProbeRequest, DemuxSourceExtension,
};
use media_core::Demuxer;

use crate::probe::{PrefixEvidence, classify_prefix};
use crate::{Ac3ElementaryDemuxError, Ac3ElementaryDemuxOptions, Ac3ElementaryDemuxer};

/// Стабильный container id сырого AC-3 / E-AC-3 в registry.
pub const AC3_ELEMENTARY_CONTAINER_ID: &str = "ac3-elementary";

/// Расширения: `.ac3` (Dolby Digital), `.eac3` и `.ec3` (Dolby Digital Plus).
const EXTENSIONS: [&str; 3] = ["ac3", "eac3", "ec3"];

/// MIME типы RFC 4184 (AC-3), RFC 4598 (E-AC-3) и vendor-тип IANA
/// `audio/vnd.dolby.dd-raw`, которым серверы отдают сырой Dolby Digital.
const MIME_TYPES: [&str; 3] = ["audio/ac3", "audio/eac3", "audio/vnd.dolby.dd-raw"];

/// Factory сырых AC-3 / E-AC-3 файлов: одна регистрация, seekable и потоковый вход.
pub struct Ac3ElementaryDemuxFactory {
    descriptor: DemuxFactoryDescriptor,
    options: Ac3ElementaryDemuxOptions,
}

impl Ac3ElementaryDemuxFactory {
    /// Строит immutable descriptor из caller-owned пределов.
    pub fn new(options: Ac3ElementaryDemuxOptions) -> Result<Self, demux_api::DemuxIdentityError> {
        let capabilities = DemuxInputCapabilities::only(DemuxInputCapability::SeekableBytes)
            .with(DemuxInputCapability::StreamingBytes);
        let extensions = EXTENSIONS
            .into_iter()
            .map(DemuxSourceExtension::new)
            .collect::<Result<Vec<_>, _>>()?;
        let mime_types = MIME_TYPES
            .into_iter()
            .map(DemuxMimeType::new)
            .collect::<Result<Vec<_>, _>>()?;
        let descriptor = DemuxFactoryDescriptor::new(
            DemuxFactoryId::new("ac3-elementary-first-party")?,
            vec![DemuxContainerRegistration::new(
                DemuxContainerId::new(AC3_ELEMENTARY_CONTAINER_ID)?,
                capabilities,
                extensions,
                mime_types,
            )],
            vec![
                DemuxFixtureId::new("generated-ac3-elementary-tones")?,
                DemuxFixtureId::new("generated-eac3-elementary-tones")?,
            ],
        );
        Ok(Self {
            descriptor,
            options,
        })
    }

    /// Единственная регистрация factory.
    fn registration(&self) -> Option<&DemuxContainerRegistration> {
        self.descriptor.containers.first()
    }
}

impl DemuxFactory for Ac3ElementaryDemuxFactory {
    fn descriptor(&self) -> &DemuxFactoryDescriptor {
        &self.descriptor
    }

    fn probe(&self, request: DemuxProbeRequest<'_>) -> DemuxProbeDecision {
        if request.cancellation.is_cancelled() {
            return DemuxProbeDecision::Rejected(DemuxProbeRejection::Cancelled);
        }
        let Some(registration) = self.registration() else {
            return DemuxProbeDecision::NoMatch;
        };
        let hint_relationship = registration.hint_relationship(request.hints);
        let confidence = match classify_prefix(request.sniffed_bytes) {
            PrefixEvidence::StreamAtStart => DemuxProbeConfidence::Exact,
            // Мусор впереди: только если расширение/MIME подтверждают AC-3.
            // `HintOnly` уступает любому контейнеру с настоящей сигнатурой.
            PrefixEvidence::StreamAfterGarbage
                if hint_relationship == DemuxHintRelationship::Agrees =>
            {
                DemuxProbeConfidence::HintOnly
            }
            PrefixEvidence::StreamAfterGarbage | PrefixEvidence::NotAc3 => {
                return DemuxProbeDecision::NoMatch;
            }
            PrefixEvidence::Truncated {
                available_bytes,
                required_bytes,
            } => {
                return DemuxProbeDecision::Rejected(DemuxProbeRejection::Truncated {
                    available_bytes,
                    required_bytes,
                });
            }
        };
        if !registration.supports_input(request.input_capability) {
            return DemuxProbeDecision::Rejected(DemuxProbeRejection::UnsupportedInput {
                capability: request.input_capability,
            });
        }
        DemuxProbeDecision::Match(DemuxProbeMatch {
            container: registration.container.clone(),
            confidence,
            hint_relationship,
        })
    }

    fn open(
        &self,
        request: DemuxOpenRequest,
    ) -> Result<Box<dyn Demuxer + Send>, DemuxFactoryOpenError> {
        if request.cancellation.is_cancelled() {
            return Err(DemuxFactoryOpenError::Cancelled);
        }
        Ac3ElementaryDemuxer::open(request.input, request.cancellation, self.options)
            .map(|demuxer| Box::new(demuxer) as Box<dyn Demuxer + Send>)
            .map_err(|error| match error {
                Ac3ElementaryDemuxError::Cancelled => DemuxFactoryOpenError::Cancelled,
                other => DemuxFactoryOpenError::Backend(other.into()),
            })
    }
}
