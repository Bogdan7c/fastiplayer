use serde::{Deserialize, Serialize};

use crate::{
    BitDepth, ChromaSubsampling, ColorMetadataOrigin, H264ParameterSetKind, H264RequirementError,
    H264SpsError, H264SpsMetadata, H265PacketDecodeStartProbe, H265ParameterSetKind,
    H265RequirementError, H265SpsError, VideoCodec, VideoColorMetadata, VideoDecodeRequirement,
    VideoFramePixelLayout, VideoProfile, Vp9DecodedFormatRequirement, Vp9MetadataSource,
    Vp9Profile, Vp9RequirementCandidate, Vp9RequirementProbe, Vp9RequirementRejection,
    Vp9RequirementUncertainty, h264_sps_metadata_from_avc_decoder_configuration_record,
    h264_sps_metadata_from_packet, h265_decode_requirement_from_hevc_decoder_configuration_record,
    h265_decode_requirement_from_packet, infer_h264_packetization, infer_h265_packetization,
    parse_hevc_decoder_configuration_record, probe_av1_packet_keyframe, probe_h264_packet_keyframe,
    probe_h265_packet_decode_start, probe_vp8_packet_keyframe, probe_vp9_packet_requirement,
    resolve_vp9_metadata, video_frame_pixel_layout_from_decode_requirement,
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
    pub surface_format: Option<VideoFramePixelLayout>,

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
            surface_format: video_frame_pixel_layout_from_decode_requirement(&requirement),
            bitstream_color: requirement.color.clone(),
            requirement,
            codec_private: CodecPrivateCandidate::Generic,
        }
    }

    /// Создаёт candidate из VP9 adapter-а.
    #[must_use]
    fn from_vp9(candidate: Vp9RequirementCandidate) -> Self {
        let surface_format = vp9_decoded_format_to_surface_format(candidate.decoded_format);

        Self {
            requirement: candidate.requirement.clone(),
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
// Cold path: probing выполняется при открытии stream-а, а не per-frame, поэтому
// крупный Candidate-variant держим inline без Box. Реструктуризация ради лишь
// этого lint-а относится к отдельной P2 boundary-задаче.
#[allow(clippy::large_enum_variant)]
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

    /// Chroma распознана в codec-specific числовом поле, но общей enum-модели пока нет.
    UnsupportedChromaFormat {
        /// Codec stream-а.
        codec: VideoCodec,

        /// Raw codec-specific chroma id.
        chroma_format_idc: u32,

        /// Codec adapter diagnostic.
        reason: String,
    },

    /// Profile распознан, но v1 production policy его запрещает.
    UnsupportedProfile {
        /// Codec stream-а.
        codec: VideoCodec,

        /// Raw codec-specific profile id.
        profile_idc: u8,

        /// Raw codec-specific constraint/compatibility flags.
        constraint_flags: u8,

        /// Codec adapter diagnostic.
        reason: String,
    },

    /// Packetization/stream framing распознаны как неподдержанные текущим adapter-ом.
    UnsupportedPacketization {
        /// Codec stream-а.
        codec: VideoCodec,

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
            Self::UnsupportedChromaFormat { reason, .. }
            | Self::UnsupportedProfile { reason, .. }
            | Self::UnsupportedPacketization { reason, .. } => reason.clone(),
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

    /// Packet не содержит нужный codec parameter set, но stream ещё может отдать его позже.
    MissingParameterSet {
        /// Codec stream-а.
        codec: VideoCodec,

        /// Какого parameter set-а не хватает.
        parameter_set: H264ParameterSetKind,
    },

    /// Packet не содержит нужный HEVC parameter set, но stream ещё может отдать его позже.
    MissingH265ParameterSet {
        /// Codec stream-а.
        codec: VideoCodec,

        /// Какого HEVC parameter set-а не хватает.
        parameter_set: H265ParameterSetKind,
    },
}

/// Итоговый generic metadata resolver output.
#[derive(Debug, Clone, PartialEq)]
pub struct VideoResolvedMetadata {
    /// Итоговое stream requirement.
    pub requirement: VideoDecodeRequirement,

    /// Decoded surface format, если он уже известен.
    pub surface_format: Option<VideoFramePixelLayout>,

    /// Resolved color metadata с сохранёнными origin/confidence.
    pub color: Option<VideoColorMetadata>,
}

/// Возвращает packet-level probing result через codec adapter registry.
#[must_use]
pub fn probe_video_packet_requirement(
    codec: VideoCodec,
    packet_bytes: &[u8],
) -> VideoRequirementProbe {
    probe_video_packet_requirement_with_codec_private(codec, packet_bytes, None)
}

/// Возвращает packet-level probing result с codec-private state контейнера.
///
/// H.264/H.265 MP4/MKV streams часто несут parameter sets и NAL length size в
/// `avcC`/`hvcC`, а packet bytes содержат только length-prefixed access unit.
/// Поэтому caller, у которого есть container private data, должен использовать
/// этот boundary вместо угадывания packetization по первым байтам packet-а.
#[must_use]
pub fn probe_video_packet_requirement_with_codec_private(
    codec: VideoCodec,
    packet_bytes: &[u8],
    codec_private: Option<&[u8]>,
) -> VideoRequirementProbe {
    match codec {
        VideoCodec::Vp9 => map_vp9_probe(probe_vp9_packet_requirement(packet_bytes)),
        VideoCodec::H264 => probe_h264_packet_requirement(packet_bytes, codec_private),
        VideoCodec::H265 => probe_h265_packet_requirement(packet_bytes, codec_private),
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
    probe_video_packet_keyframe_with_codec_private(codec, packet_bytes, None)
}

/// Возвращает keyframe flag с учётом container codec private data.
#[must_use]
pub fn probe_video_packet_keyframe_with_codec_private(
    codec: VideoCodec,
    packet_bytes: &[u8],
    codec_private: Option<&[u8]>,
) -> VideoPacketKeyframeProbe {
    match codec {
        VideoCodec::Vp8 => match probe_vp8_packet_keyframe(packet_bytes) {
            Ok(keyframe) => VideoPacketKeyframeProbe::Keyframe(keyframe),
            Err(error) => {
                VideoPacketKeyframeProbe::Uncertain(VideoRequirementUncertainty::ParseError {
                    codec,
                    reason: error.to_string(),
                })
            }
        },
        VideoCodec::Vp9 => match vp9_parser::parse_uncompressed_header(packet_bytes) {
            Ok(frame_info) => VideoPacketKeyframeProbe::Keyframe(frame_info.keyframe),
            Err(error) => {
                VideoPacketKeyframeProbe::Uncertain(VideoRequirementUncertainty::ParseError {
                    codec,
                    reason: error.to_string(),
                })
            }
        },
        VideoCodec::H264 => match infer_h264_packetization(codec_private, packet_bytes)
            .and_then(|packetization| probe_h264_packet_keyframe(packet_bytes, packetization))
        {
            Ok(keyframe) => VideoPacketKeyframeProbe::Keyframe(keyframe),
            Err(error) => {
                VideoPacketKeyframeProbe::Uncertain(VideoRequirementUncertainty::ParseError {
                    codec,
                    reason: error.to_string(),
                })
            }
        },
        VideoCodec::H265 => match infer_h265_packetization(codec_private, packet_bytes) {
            Ok(packetization) => {
                match probe_h265_packet_decode_start(packet_bytes, packetization) {
                    H265PacketDecodeStartProbe::DecodeStart => {
                        VideoPacketKeyframeProbe::Keyframe(true)
                    }
                    H265PacketDecodeStartProbe::NotDecodeStart => {
                        VideoPacketKeyframeProbe::Keyframe(false)
                    }
                    H265PacketDecodeStartProbe::Uncertain(error) => {
                        VideoPacketKeyframeProbe::Uncertain(
                            VideoRequirementUncertainty::ParseError {
                                codec,
                                reason: error.to_string(),
                            },
                        )
                    }
                }
            }
            Err(error) => {
                VideoPacketKeyframeProbe::Uncertain(VideoRequirementUncertainty::ParseError {
                    codec,
                    reason: error.to_string(),
                })
            }
        },
        VideoCodec::Av1 => match probe_av1_packet_keyframe(packet_bytes, codec_private) {
            Ok(keyframe) => VideoPacketKeyframeProbe::Keyframe(keyframe),
            Err(error) => {
                VideoPacketKeyframeProbe::Uncertain(VideoRequirementUncertainty::ParseError {
                    codec,
                    reason: error.to_string(),
                })
            }
        },
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

    let bitstream_surface_format = bitstream_candidate
        .as_ref()
        .and_then(|candidate| candidate.surface_format);

    if let Some(candidate) = bitstream_candidate {
        requirement = merge_bitstream_requirement(requirement, candidate.requirement);
    }

    VideoResolvedMetadata {
        surface_format: bitstream_surface_format
            .or_else(|| video_frame_pixel_layout_from_decode_requirement(&requirement)),
        color: requirement.color.clone(),
        requirement,
    }
}

/// Объединяет container metadata и bitstream refinement без потери полей,
/// которые packet/header parser не может выразить сам, например MP4 HDR `colr`.
fn merge_bitstream_requirement(
    container_requirement: VideoDecodeRequirement,
    bitstream_requirement: VideoDecodeRequirement,
) -> VideoDecodeRequirement {
    let mut requirement = bitstream_requirement;

    if requirement.profile.is_none() {
        requirement.profile = container_requirement.profile;
    }
    if requirement.bit_depth.is_none() {
        requirement.bit_depth = container_requirement.bit_depth;
    }
    if requirement.chroma.is_none() {
        requirement.chroma = container_requirement.chroma;
    }
    if requirement.width.is_none() {
        requirement.width = container_requirement.width;
    }
    if requirement.height.is_none() {
        requirement.height = container_requirement.height;
    }
    if requirement.fps.is_none() {
        requirement.fps = container_requirement.fps;
    }
    if requirement.timing_contract.nominal_frame_rate.is_none() {
        requirement.timing_contract = container_requirement.timing_contract;
    }
    if let Some(color) = requirement.color.clone().or(container_requirement.color) {
        requirement = requirement.with_color(color);
    }

    requirement
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
                || video_frame_pixel_layout_from_decode_requirement(requirement).is_none()
        }
        VideoCodec::H264 => {
            requirement.profile.is_none()
                || requirement.bit_depth.is_none()
                || requirement.chroma.is_none()
                || requirement.width.is_none()
                || requirement.height.is_none()
        }
        VideoCodec::H265 => {
            requirement.profile.is_none()
                || requirement.bit_depth.is_none()
                || requirement.chroma.is_none()
                || requirement.width.is_none()
                || requirement.height.is_none()
        }
        VideoCodec::Av1 | VideoCodec::Vp8 => false,
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

/// Пробует H.264 requirement из avcC, затем из SPS внутри packet-а.
fn probe_h264_packet_requirement(
    packet_bytes: &[u8],
    codec_private: Option<&[u8]>,
) -> VideoRequirementProbe {
    let mut deferred_private_error = None;

    if let Some(codec_private) = codec_private.filter(|bytes| !bytes.is_empty()) {
        match h264_sps_metadata_from_avc_decoder_configuration_record(codec_private) {
            Ok(metadata) => return VideoRequirementProbe::Candidate(h264_candidate(metadata)),
            Err(error) if h264_requirement_error_is_policy_rejection(&error) => {
                return map_h264_requirement_error(error);
            }
            Err(error) => {
                deferred_private_error = Some(error);
            }
        }
    }

    match infer_h264_packetization(codec_private, packet_bytes)
        .map_err(H264RequirementError::ByteStream)
        .and_then(|packetization| h264_sps_metadata_from_packet(packet_bytes, packetization))
    {
        Ok(metadata) => VideoRequirementProbe::Candidate(h264_candidate(metadata)),
        Err(error @ H264RequirementError::ByteStream(_)) => match deferred_private_error {
            Some(private_error) => map_h264_requirement_error(private_error),
            None => map_h264_requirement_error(error),
        },
        Err(error) => map_h264_requirement_error(error),
    }
}

/// Строит codec-neutral candidate из подтверждённого H.264 SPS.
fn h264_candidate(metadata: H264SpsMetadata) -> VideoRequirementCandidate {
    let mut requirement = VideoDecodeRequirement::new(VideoCodec::H264)
        .with_profile(VideoProfile::H264(metadata.profile))
        .with_bit_depth(metadata.bit_depth)
        .with_chroma(metadata.chroma)
        .with_resolution(metadata.width, metadata.height);

    if let Some(color) = metadata.color {
        requirement = requirement.with_color(color);
    }

    let mut candidate = VideoRequirementCandidate::generic(requirement);
    candidate.surface_format = Some(VideoFramePixelLayout::Nv12);
    candidate
}

/// Пробует H.265 requirement из `hvcC`, затем из in-band SPS внутри packet-а.
fn probe_h265_packet_requirement(
    packet_bytes: &[u8],
    codec_private: Option<&[u8]>,
) -> VideoRequirementProbe {
    let mut packetization_from_private = None;
    let mut fallback_private_requirement = None;
    let mut deferred_private_error = None;

    if let Some(codec_private) = codec_private.filter(|bytes| !bytes.is_empty()) {
        match parse_hevc_decoder_configuration_record(codec_private) {
            Ok(record) => {
                packetization_from_private = Some(record.packetization());
                match h265_decode_requirement_from_hevc_decoder_configuration_record(codec_private)
                {
                    Ok(requirement) if record.sequence_parameter_sets().is_empty() => {
                        fallback_private_requirement = Some(requirement);
                    }
                    Ok(requirement) => {
                        return VideoRequirementProbe::Candidate(
                            VideoRequirementCandidate::generic(requirement),
                        );
                    }
                    Err(error) if h265_requirement_error_is_policy_rejection(&error) => {
                        return map_h265_requirement_error(error);
                    }
                    Err(error) => {
                        deferred_private_error = Some(error);
                    }
                }
            }
            Err(error) => {
                deferred_private_error =
                    Some(H265RequirementError::HevcDecoderConfigurationRecord(error));
            }
        }
    }

    let packetization = match packetization_from_private {
        Some(packetization) => Ok(packetization),
        None => infer_h265_packetization(codec_private, packet_bytes),
    };

    match packetization
        .map_err(H265RequirementError::ByteStream)
        .and_then(|packetization| h265_decode_requirement_from_packet(packet_bytes, packetization))
    {
        Ok(requirement) => {
            VideoRequirementProbe::Candidate(VideoRequirementCandidate::generic(requirement))
        }
        Err(error @ H265RequirementError::MissingSequenceParameterSet)
        | Err(error @ H265RequirementError::ByteStream(_)) => {
            if let Some(private_requirement) = fallback_private_requirement {
                VideoRequirementProbe::Candidate(VideoRequirementCandidate::generic(
                    private_requirement,
                ))
            } else if matches!(error, H265RequirementError::ByteStream(_)) {
                match deferred_private_error {
                    Some(private_error) => map_h265_requirement_error(private_error),
                    None => map_h265_requirement_error(error),
                }
            } else {
                map_h265_requirement_error(error)
            }
        }
        Err(error) => map_h265_requirement_error(error),
    }
}

/// Проверяет, что H.264 ошибка является policy reject-ом, а не recoverable uncertainty.
fn h264_requirement_error_is_policy_rejection(error: &H264RequirementError) -> bool {
    matches!(
        error,
        H264RequirementError::AvcDecoderConfigurationRecord(
            crate::AvcDecoderConfigurationRecordError::UnsupportedProfile { .. }
                | crate::AvcDecoderConfigurationRecordError::UnsupportedNalLengthSize { .. }
        ) | H264RequirementError::SequenceParameterSet(
            H264SpsError::UnsupportedProfile { .. }
                | H264SpsError::UnsupportedBitDepth { .. }
                | H264SpsError::UnsupportedChroma { .. }
        )
    )
}

/// Мапит H.264 typed errors в общий adapter result.
fn map_h264_requirement_error(error: H264RequirementError) -> VideoRequirementProbe {
    match error {
        H264RequirementError::AvcDecoderConfigurationRecord(
            crate::AvcDecoderConfigurationRecordError::UnsupportedProfile {
                profile_idc,
                profile_compatibility,
                reason,
            },
        ) => VideoRequirementProbe::Rejected(VideoRequirementRejection::UnsupportedProfile {
            codec: VideoCodec::H264,
            profile_idc,
            constraint_flags: profile_compatibility,
            reason: reason.to_string(),
        }),
        H264RequirementError::AvcDecoderConfigurationRecord(
            crate::AvcDecoderConfigurationRecordError::UnsupportedNalLengthSize { nal_length_size },
        ) => VideoRequirementProbe::Rejected(VideoRequirementRejection::UnsupportedPacketization {
            codec: VideoCodec::H264,
            reason: format!(
                "H.264 AVCC NAL length size {nal_length_size} is unsupported; expected 1, 2 or 4"
            ),
        }),
        H264RequirementError::SequenceParameterSet(H264SpsError::UnsupportedProfile {
            profile_idc,
            constraint_flags,
            reason,
        }) => VideoRequirementProbe::Rejected(VideoRequirementRejection::UnsupportedProfile {
            codec: VideoCodec::H264,
            profile_idc,
            constraint_flags,
            reason: reason.to_string(),
        }),
        H264RequirementError::SequenceParameterSet(H264SpsError::UnsupportedBitDepth {
            bit_depth,
        }) => VideoRequirementProbe::Rejected(VideoRequirementRejection::UnsupportedBitDepth {
            codec: VideoCodec::H264,
            bit_depth,
            reason: format!(
                "H.264 {bit_depth}-bit stream is not supported by the v1 zero-copy path"
            ),
        }),
        H264RequirementError::SequenceParameterSet(H264SpsError::UnsupportedChroma {
            chroma_format_idc,
        }) => map_h264_unsupported_chroma(chroma_format_idc),
        H264RequirementError::MissingSequenceParameterSet => {
            VideoRequirementProbe::Recoverable(VideoRequirementUncertainty::MissingParameterSet {
                codec: VideoCodec::H264,
                parameter_set: H264ParameterSetKind::Sequence,
            })
        }
        other_error => {
            VideoRequirementProbe::Recoverable(VideoRequirementUncertainty::ParseError {
                codec: VideoCodec::H264,
                reason: other_error.to_string(),
            })
        }
    }
}

/// Мапит H.264 `chroma_format_idc` в максимально конкретный общий reject.
fn map_h264_unsupported_chroma(chroma_format_idc: u32) -> VideoRequirementProbe {
    let Some(chroma) = h264_chroma_subsampling_from_format_idc(chroma_format_idc) else {
        return VideoRequirementProbe::Rejected(
            VideoRequirementRejection::UnsupportedChromaFormat {
                codec: VideoCodec::H264,
                chroma_format_idc,
                reason: format!(
                    "H.264 chroma_format_idc {chroma_format_idc} is not supported by the v1 zero-copy path"
                ),
            },
        );
    };

    VideoRequirementProbe::Rejected(VideoRequirementRejection::UnsupportedChroma {
        codec: VideoCodec::H264,
        chroma,
        reason: format!("H.264 {chroma} stream is not supported by the v1 zero-copy path"),
    })
}

/// Переводит H.264 chroma id в общую model там, где это возможно.
fn h264_chroma_subsampling_from_format_idc(chroma_format_idc: u32) -> Option<ChromaSubsampling> {
    match chroma_format_idc {
        1 => Some(ChromaSubsampling::Yuv420),
        2 => Some(ChromaSubsampling::Yuv422),
        3 => Some(ChromaSubsampling::Yuv444),
        _ => None,
    }
}

/// Проверяет, что H.265 ошибка является policy reject-ом, а не recoverable uncertainty.
fn h265_requirement_error_is_policy_rejection(error: &H265RequirementError) -> bool {
    matches!(
        error,
        H265RequirementError::HevcDecoderConfigurationRecord(
            crate::HevcDecoderConfigurationRecordError::UnsupportedNalLengthSize { .. }
        ) | H265RequirementError::SequenceParameterSet(
            H265SpsError::UnsupportedProfile { .. }
                | H265SpsError::UnsupportedBitDepth { .. }
                | H265SpsError::UnsupportedChroma { .. }
                | H265SpsError::UnsupportedProfileFormat { .. }
        ) | H265RequirementError::UnsupportedProfile { .. }
            | H265RequirementError::UnsupportedBitDepth { .. }
            | H265RequirementError::UnsupportedChroma { .. }
            | H265RequirementError::UnsupportedProfileFormat { .. }
    )
}

/// Мапит H.265 typed errors в общий adapter result.
fn map_h265_requirement_error(error: H265RequirementError) -> VideoRequirementProbe {
    match error {
        H265RequirementError::HevcDecoderConfigurationRecord(
            crate::HevcDecoderConfigurationRecordError::UnsupportedNalLengthSize {
                nal_length_size,
            },
        ) => VideoRequirementProbe::Rejected(VideoRequirementRejection::UnsupportedPacketization {
            codec: VideoCodec::H265,
            reason: format!(
                "H.265 hvcC NAL length size {nal_length_size} is unsupported; expected 1, 2 or 4"
            ),
        }),
        H265RequirementError::SequenceParameterSet(H265SpsError::UnsupportedProfile {
            profile_idc,
            profile_compatibility_flags,
            reason,
        })
        | H265RequirementError::UnsupportedProfile {
            profile_idc,
            profile_compatibility_flags,
            reason,
        } => VideoRequirementProbe::Rejected(VideoRequirementRejection::UnsupportedProfile {
            codec: VideoCodec::H265,
            profile_idc,
            constraint_flags: (profile_compatibility_flags & 0xff) as u8,
            reason: format!(
                "{reason}; H.265 profile_compatibility_flags=0x{profile_compatibility_flags:08x}"
            ),
        }),
        H265RequirementError::SequenceParameterSet(H265SpsError::UnsupportedBitDepth {
            bit_depth_luma,
            bit_depth_chroma,
        })
        | H265RequirementError::UnsupportedBitDepth {
            bit_depth_luma,
            bit_depth_chroma,
        } => VideoRequirementProbe::Rejected(VideoRequirementRejection::UnsupportedBitDepth {
            codec: VideoCodec::H265,
            bit_depth: bit_depth_luma.max(bit_depth_chroma),
            reason: format!(
                "H.265 luma={bit_depth_luma}-bit/chroma={bit_depth_chroma}-bit stream is not supported by the v1 zero-copy path"
            ),
        }),
        H265RequirementError::SequenceParameterSet(H265SpsError::UnsupportedChroma {
            chroma_format_idc,
        })
        | H265RequirementError::UnsupportedChroma { chroma_format_idc } => {
            map_h265_unsupported_chroma(chroma_format_idc)
        }
        H265RequirementError::SequenceParameterSet(H265SpsError::UnsupportedProfileFormat {
            profile,
            bit_depth,
            chroma,
        })
        | H265RequirementError::UnsupportedProfileFormat {
            profile,
            bit_depth,
            chroma,
        } => VideoRequirementProbe::Rejected(VideoRequirementRejection::UnsupportedProfile {
            codec: VideoCodec::H265,
            profile_idc: h265_profile_idc(profile),
            constraint_flags: 0,
            reason: format!(
                "H.265 {profile} {bit_depth} {chroma} stream is not supported by the v1 zero-copy path"
            ),
        }),
        H265RequirementError::MissingSequenceParameterSet => VideoRequirementProbe::Recoverable(
            VideoRequirementUncertainty::MissingH265ParameterSet {
                codec: VideoCodec::H265,
                parameter_set: H265ParameterSetKind::Sequence,
            },
        ),
        other_error => {
            VideoRequirementProbe::Recoverable(VideoRequirementUncertainty::ParseError {
                codec: VideoCodec::H265,
                reason: other_error.to_string(),
            })
        }
    }
}

/// Мапит HEVC `chroma_format_idc` в максимально конкретный общий reject.
fn map_h265_unsupported_chroma(chroma_format_idc: u32) -> VideoRequirementProbe {
    let Some(chroma) = h265_chroma_subsampling_from_format_idc(chroma_format_idc) else {
        return VideoRequirementProbe::Rejected(
            VideoRequirementRejection::UnsupportedChromaFormat {
                codec: VideoCodec::H265,
                chroma_format_idc,
                reason: format!(
                    "H.265 chroma_format_idc {chroma_format_idc} is not supported by the v1 zero-copy path"
                ),
            },
        );
    };

    VideoRequirementProbe::Rejected(VideoRequirementRejection::UnsupportedChroma {
        codec: VideoCodec::H265,
        chroma,
        reason: format!("H.265 {chroma} stream is not supported by the v1 zero-copy path"),
    })
}

/// Переводит HEVC chroma id в общую model там, где это возможно.
fn h265_chroma_subsampling_from_format_idc(chroma_format_idc: u32) -> Option<ChromaSubsampling> {
    match chroma_format_idc {
        1 => Some(ChromaSubsampling::Yuv420),
        2 => Some(ChromaSubsampling::Yuv422),
        3 => Some(ChromaSubsampling::Yuv444),
        _ => None,
    }
}

/// Возвращает canonical profile_idc для typed HEVC profile diagnostic.
const fn h265_profile_idc(profile: crate::H265Profile) -> u8 {
    match profile {
        crate::H265Profile::Main => 1,
        crate::H265Profile::Main10 => 2,
        crate::H265Profile::Main12 => 3,
        crate::H265Profile::Main422_10 => 4,
        crate::H265Profile::Main422_12 => 5,
        crate::H265Profile::Main444 => 6,
        crate::H265Profile::Main444_10 => 7,
        crate::H265Profile::Main444_12 => 8,
        crate::H265Profile::SccMain => 9,
        crate::H265Profile::SccMain10 => 10,
        crate::H265Profile::SccMain444 => 11,
        crate::H265Profile::SccMain444_10 => 12,
    }
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
        .or_else(|| video_frame_pixel_layout_from_decode_requirement(&resolved.requirement));

    VideoResolvedMetadata {
        surface_format,
        color: Some(resolved.color),
        requirement: resolved.requirement,
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
) -> VideoFramePixelLayout {
    match decoded_format {
        Vp9DecodedFormatRequirement::Nv12 => VideoFramePixelLayout::Nv12,
        Vp9DecodedFormatRequirement::P010 => VideoFramePixelLayout::P010,
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

        assert_eq!(candidate.surface_format, Some(VideoFramePixelLayout::P010));
        assert_eq!(
            video_frame_pixel_layout_from_decode_requirement(&candidate.requirement),
            Some(VideoFramePixelLayout::P010)
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

    /// Проверяет, что неполный, но структурно валидный `hvcC` уточняет HEVC Main10 contract.
    #[test]
    fn h265_hvcc_header_refines_main10_requirement_without_parameter_sets() {
        let codec_private = h265_hvcc(4, 2, 1, 10);
        let packet_bytes =
            h265_hvcc_access_unit(crate::H265NalLengthSize::FOUR, &[h265_nal_unit(1, &[0x01])]);

        let probe = probe_video_packet_requirement_with_codec_private(
            VideoCodec::H265,
            &packet_bytes,
            Some(&codec_private),
        );

        let VideoRequirementProbe::Candidate(candidate) = probe else {
            panic!("H.265 Main10 hvcC должен дать candidate, получено {probe:?}");
        };
        assert_eq!(
            candidate.requirement.profile,
            Some(VideoProfile::H265(crate::H265Profile::Main10))
        );
        assert_eq!(candidate.requirement.bit_depth, Some(BitDepth::Ten));
        assert_eq!(
            candidate.requirement.chroma,
            Some(ChromaSubsampling::Yuv420)
        );
        assert_eq!(
            video_frame_pixel_layout_from_decode_requirement(&candidate.requirement),
            Some(VideoFramePixelLayout::P010)
        );
    }

    #[test]
    fn h265_hvcc_refinement_preserves_container_hdr_color_metadata() {
        let container_color = VideoColorMetadata::container(
            crate::ColorRange::Limited,
            crate::MatrixCoefficients::Bt2020,
            crate::ColorPrimaries::Bt2020,
            crate::TransferFunction::Pq,
            Some(crate::HdrMetadata {
                color_primaries: crate::ColorPrimaries::Bt2020,
                transfer_function: crate::TransferFunction::Pq,
                max_luminance_nits: Some(1_000.0),
                min_luminance_nits: Some(0.01),
                max_content_light_level_nits: Some(1_000),
                max_frame_average_light_level_nits: Some(400),
            }),
        );
        let container_source = VideoMetadataSource::container(VideoCodec::H265)
            .with_resolution(3840, 2160)
            .with_color(container_color.clone());
        let codec_private = h265_hvcc(4, 2, 1, 10);
        let packet_bytes =
            h265_hvcc_access_unit(crate::H265NalLengthSize::FOUR, &[h265_nal_unit(1, &[0x01])]);

        let probe = probe_video_packet_requirement_with_codec_private(
            VideoCodec::H265,
            &packet_bytes,
            Some(&codec_private),
        );
        let VideoRequirementProbe::Candidate(candidate) = probe else {
            panic!("H.265 Main10 hvcC должен дать candidate, получено {probe:?}");
        };
        let resolved =
            resolve_video_metadata(VideoCodec::H265, Some(container_source), Some(candidate));

        assert_eq!(
            resolved.requirement.profile,
            Some(VideoProfile::H265(crate::H265Profile::Main10))
        );
        assert_eq!(resolved.requirement.bit_depth, Some(BitDepth::Ten));
        assert_eq!(resolved.requirement.chroma, Some(ChromaSubsampling::Yuv420));
        assert_eq!(resolved.requirement.width, Some(3840));
        assert_eq!(resolved.requirement.height, Some(2160));
        assert_eq!(resolved.surface_format, Some(VideoFramePixelLayout::P010));
        assert_eq!(resolved.color, Some(container_color.clone()));
        assert_eq!(resolved.requirement.color, Some(container_color));
        assert!(resolved.requirement.hdr);
        assert!(resolved.requirement.color_pipeline.requires_hdr_processing);
    }

    /// Проверяет real-world `hev1`: parameter sets могут прийти in-band, а `hvcC` доказывает framing.
    #[test]
    fn h265_keyframe_probe_accepts_hev1_style_in_band_parameter_sets() {
        let codec_private = h265_hvcc(4, 1, 1, 8);
        let packet_bytes = h265_hvcc_access_unit(
            crate::H265NalLengthSize::FOUR,
            &[
                h265_nal_unit(32, &[0x01]),
                h265_nal_unit(33, &[0x01]),
                h265_nal_unit(34, &[0x01]),
                h265_nal_unit(21, &[0x01]),
            ],
        );

        let probe = probe_video_packet_keyframe_with_codec_private(
            VideoCodec::H265,
            &packet_bytes,
            Some(&codec_private),
        );

        assert_eq!(probe, VideoPacketKeyframeProbe::Keyframe(true));
    }

    /// Проверяет, что HEVC inter-frame не становится ложным decode-start.
    #[test]
    fn h265_keyframe_probe_distinguishes_inter_frame() {
        let codec_private = h265_hvcc(4, 1, 1, 8);
        let packet_bytes =
            h265_hvcc_access_unit(crate::H265NalLengthSize::FOUR, &[h265_nal_unit(1, &[0x01])]);

        let probe = probe_video_packet_keyframe_with_codec_private(
            VideoCodec::H265,
            &packet_bytes,
            Some(&codec_private),
        );

        assert_eq!(probe, VideoPacketKeyframeProbe::Keyframe(false));
    }

    /// Проверяет, что HEVC 4:2:2 остаётся typed unsupported, а не recoverable parse noise.
    #[test]
    fn h265_unsupported_chroma_maps_to_codec_neutral_reject() {
        let codec_private = h265_hvcc(4, 1, 2, 8);
        let packet_bytes =
            h265_hvcc_access_unit(crate::H265NalLengthSize::FOUR, &[h265_nal_unit(1, &[0x01])]);

        let probe = probe_video_packet_requirement_with_codec_private(
            VideoCodec::H265,
            &packet_bytes,
            Some(&codec_private),
        );

        assert!(matches!(
            probe,
            VideoRequirementProbe::Rejected(VideoRequirementRejection::UnsupportedChroma {
                codec: VideoCodec::H265,
                chroma: ChromaSubsampling::Yuv422,
                ..
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

    /// VP8 keyframe проходит через тот же codec-neutral adapter boundary.
    #[test]
    fn vp8_keyframe_probe_returns_codec_neutral_result() {
        let packet_bytes = [0xe0, 0x00, 0x00, 0x9d, 0x01, 0x2a, 0x40, 0x00, 0x40, 0x00];

        assert_eq!(
            probe_video_packet_keyframe(VideoCodec::Vp8, &packet_bytes),
            VideoPacketKeyframeProbe::Keyframe(true)
        );
    }

    /// Повреждённый VP8 keyframe остаётся recoverable uncertainty с typed причиной.
    #[test]
    fn vp8_keyframe_probe_reports_uncertain_parse_result() {
        let packet_bytes = [0xe0, 0x00, 0x00, 0x00, 0x01, 0x2a, 0x40, 0x00, 0x40, 0x00];

        let probe = probe_video_packet_keyframe(VideoCodec::Vp8, &packet_bytes);
        assert!(matches!(
            probe,
            VideoPacketKeyframeProbe::Uncertain(VideoRequirementUncertainty::ParseError {
                codec: VideoCodec::Vp8,
                ..
            })
        ));
    }

    /// Проверяет, что VP9 inter-frame не становится ложным keyframe.
    #[test]
    fn vp9_keyframe_probe_distinguishes_inter_frame() {
        let packet_bytes = build_vp9_inter_frame();

        let probe = probe_video_packet_keyframe(VideoCodec::Vp9, &packet_bytes);

        assert_eq!(probe, VideoPacketKeyframeProbe::Keyframe(false));
    }

    /// Проверяет, что parser uncertainty остаётся typed keyframe uncertainty.
    #[test]
    fn vp9_keyframe_probe_reports_uncertain_parse_result() {
        let probe = probe_video_packet_keyframe(VideoCodec::Vp9, b"\x00");

        assert!(matches!(
            probe,
            VideoPacketKeyframeProbe::Uncertain(VideoRequirementUncertainty::ParseError {
                codec: VideoCodec::Vp9,
                ..
            })
        ));
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

    /// Собирает VP9 inter-frame, который parser может уверенно классифицировать.
    fn build_vp9_inter_frame() -> Vec<u8> {
        let mut bits = Vec::new();
        push_bits(&mut bits, 0b10, 2);
        push_profile(&mut bits, 0);
        bits.push(0);
        bits.push(1);
        bits.push(1);
        bits.push(0);
        push_bits(&mut bits, 0, 2);
        push_bits(&mut bits, 0x01, 8);
        push_bits(&mut bits, 1, 3);
        bits.push(1);
        push_bits(&mut bits, 2, 3);
        bits.push(0);
        push_bits(&mut bits, 3, 3);
        bits.push(1);
        bits.push(0);
        bits.push(0);
        bits.push(0);
        push_bits(&mut bits, 63, 16);
        push_bits(&mut bits, 63, 16);
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

    /// Собирает минимальный `hvcC` record с header metadata и без parameter set arrays.
    fn h265_hvcc(
        nal_length_size: u8,
        profile_idc: u8,
        chroma_format_idc: u8,
        bit_depth: u8,
    ) -> Vec<u8> {
        let mut record_bytes = vec![
            1,
            profile_idc & 0x1f,
            0,
            0,
            0,
            0,
            0,
            0,
            0,
            0,
            0,
            0,
            120,
            0xf0,
            0x00,
            0xfc,
            0xfc | (chroma_format_idc & 0x03),
            0xf8 | ((bit_depth - 8) & 0x07),
            0xf8 | ((bit_depth - 8) & 0x07),
            0,
            0,
            0b0000_1100 | ((nal_length_size - 1) & 0x03),
            0,
        ];
        set_h265_profile_compatibility_flag(&mut record_bytes, profile_idc);
        record_bytes
    }

    /// Помечает profile compatibility flag так же, как это делает реальный `hvcC`.
    fn set_h265_profile_compatibility_flag(record_bytes: &mut [u8], profile_idc: u8) {
        let flag_index = usize::from(profile_idc);
        let byte_index = 2 + flag_index / 8;
        let bit_index = 7 - (flag_index % 8);
        record_bytes[byte_index] |= 1 << bit_index;
    }

    /// Собирает HEVC NAL unit с валидным двухбайтовым header-ом.
    fn h265_nal_unit(nal_unit_type: u8, payload: &[u8]) -> Vec<u8> {
        let mut nal_unit = vec![(nal_unit_type & 0x3f) << 1, 0x01];
        nal_unit.extend_from_slice(payload);
        nal_unit
    }

    /// Собирает length-prefixed HEVC access unit под выбранный `hvcC` length size.
    fn h265_hvcc_access_unit(
        nal_length_size: crate::H265NalLengthSize,
        nal_units: &[Vec<u8>],
    ) -> Vec<u8> {
        let mut access_unit = Vec::new();
        for nal_unit in nal_units {
            push_h265_nal_length(&mut access_unit, nal_length_size, nal_unit.len());
            access_unit.extend_from_slice(nal_unit);
        }
        access_unit
    }

    /// Пишет big-endian HEVC NAL length prefix.
    fn push_h265_nal_length(
        output_bytes: &mut Vec<u8>,
        nal_length_size: crate::H265NalLengthSize,
        nal_size: usize,
    ) {
        match nal_length_size.get() {
            1 => output_bytes.push(nal_size as u8),
            2 => output_bytes.extend_from_slice(&(nal_size as u16).to_be_bytes()),
            4 => output_bytes.extend_from_slice(&(nal_size as u32).to_be_bytes()),
            _ => unreachable!("test uses validated H265NalLengthSize"),
        }
    }
}
