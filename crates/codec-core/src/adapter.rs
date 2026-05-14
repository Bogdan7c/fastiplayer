use serde::{Deserialize, Serialize};

use crate::{
    BitDepth, ChromaSubsampling, ColorMetadataOrigin, VideoCodec, VideoColorMetadata,
    VideoDecodeRequirement, VideoProfile, VideoSurfaceFormat, Vp9DecodedFormatRequirement,
    Vp9MetadataSource, Vp9Profile, Vp9RequirementCandidate, Vp9RequirementProbe,
    Vp9RequirementRejection, Vp9RequirementUncertainty, probe_vp9_packet_requirement,
    resolve_vp9_metadata,
};

/// Codec-neutral source metadata, пришедшая из manifest/container до decode.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub struct VideoMetadataSource {
    /// Codec, к которому относится metadata source.
    pub codec: VideoCodec,

    /// Источник metadata: manifest/container/bitstream/backend.
    pub origin: ColorMetadataOrigin,

    /// Codec profile hint, если source его сообщил.
    pub profile: Option<VideoProfile>,

    /// Bit depth hint, если source его сообщил.
    pub bit_depth: Option<BitDepth>,

    /// Chroma hint, если source его сообщил.
    pub chroma: Option<ChromaSubsampling>,

    /// Coded width, если source его сообщил.
    pub width: Option<u32>,

    /// Coded height, если source его сообщил.
    pub height: Option<u32>,

    /// Color metadata hint с сохранёнными origin/confidence.
    pub color: Option<VideoColorMetadata>,
}

impl VideoMetadataSource {
    /// Создаёт пустой metadata source для указанного codec/origin.
    #[must_use]
    pub const fn empty(codec: VideoCodec, origin: ColorMetadataOrigin) -> Self {
        Self {
            codec,
            origin,
            profile: None,
            bit_depth: None,
            chroma: None,
            width: None,
            height: None,
            color: None,
        }
    }

    /// Создаёт container metadata source.
    #[must_use]
    pub const fn container(codec: VideoCodec) -> Self {
        Self::empty(codec, ColorMetadataOrigin::Container)
    }

    /// Возвращает source с profile hint.
    #[must_use]
    pub const fn with_profile(mut self, profile: VideoProfile) -> Self {
        self.profile = Some(profile);
        self
    }

    /// Возвращает source с bit depth hint.
    #[must_use]
    pub const fn with_bit_depth(mut self, bit_depth: BitDepth) -> Self {
        self.bit_depth = Some(bit_depth);
        self
    }

    /// Возвращает source с chroma hint.
    #[must_use]
    pub const fn with_chroma(mut self, chroma: ChromaSubsampling) -> Self {
        self.chroma = Some(chroma);
        self
    }

    /// Возвращает source с coded resolution.
    #[must_use]
    pub const fn with_resolution(mut self, width: u32, height: u32) -> Self {
        self.width = Some(width);
        self.height = Some(height);
        self
    }

    /// Возвращает source с color metadata.
    #[must_use]
    pub fn with_color(mut self, color: VideoColorMetadata) -> Self {
        self.color = Some(color);
        self
    }

    /// Строит общий requirement из уже нормализованных metadata полей.
    #[must_use]
    pub fn to_requirement(&self) -> VideoDecodeRequirement {
        let mut requirement = VideoDecodeRequirement::new(self.codec);

        if let Some(profile) = self.profile {
            requirement = requirement.with_profile(profile);
        }

        if let Some(bit_depth) = self.bit_depth {
            requirement = requirement.with_bit_depth(bit_depth);
        }

        if let Some(chroma) = self.chroma {
            requirement = requirement.with_chroma(chroma);
        }

        if let (Some(width), Some(height)) = (self.width, self.height) {
            requirement = requirement.with_resolution(width, height);
        }

        if let Some(color) = &self.color {
            requirement = requirement.with_color(color.clone());
        }

        requirement
    }
}

/// Codec-neutral candidate, полученный adapter-ом из валидного bitstream header-а.
#[derive(Debug, Clone, PartialEq)]
pub struct VideoRequirementCandidate {
    /// Уточнённое stream requirement.
    pub requirement: VideoDecodeRequirement,

    /// Decoded surface format, если adapter смог вывести его из codec header-а.
    pub surface_format: Option<VideoSurfaceFormat>,

    /// Color metadata из bitstream header-а, если codec её выразил.
    pub bitstream_color: Option<VideoColorMetadata>,

    /// Codec-private payload нужен только adapter registry внутри `codec-core`.
    codec_private: CodecPrivateCandidate,
}

impl VideoRequirementCandidate {
    /// Создаёт generic candidate без codec-private state.
    #[must_use]
    pub fn generic(requirement: VideoDecodeRequirement) -> Self {
        Self {
            surface_format: requirement.surface_format,
            bitstream_color: requirement.color.clone(),
            requirement,
            codec_private: CodecPrivateCandidate::Generic,
        }
    }

    /// Создаёт candidate из VP9 adapter-а.
    #[must_use]
    fn from_vp9(candidate: Vp9RequirementCandidate) -> Self {
        let surface_format = vp9_decoded_format_to_surface_format(candidate.decoded_format);
        let requirement = candidate
            .requirement
            .clone()
            .with_surface_format(surface_format);

        Self {
            requirement,
            surface_format: Some(surface_format),
            bitstream_color: candidate.bitstream_color.clone(),
            codec_private: CodecPrivateCandidate::Vp9(candidate),
        }
    }
}

/// Codec-private часть candidate-а не выходит за пределы adapter registry.
#[derive(Debug, Clone, PartialEq)]
enum CodecPrivateCandidate {
    /// Candidate без codec-private resolver state.
    Generic,

    /// Исходный VP9 candidate для field-level resolver-а.
    Vp9(Vp9RequirementCandidate),
}

/// Результат codec-neutral packet probing.
#[derive(Debug, Clone, PartialEq)]
pub enum VideoRequirementProbe {
    /// Header прочитан достаточно надёжно для typed capability check.
    Candidate(VideoRequirementCandidate),

    /// Header валиден, но stream variant запрещён production policy.
    Rejected(VideoRequirementRejection),

    /// Header не даёт уверенного reject-а, поэтому decoder path может продолжаться.
    Recoverable(VideoRequirementUncertainty),
}

/// Результат packet-level keyframe probing через codec adapter registry.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum VideoPacketKeyframeProbe {
    /// Adapter уверенно прочитал keyframe flag.
    Keyframe(bool),

    /// Для codec нет packet keyframe adapter-а.
    AdapterUnavailable {
        /// Codec без keyframe adapter-а.
        codec: VideoCodec,
    },

    /// Adapter есть, но packet не дал надёжного результата.
    Uncertain(VideoRequirementUncertainty),
}

/// Typed reject от codec adapter-а до запуска heavy decode pipeline.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum VideoRequirementRejection {
    /// Для codec нет adapter-а, который может подтвердить packet requirement.
    UnsupportedCodecAdapter {
        /// Codec без adapter-а.
        codec: VideoCodec,
    },

    /// Bit depth распознана, но production surface/render path её не поддерживает.
    UnsupportedBitDepth {
        /// Codec stream-а.
        codec: VideoCodec,

        /// Сырое значение bit depth из codec header-а.
        bit_depth: u8,

        /// Codec adapter diagnostic.
        reason: String,
    },

    /// Chroma распознана, но production surface/render path её не поддерживает.
    UnsupportedChroma {
        /// Codec stream-а.
        codec: VideoCodec,

        /// Нормализованная chroma subsampling.
        chroma: ChromaSubsampling,

        /// Codec adapter diagnostic.
        reason: String,
    },
}

impl VideoRequirementRejection {
    /// Возвращает user-facing diagnostic без generic hardware wording.
    #[must_use]
    pub fn user_message(&self) -> String {
        match self {
            Self::UnsupportedCodecAdapter { codec } => {
                format!("для codec {codec} нет production packet requirement adapter-а")
            }
            Self::UnsupportedBitDepth { reason, .. } | Self::UnsupportedChroma { reason, .. } => {
                reason.clone()
            }
        }
    }
}

/// Recoverable uncertainty от codec adapter-а.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum VideoRequirementUncertainty {
    /// Для codec нет adapter-а; capability layer должен полагаться на container metadata.
    AdapterUnavailable {
        /// Codec без packet adapter-а.
        codec: VideoCodec,
    },

    /// Parser не смог надёжно разобрать header.
    ParseError {
        /// Codec stream-а.
        codec: VideoCodec,

        /// Диагностическая причина parser-а.
        reason: String,
    },

    /// Packet ссылается на уже декодированный кадр и не несёт stream requirement.
    ShowExistingFrame {
        /// Codec stream-а.
        codec: VideoCodec,
    },

    /// Header не содержит полного color/format config.
    MissingColorConfig {
        /// Codec stream-а.
        codec: VideoCodec,

        /// Profile, если adapter уже смог его прочитать.
        profile: Option<VideoProfile>,
    },

    /// Header содержит chroma flags вне текущей typed model.
    UnknownChromaLayout {
        /// Codec stream-а.
        codec: VideoCodec,

        /// Codec-specific horizontal subsampling flag.
        subsampling_x: bool,

        /// Codec-specific vertical subsampling flag.
        subsampling_y: bool,
    },

    /// Header содержит bit depth вне текущей typed model.
    UnknownBitDepth {
        /// Codec stream-а.
        codec: VideoCodec,

        /// Сырое значение bit depth из parser-а.
        bit_depth: u8,
    },
}

/// Итоговый generic metadata resolver output.
#[derive(Debug, Clone, PartialEq)]
pub struct VideoResolvedMetadata {
    /// Итоговое stream requirement.
    pub requirement: VideoDecodeRequirement,

    /// Decoded surface format, если он уже известен.
    pub surface_format: Option<VideoSurfaceFormat>,

    /// Resolved color metadata с сохранёнными origin/confidence.
    pub color: Option<VideoColorMetadata>,
}

/// Возвращает packet-level probing result через codec adapter registry.
#[must_use]
pub fn probe_video_packet_requirement(
    codec: VideoCodec,
    packet_bytes: &[u8],
) -> VideoRequirementProbe {
    match codec {
        VideoCodec::Vp9 => map_vp9_probe(probe_vp9_packet_requirement(packet_bytes)),
        other_codec => {
            VideoRequirementProbe::Recoverable(VideoRequirementUncertainty::AdapterUnavailable {
                codec: other_codec,
            })
        }
    }
}

/// Возвращает keyframe flag через codec adapter registry, если container его не сообщил.
#[must_use]
pub fn probe_video_packet_keyframe(
    codec: VideoCodec,
    packet_bytes: &[u8],
) -> VideoPacketKeyframeProbe {
    match codec {
        VideoCodec::Vp9 => match vp9_parser::parse_uncompressed_header(packet_bytes) {
            Ok(frame_info) => VideoPacketKeyframeProbe::Keyframe(frame_info.keyframe),
            Err(error) => {
                VideoPacketKeyframeProbe::Uncertain(VideoRequirementUncertainty::ParseError {
                    codec,
                    reason: error.to_string(),
                })
            }
        },
        other_codec => VideoPacketKeyframeProbe::AdapterUnavailable { codec: other_codec },
    }
}

/// Объединяет manifest/container и bitstream metadata через codec adapter registry.
#[must_use]
pub fn resolve_video_metadata(
    codec: VideoCodec,
    container_source: Option<VideoMetadataSource>,
    bitstream_candidate: Option<VideoRequirementCandidate>,
) -> VideoResolvedMetadata {
    if codec == VideoCodec::Vp9 {
        return resolve_vp9_video_metadata(container_source, bitstream_candidate);
    }

    let mut requirement = container_source
        .as_ref()
        .map(VideoMetadataSource::to_requirement)
        .unwrap_or_else(|| VideoDecodeRequirement::new(codec));

    if let Some(candidate) = bitstream_candidate {
        requirement = candidate.requirement;
    }

    VideoResolvedMetadata {
        surface_format: requirement.surface_format,
        color: requirement.color.clone(),
        requirement,
    }
}

/// Возвращает `true`, если requirement ещё нуждается в packet-level refinement.
#[must_use]
pub fn video_requirement_needs_packet_refinement(requirement: &VideoDecodeRequirement) -> bool {
    match requirement.codec {
        VideoCodec::Vp9 => {
            requirement.profile.is_none()
                || requirement.bit_depth.is_none()
                || requirement.chroma.is_none()
                || requirement.color.is_none()
                || requirement.surface_format.is_none()
        }
        _ => false,
    }
}

/// Проверяет, может ли codec adapter уточнить текущий unsupported metadata reject.
#[must_use]
pub fn unsupported_requirement_can_be_refined_by_packet_probe(
    requirement: &VideoDecodeRequirement,
    is_metadata_rejection: bool,
) -> bool {
    video_requirement_needs_packet_refinement(requirement) && is_metadata_rejection
}

/// Мапит VP9 packet probe в общий adapter result.
fn map_vp9_probe(probe: Vp9RequirementProbe) -> VideoRequirementProbe {
    match probe {
        Vp9RequirementProbe::Candidate(candidate) => {
            VideoRequirementProbe::Candidate(VideoRequirementCandidate::from_vp9(candidate))
        }
        Vp9RequirementProbe::Rejected(rejection) => {
            VideoRequirementProbe::Rejected(map_vp9_rejection(rejection))
        }
        Vp9RequirementProbe::Recoverable(uncertainty) => {
            VideoRequirementProbe::Recoverable(map_vp9_uncertainty(uncertainty))
        }
    }
}

/// Мапит VP9 policy reject в общий reject.
fn map_vp9_rejection(rejection: Vp9RequirementRejection) -> VideoRequirementRejection {
    match rejection {
        Vp9RequirementRejection::UnsupportedBitDepth(bit_depth) => {
            VideoRequirementRejection::UnsupportedBitDepth {
                codec: VideoCodec::Vp9,
                bit_depth,
                reason: rejection.user_message(),
            }
        }
        Vp9RequirementRejection::UnsupportedChroma(chroma) => {
            VideoRequirementRejection::UnsupportedChroma {
                codec: VideoCodec::Vp9,
                chroma,
                reason: rejection.user_message(),
            }
        }
    }
}

/// Мапит VP9 recoverable uncertainty в общий uncertainty.
fn map_vp9_uncertainty(uncertainty: Vp9RequirementUncertainty) -> VideoRequirementUncertainty {
    match uncertainty {
        Vp9RequirementUncertainty::ParseError(reason) => VideoRequirementUncertainty::ParseError {
            codec: VideoCodec::Vp9,
            reason,
        },
        Vp9RequirementUncertainty::ShowExistingFrame => {
            VideoRequirementUncertainty::ShowExistingFrame {
                codec: VideoCodec::Vp9,
            }
        }
        Vp9RequirementUncertainty::MissingColorConfig { profile } => {
            VideoRequirementUncertainty::MissingColorConfig {
                codec: VideoCodec::Vp9,
                profile: Some(VideoProfile::Vp9(profile)),
            }
        }
        Vp9RequirementUncertainty::UnknownChromaLayout {
            subsampling_x,
            subsampling_y,
        } => VideoRequirementUncertainty::UnknownChromaLayout {
            codec: VideoCodec::Vp9,
            subsampling_x,
            subsampling_y,
        },
        Vp9RequirementUncertainty::UnknownBitDepth { bit_depth } => {
            VideoRequirementUncertainty::UnknownBitDepth {
                codec: VideoCodec::Vp9,
                bit_depth,
            }
        }
    }
}

/// Запускает существующий VP9 resolver через generic source/candidate wrappers.
fn resolve_vp9_video_metadata(
    container_source: Option<VideoMetadataSource>,
    bitstream_candidate: Option<VideoRequirementCandidate>,
) -> VideoResolvedMetadata {
    let vp9_container_source = container_source.and_then(vp9_source_from_generic);
    let vp9_bitstream_candidate = bitstream_candidate.and_then(|candidate| {
        if let CodecPrivateCandidate::Vp9(vp9_candidate) = candidate.codec_private {
            Some(vp9_candidate)
        } else {
            None
        }
    });
    let resolved = resolve_vp9_metadata(vp9_container_source, vp9_bitstream_candidate);
    let surface_format = resolved
        .decoded_format
        .map(vp9_decoded_format_to_surface_format)
        .or(resolved.requirement.surface_format);
    let requirement = if let Some(surface_format) = surface_format {
        resolved.requirement.with_surface_format(surface_format)
    } else {
        resolved.requirement
    };

    VideoResolvedMetadata {
        surface_format,
        color: Some(resolved.color),
        requirement,
    }
}

/// Переводит generic metadata source в VP9 resolver source.
fn vp9_source_from_generic(source: VideoMetadataSource) -> Option<Vp9MetadataSource> {
    if source.codec != VideoCodec::Vp9 {
        return None;
    }

    let mut vp9_source = Vp9MetadataSource::empty(source.origin);
    vp9_source.profile = source.profile;
    vp9_source.bit_depth = source.bit_depth;
    vp9_source.chroma = source.chroma;
    vp9_source.width = source.width;
    vp9_source.height = source.height;
    if let Some(color) = source.color {
        vp9_source = vp9_source.with_color(color);
    }
    Some(vp9_source)
}

/// Переводит VP9 decoded format adapter-а в общий surface format.
const fn vp9_decoded_format_to_surface_format(
    decoded_format: Vp9DecodedFormatRequirement,
) -> VideoSurfaceFormat {
    match decoded_format {
        Vp9DecodedFormatRequirement::Nv12 => VideoSurfaceFormat::Nv12,
        Vp9DecodedFormatRequirement::P010 => VideoSurfaceFormat::P010,
    }
}

/// Возвращает VP9 profile из generic profile, если profile действительно VP9.
#[must_use]
pub const fn vp9_profile_from_video_profile(profile: VideoProfile) -> Option<Vp9Profile> {
    match profile {
        VideoProfile::Vp9(profile) => Some(profile),
        _ => None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Проверяет, что generic adapter сохраняет typed reject без player-core parsing.
    #[test]
    fn vp9_12bit_packet_maps_to_codec_neutral_reject() {
        let packet_bytes = build_vp9_keyframe(Vp9HeaderFixture {
            profile: 2,
            bit_depth: 12,
            subsampling_x: true,
            subsampling_y: true,
            width: 128,
            height: 72,
        });

        let probe = probe_video_packet_requirement(VideoCodec::Vp9, &packet_bytes);

        assert!(matches!(
            probe,
            VideoRequirementProbe::Rejected(VideoRequirementRejection::UnsupportedBitDepth {
                codec: VideoCodec::Vp9,
                bit_depth: 12,
                ..
            })
        ));
    }

    /// Проверяет, что codec adapter выставляет общий surface contract.
    #[test]
    fn vp9_profile2_10bit_packet_sets_p010_surface_contract() {
        let packet_bytes = build_vp9_keyframe(Vp9HeaderFixture {
            profile: 2,
            bit_depth: 10,
            subsampling_x: true,
            subsampling_y: true,
            width: 3840,
            height: 2160,
        });

        let probe = probe_video_packet_requirement(VideoCodec::Vp9, &packet_bytes);
        let VideoRequirementProbe::Candidate(candidate) = probe else {
            panic!("VP9 Profile 2 10-bit должен дать generic candidate, получено {probe:?}");
        };

        assert_eq!(candidate.surface_format, Some(VideoSurfaceFormat::P010));
        assert_eq!(
            candidate.requirement.surface_format,
            Some(VideoSurfaceFormat::P010)
        );
    }

    /// Проверяет, что codec без adapter-а не превращается в ложный hardware reject.
    #[test]
    fn codec_without_packet_adapter_is_recoverable() {
        let probe = probe_video_packet_requirement(VideoCodec::Av1, &[0, 1, 2, 3]);

        assert!(matches!(
            probe,
            VideoRequirementProbe::Recoverable(VideoRequirementUncertainty::AdapterUnavailable {
                codec: VideoCodec::Av1
            })
        ));
    }

    /// Проверяет, что keyframe probing тоже живёт за codec adapter API.
    #[test]
    fn vp9_keyframe_probe_returns_codec_neutral_result() {
        let packet_bytes = build_vp9_keyframe(Vp9HeaderFixture {
            profile: 0,
            bit_depth: 8,
            subsampling_x: true,
            subsampling_y: true,
            width: 64,
            height: 64,
        });

        let probe = probe_video_packet_keyframe(VideoCodec::Vp9, &packet_bytes);

        assert_eq!(probe, VideoPacketKeyframeProbe::Keyframe(true));
    }

    /// Минимальный набор VP9 header полей для adapter-level regression tests.
    struct Vp9HeaderFixture {
        /// VP9 profile id из uncompressed header.
        profile: u8,

        /// VP9 bit depth для profile 2/3.
        bit_depth: u8,

        /// VP9 subsampling_x flag.
        subsampling_x: bool,

        /// VP9 subsampling_y flag.
        subsampling_y: bool,

        /// Coded width.
        width: u32,

        /// Coded height.
        height: u32,
    }

    /// Собирает маленький VP9 keyframe header без зависимости от player-core.
    fn build_vp9_keyframe(fixture: Vp9HeaderFixture) -> Vec<u8> {
        let mut bits = Vec::new();
        push_bits(&mut bits, 0b10, 2);
        push_profile(&mut bits, fixture.profile);
        bits.push(0);
        bits.push(0);
        bits.push(1);
        bits.push(0);
        push_bits(&mut bits, 0x498342, 24);
        if matches!(fixture.profile, 2 | 3) {
            bits.push(u8::from(fixture.bit_depth == 12));
        }
        push_bits(&mut bits, 1, 3);
        bits.push(0);
        if matches!(fixture.profile, 1 | 3) {
            bits.push(u8::from(fixture.subsampling_x));
            bits.push(u8::from(fixture.subsampling_y));
            bits.push(0);
        }
        push_bits(&mut bits, fixture.width - 1, 16);
        push_bits(&mut bits, fixture.height - 1, 16);
        bits.push(0);
        bits_to_bytes(&bits)
    }

    /// Упаковывает MSB-first bits в bytes.
    fn bits_to_bytes(bits: &[u8]) -> Vec<u8> {
        bits.chunks(8)
            .map(|chunk| {
                let mut byte = 0u8;
                for (index, bit) in chunk.iter().enumerate() {
                    byte |= bit << (7 - index);
                }
                byte
            })
            .collect()
    }

    /// Добавляет `width` старших битов числа в VP9 bitstream order.
    fn push_bits(bits: &mut Vec<u8>, value: u32, width: u8) {
        for shift in (0..width).rev() {
            bits.push(((value >> shift) & 1) as u8);
        }
    }

    /// Кодирует VP9 profile bits из uncompressed header.
    fn push_profile(bits: &mut Vec<u8>, profile: u8) {
        bits.push(profile & 1);
        bits.push((profile >> 1) & 1);
        if profile == 3 {
            bits.push(0);
        }
    }
}
