//! `demux-api` registration для existing Symphonia container backend-а.

use demux_api::{
    DemuxContainerId, DemuxContainerRegistration, DemuxFactory, DemuxFactoryDescriptor,
    DemuxFactoryId, DemuxFactoryOpenError, DemuxFixtureId, DemuxHintRelationship, DemuxHints,
    DemuxIdentityError, DemuxInput, DemuxInputCapabilities, DemuxInputCapability, DemuxMimeType,
    DemuxOpenRequest, DemuxProbeConfidence, DemuxProbeDecision, DemuxProbeMatch,
    DemuxProbeRejection, DemuxProbeRequest, DemuxSourceExtension,
};
use media_core::Demuxer;

use crate::{DemuxerOptions, SymphoniaDemuxer};

/// Safe label не содержит URL/path/fingerprint concrete source-а.
const REGISTRY_SOURCE_LABEL: &str = "demux-registry-input";

/// Existing Symphonia backend как neutral project-owned factory registration.
pub struct SymphoniaDemuxFactory {
    /// Immutable registry capability/evidence snapshot.
    descriptor: DemuxFactoryDescriptor,
    /// Existing fail-safe runtime demux options.
    demuxer_options: DemuxerOptions,
}

impl SymphoniaDemuxFactory {
    /// Строит registration из checked static identities без runtime probing.
    pub fn new(demuxer_options: DemuxerOptions) -> Result<Self, DemuxIdentityError> {
        let descriptor = DemuxFactoryDescriptor::new(
            DemuxFactoryId::new("symphonia")?,
            DemuxInputCapabilities::only(DemuxInputCapability::SeekableBytes)
                .with(DemuxInputCapability::StreamingBytes),
            symphonia_container_registrations()?,
            vec![
                DemuxFixtureId::new("symphonia/generated-pcm-wav")?,
                DemuxFixtureId::new("symphonia/webm-vp9-opus")?,
                DemuxFixtureId::new("symphonia/mp4-h264-aac")?,
            ],
        );
        Ok(Self {
            descriptor,
            demuxer_options,
        })
    }

    /// Возвращает registration row по canonical container ID.
    fn container_registration(&self, container_id: &str) -> Option<&DemuxContainerRegistration> {
        self.descriptor
            .containers
            .iter()
            .find(|registration| registration.container.as_str() == container_id)
    }

    /// Передаёт caller extension только когда он согласован с доказанным container-ом.
    fn extension_hint<'request>(&self, request: &'request DemuxOpenRequest) -> &'request str {
        let selected_container = request.selected_probe.container.as_str();
        if request.selected_probe.hint_relationship != DemuxHintRelationship::Disagrees
            && let Some(extension) = request.hints.extension.as_ref()
        {
            return extension.as_str();
        }
        preferred_extension(selected_container)
    }
}

impl DemuxFactory for SymphoniaDemuxFactory {
    fn descriptor(&self) -> &DemuxFactoryDescriptor {
        &self.descriptor
    }

    fn probe(&self, request: DemuxProbeRequest<'_>) -> DemuxProbeDecision {
        if request.cancellation.is_cancelled() {
            return DemuxProbeDecision::Rejected(DemuxProbeRejection::Cancelled);
        }
        if !self
            .descriptor
            .input_capabilities
            .contains(request.input_capability)
        {
            return DemuxProbeDecision::Rejected(DemuxProbeRejection::UnsupportedInput {
                capability: request.input_capability,
            });
        }
        let detected_container = match detect_container(request.sniffed_bytes, request.hints) {
            ContainerDetection::Match(container_id) => container_id,
            ContainerDetection::Truncated { required_bytes } => {
                return DemuxProbeDecision::Rejected(DemuxProbeRejection::Truncated {
                    available_bytes: request.sniffed_bytes.len(),
                    required_bytes,
                });
            }
            ContainerDetection::NoMatch => return DemuxProbeDecision::NoMatch,
        };
        let Some(registration) = self.container_registration(detected_container) else {
            return DemuxProbeDecision::Rejected(DemuxProbeRejection::Malformed {
                reason: "Symphonia detector вернул незарегистрированный container ID".to_owned(),
            });
        };
        DemuxProbeDecision::Match(DemuxProbeMatch {
            container: registration.container.clone(),
            confidence: DemuxProbeConfidence::Signature,
            hint_relationship: registration.hint_relationship(request.hints),
        })
    }

    fn open(
        &self,
        request: DemuxOpenRequest,
    ) -> Result<Box<dyn Demuxer + Send>, DemuxFactoryOpenError> {
        if request.cancellation.is_cancelled() {
            return Err(DemuxFactoryOpenError::Cancelled);
        }
        let extension_hint = self.extension_hint(&request).to_owned();
        let demuxer = match request.input {
            DemuxInput::ByteSource(source) => SymphoniaDemuxer::from_byte_source_with_options(
                source,
                &extension_hint,
                REGISTRY_SOURCE_LABEL,
                self.demuxer_options,
            ),
            DemuxInput::ByteStream(reader) => SymphoniaDemuxer::from_stream_with_options(
                reader,
                &extension_hint,
                REGISTRY_SOURCE_LABEL,
                self.demuxer_options,
            ),
            DemuxInput::OrderedSegments(_) => {
                return Err(DemuxFactoryOpenError::UnsupportedInput {
                    capability: DemuxInputCapability::OrderedSegments,
                });
            }
        }
        .map_err(|error| DemuxFactoryOpenError::Backend(error.into()))?;
        if request.cancellation.is_cancelled() {
            return Err(DemuxFactoryOpenError::Cancelled);
        }
        Ok(Box::new(demuxer))
    }
}

/// Detector terminal outcome без backend allocation/open.
enum ContainerDetection {
    /// Stable signature определила canonical container ID.
    Match(&'static str),
    /// Prefix совпал с началом signature, но input закончился раньше решения.
    Truncated {
        /// Минимум bytes для terminal signature check.
        required_bytes: usize,
    },
    /// Ни одна Symphonia registration не подтверждена.
    NoMatch,
}

/// Выполняет bounded magic sniff; hints используются только для EBML subtype.
fn detect_container(bytes: &[u8], hints: &DemuxHints) -> ContainerDetection {
    if bytes.starts_with(b"\x1a\x45\xdf\xa3") {
        if contains_bytes(bytes, b"webm") {
            return ContainerDetection::Match("webm");
        }
        if contains_bytes(bytes, b"matroska") {
            return ContainerDetection::Match("matroska");
        }
        if hint_names_container(hints, "webm") {
            return ContainerDetection::Match("webm");
        }
        return ContainerDetection::Match("matroska");
    }
    if bytes.len() >= 12 && bytes.starts_with(b"RIFF") && &bytes[8..12] == b"WAVE" {
        return ContainerDetection::Match("wave");
    }
    if bytes.len() >= 12 && bytes.starts_with(b"FORM") && matches!(&bytes[8..12], b"AIFF" | b"AIFC")
    {
        return ContainerDetection::Match("aiff");
    }
    if bytes.len() >= 8 && &bytes[4..8] == b"ftyp" {
        return ContainerDetection::Match("iso-bmff");
    }
    if bytes.starts_with(b"OggS") {
        return ContainerDetection::Match("ogg");
    }
    if bytes.starts_with(b"fLaC") {
        return ContainerDetection::Match("flac");
    }
    if bytes.starts_with(b"caff") {
        return ContainerDetection::Match("caf");
    }
    if bytes.starts_with(b"ID3") || is_mpeg_audio_sync(bytes) {
        return ContainerDetection::Match("mpeg-audio");
    }

    for (signature, required_bytes) in [
        (b"\x1a\x45\xdf\xa3".as_slice(), 4),
        (b"RIFF".as_slice(), 12),
        (b"FORM".as_slice(), 12),
        (b"OggS".as_slice(), 4),
        (b"fLaC".as_slice(), 4),
        (b"caff".as_slice(), 4),
        (b"ID3".as_slice(), 3),
    ] {
        if !bytes.is_empty() && signature.starts_with(bytes) {
            return ContainerDetection::Truncated { required_bytes };
        }
    }
    if bytes.len() >= 5 && bytes.len() < 8 && b"ftyp".starts_with(&bytes[4..]) {
        return ContainerDetection::Truncated { required_bytes: 8 };
    }
    ContainerDetection::NoMatch
}

/// Ищет короткий EBML DocType marker без полноценного container parse.
fn contains_bytes(haystack: &[u8], needle: &[u8]) -> bool {
    haystack
        .windows(needle.len())
        .any(|window| window == needle)
}

/// Проверяет exact typed container/extension hints для EBML subtype choice.
fn hint_names_container(hints: &DemuxHints, container_id: &str) -> bool {
    hints
        .container
        .as_ref()
        .is_some_and(|container| container.as_str() == container_id)
        || hints
            .extension
            .as_ref()
            .is_some_and(|extension| match container_id {
                "webm" => matches!(extension.as_str(), "webm" | "weba"),
                "matroska" => matches!(extension.as_str(), "mkv" | "mka" | "mks"),
                _ => extension.as_str() == container_id,
            })
}

/// MPEG audio syncword сохраняет layer/version bits для отсечения случайного `0xff`.
fn is_mpeg_audio_sync(bytes: &[u8]) -> bool {
    bytes.len() >= 2 && bytes[0] == 0xff && bytes[1] & 0xe0 == 0xe0
}

/// Возвращает extension, который включает existing container-specific pre-scan path.
fn preferred_extension(container_id: &str) -> &'static str {
    match container_id {
        "iso-bmff" => "mp4",
        "matroska" => "mkv",
        "webm" => "webm",
        "ogg" => "ogg",
        "caf" => "caf",
        "wave" => "wav",
        "aiff" => "aiff",
        "flac" => "flac",
        "mpeg-audio" => "mp3",
        _ => "",
    }
}

/// Собирает exact current Symphonia container ownership rows.
fn symphonia_container_registrations() -> Result<Vec<DemuxContainerRegistration>, DemuxIdentityError>
{
    Ok(vec![
        registration(
            "iso-bmff",
            &["mp4", "m4a", "m4v", "mov", "3gp", "3g2"],
            &["audio/mp4", "video/mp4", "video/quicktime"],
        )?,
        registration("matroska", &["mkv", "mka", "mks"], &["video/x-matroska"])?,
        registration("webm", &["webm", "weba"], &["audio/webm", "video/webm"])?,
        registration(
            "ogg",
            &["ogg", "oga", "ogv", "opus", "spx"],
            &["application/ogg", "audio/ogg", "video/ogg"],
        )?,
        registration("caf", &["caf"], &["audio/x-caf"])?,
        registration("wave", &["wav", "wave"], &["audio/wav", "audio/x-wav"])?,
        registration(
            "aiff",
            &["aif", "aiff", "aifc"],
            &["audio/aiff", "audio/x-aiff"],
        )?,
        registration("flac", &["flac"], &["audio/flac"])?,
        registration("mpeg-audio", &["mp1", "mp2", "mp3"], &["audio/mpeg"])?,
    ])
}

/// Преобразует checked static string slices в owned neutral descriptor values.
fn registration(
    container: &str,
    extensions: &[&str],
    mime_types: &[&str],
) -> Result<DemuxContainerRegistration, DemuxIdentityError> {
    Ok(DemuxContainerRegistration::new(
        DemuxContainerId::new(container)?,
        extensions
            .iter()
            .map(|extension| DemuxSourceExtension::new(*extension))
            .collect::<Result<Vec<_>, _>>()?,
        mime_types
            .iter()
            .map(|mime_type| DemuxMimeType::new(*mime_type))
            .collect::<Result<Vec<_>, _>>()?,
    ))
}

#[cfg(test)]
mod tests;
