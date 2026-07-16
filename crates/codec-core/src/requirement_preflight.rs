//! Единый codec registry для preflight-а точного decode requirement.
//!
//! Этот модуль намеренно содержит исчерпывающий `match` по [`VideoCodec`].
//! Поэтому добавление нового codec-а не сможет молча унаследовать неподходящий
//! fallback: автор расширения обязан явно выбрать источник достоверных полей.

use crate::{
    BitDepth, ChromaSubsampling, VideoCodec, VideoDecodeRequirement, VideoMetadataSource,
    VideoProfile, VideoRequirementCandidate, VideoRequirementProbe, VideoRequirementRejection,
    VideoResolvedMetadata, Vp8Profile, av1_decode_requirement_from_decoder_configuration_record,
    probe_video_packet_requirement_with_codec_private, resolve_video_metadata,
};

/// Источник доказательства, достаточного для точного capability selection.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum VideoRequirementEvidencePolicy {
    /// Container metadata достаточно только когда в ней есть все обязательные поля;
    /// иначе нужен header encoded packet-а.
    PacketWhenContainerIncomplete,

    /// Сначала читается codec-private record, а при его отсутствии или неполноте — packet.
    CodecPrivateThenPacket,

    /// Полный container snapshot принимается сразу, иначе обязателен codec-private record.
    CodecPrivateWhenContainerIncomplete,

    /// Формат однозначно задаётся самим codec contract-ом.
    FixedCodecContract,
}

/// Возвращает единственную production policy получения точного requirement для codec-а.
///
/// Исчерпывающий `match` — compile-time guard для будущего расширения [`VideoCodec`].
#[must_use]
pub const fn video_requirement_evidence_policy(
    codec: VideoCodec,
) -> VideoRequirementEvidencePolicy {
    match codec {
        VideoCodec::Vp9 => VideoRequirementEvidencePolicy::PacketWhenContainerIncomplete,
        VideoCodec::Av1 => VideoRequirementEvidencePolicy::CodecPrivateWhenContainerIncomplete,
        VideoCodec::H264 | VideoCodec::H265 => {
            VideoRequirementEvidencePolicy::CodecPrivateThenPacket
        }
        VideoCodec::Vp8 => VideoRequirementEvidencePolicy::FixedCodecContract,
    }
}

/// Причина, по которой registry не может получить точный requirement до decode.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum VideoRequirementPreflightUnavailable {
    /// Codec требует container configuration record, но demuxer его не предоставил.
    MissingCodecPrivate {
        /// Codec проблемного stream-а.
        codec: VideoCodec,
    },

    /// Container configuration record присутствует, но не прошёл codec parser.
    InvalidCodecPrivate {
        /// Codec проблемного stream-а.
        codec: VideoCodec,

        /// Диагностическая причина codec parser-а.
        reason: String,
    },
}

impl VideoRequirementPreflightUnavailable {
    /// Возвращает понятное пользователю описание preflight failure.
    #[must_use]
    pub fn user_message(&self) -> String {
        match self {
            Self::MissingCodecPrivate { codec } => format!(
                "Для {} не хватает обязательного codec-private заголовка с точным форматом видео",
                codec.display_name()
            ),
            Self::InvalidCodecPrivate { codec, reason } => format!(
                "Codec-private заголовок {} повреждён или не поддерживается: {reason}",
                codec.display_name()
            ),
        }
    }
}

/// Результат registry preflight-а до чтения packet-а из candidate demuxer-а.
#[derive(Debug, Clone, PartialEq)]
pub enum VideoRequirementPreflight {
    /// Requirement уже подтверждён container/private/fixed codec evidence.
    Resolved(VideoResolvedMetadata),

    /// Для точного requirement нужен encoded packet; initial metadata сохраняется для merge.
    PacketProbeRequired(VideoResolvedMetadata),

    /// Header валиден, но stream variant запрещён production policy.
    Rejected(VideoRequirementRejection),

    /// У текущего codec path нет безопасного способа закончить preflight.
    Unavailable {
        /// Неполный initial snapshot нужен для диагностики и compatibility path-а.
        initial: VideoResolvedMetadata,

        /// Typed причина отсутствия доказательства.
        reason: VideoRequirementPreflightUnavailable,
    },
}

/// Строит requirement из container metadata и codec-private data по единому registry.
///
/// Packet bytes здесь намеренно не читаются: ownership demuxer-а остаётся у player layer.
/// Если они нужны, caller получает [`VideoRequirementPreflight::PacketProbeRequired`].
#[must_use]
pub fn preflight_video_requirement(
    codec: VideoCodec,
    container_source: Option<VideoMetadataSource>,
    codec_private: Option<&[u8]>,
) -> VideoRequirementPreflight {
    let initial = resolve_video_metadata(codec, container_source.clone(), None);

    match video_requirement_evidence_policy(codec) {
        VideoRequirementEvidencePolicy::PacketWhenContainerIncomplete => {
            if vp9_container_evidence_is_complete(&initial.requirement) {
                VideoRequirementPreflight::Resolved(initial)
            } else {
                VideoRequirementPreflight::PacketProbeRequired(initial)
            }
        }
        VideoRequirementEvidencePolicy::CodecPrivateThenPacket => {
            preflight_h26x_requirement(codec, container_source, codec_private, initial)
        }
        VideoRequirementEvidencePolicy::CodecPrivateWhenContainerIncomplete => {
            preflight_av1_requirement(container_source, codec_private, initial)
        }
        VideoRequirementEvidencePolicy::FixedCodecContract => {
            let fixed_requirement = VideoDecodeRequirement::new(VideoCodec::Vp8)
                .with_profile(VideoProfile::Vp8(Vp8Profile::Version0To3))
                .with_bit_depth(BitDepth::Eight)
                .with_chroma(ChromaSubsampling::Yuv420);
            let candidate = VideoRequirementCandidate::generic(fixed_requirement);
            VideoRequirementPreflight::Resolved(resolve_video_metadata(
                VideoCodec::Vp8,
                container_source,
                Some(candidate),
            ))
        }
    }
}

/// Завершает H.264/H.265 preflight через `avcC`/`hvcC` или запрашивает packet fallback.
fn preflight_h26x_requirement(
    codec: VideoCodec,
    container_source: Option<VideoMetadataSource>,
    codec_private: Option<&[u8]>,
    initial: VideoResolvedMetadata,
) -> VideoRequirementPreflight {
    if let Some(codec_private) = codec_private.filter(|bytes| !bytes.is_empty()) {
        // H.264/H.265 registry сначала разбирает parameter sets из codec-private.
        // Пустой packet не является данными stream-а: существующий adapter boundary
        // явно умеет fallback-ить к packet только когда private record недостаточен.
        match probe_video_packet_requirement_with_codec_private(codec, &[], Some(codec_private)) {
            VideoRequirementProbe::Candidate(candidate) => {
                return VideoRequirementPreflight::Resolved(resolve_video_metadata(
                    codec,
                    container_source,
                    Some(candidate),
                ));
            }
            VideoRequirementProbe::Rejected(rejection) => {
                return VideoRequirementPreflight::Rejected(rejection);
            }
            VideoRequirementProbe::Recoverable(_) => {
                return VideoRequirementPreflight::PacketProbeRequired(initial);
            }
        }
    }

    if h26x_container_evidence_is_complete(&initial.requirement) {
        VideoRequirementPreflight::Resolved(initial)
    } else {
        VideoRequirementPreflight::PacketProbeRequired(initial)
    }
}

/// Завершает AV1 preflight через уже нормализованный container snapshot или `av1C`.
fn preflight_av1_requirement(
    container_source: Option<VideoMetadataSource>,
    codec_private: Option<&[u8]>,
    initial: VideoResolvedMetadata,
) -> VideoRequirementPreflight {
    if let Some(codec_private) = codec_private.filter(|bytes| !bytes.is_empty()) {
        return match av1_decode_requirement_from_decoder_configuration_record(codec_private) {
            Ok(requirement) => {
                let candidate = VideoRequirementCandidate::generic(requirement);
                VideoRequirementPreflight::Resolved(resolve_video_metadata(
                    VideoCodec::Av1,
                    container_source,
                    Some(candidate),
                ))
            }
            Err(error) => VideoRequirementPreflight::Unavailable {
                initial,
                reason: VideoRequirementPreflightUnavailable::InvalidCodecPrivate {
                    codec: VideoCodec::Av1,
                    reason: error.to_string(),
                },
            },
        };
    }

    if av1_container_evidence_is_complete(&initial.requirement) {
        VideoRequirementPreflight::Resolved(initial)
    } else {
        VideoRequirementPreflight::Unavailable {
            initial,
            reason: VideoRequirementPreflightUnavailable::MissingCodecPrivate {
                codec: VideoCodec::Av1,
            },
        }
    }
}

/// Проверяет поля VP9, которые влияют на backend, surface contract и HDR policy.
fn vp9_container_evidence_is_complete(requirement: &VideoDecodeRequirement) -> bool {
    requirement.profile.is_some()
        && requirement.bit_depth.is_some()
        && requirement.chroma.is_some()
        && requirement.width.is_some()
        && requirement.height.is_some()
        && requirement.color.is_some()
}

/// Проверяет H.264/H.265 container evidence без предположений о скрытом VUI.
fn h26x_container_evidence_is_complete(requirement: &VideoDecodeRequirement) -> bool {
    requirement.profile.is_some()
        && requirement.bit_depth.is_some()
        && requirement.chroma.is_some()
        && requirement.width.is_some()
        && requirement.height.is_some()
        && requirement.color.is_some()
}

/// Проверяет поля AV1, которые текущий `av1C` adapter способен подтвердить до decode.
fn av1_container_evidence_is_complete(requirement: &VideoDecodeRequirement) -> bool {
    requirement.profile.is_some()
        && requirement.bit_depth.is_some()
        && requirement.chroma.is_some()
        && requirement.width.is_some()
        && requirement.height.is_some()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{
        Av1Profile, ColorPrimaries, ColorRange, H265Profile, MatrixCoefficients, TransferFunction,
        VideoColorMetadata, VideoProfile,
    };

    /// Закрепляет явную evidence policy каждого поддерживаемого codec-а.
    #[test]
    fn every_supported_codec_has_explicit_preflight_policy() {
        assert_eq!(
            video_requirement_evidence_policy(VideoCodec::Vp9),
            VideoRequirementEvidencePolicy::PacketWhenContainerIncomplete
        );
        assert_eq!(
            video_requirement_evidence_policy(VideoCodec::Av1),
            VideoRequirementEvidencePolicy::CodecPrivateWhenContainerIncomplete
        );
        assert_eq!(
            video_requirement_evidence_policy(VideoCodec::H264),
            VideoRequirementEvidencePolicy::CodecPrivateThenPacket
        );
        assert_eq!(
            video_requirement_evidence_policy(VideoCodec::H265),
            VideoRequirementEvidencePolicy::CodecPrivateThenPacket
        );
        assert_eq!(
            video_requirement_evidence_policy(VideoCodec::Vp8),
            VideoRequirementEvidencePolicy::FixedCodecContract
        );
    }

    /// AV1 codec-private record должен давать точный profile/depth/chroma contract.
    #[test]
    fn av1_codec_private_resolves_incomplete_container_requirement() {
        let container_source =
            VideoMetadataSource::container(VideoCodec::Av1).with_resolution(3_840, 2_160);
        let av1c_main_8bit_yuv420 = [0x81, 0x00, 0x0c, 0x00];

        let VideoRequirementPreflight::Resolved(resolved) = preflight_video_requirement(
            VideoCodec::Av1,
            Some(container_source),
            Some(&av1c_main_8bit_yuv420),
        ) else {
            panic!("валидный av1C должен завершить preflight");
        };

        assert_eq!(
            resolved.requirement.profile,
            Some(VideoProfile::Av1(Av1Profile::Main))
        );
        assert_eq!(resolved.requirement.bit_depth, Some(BitDepth::Eight));
        assert_eq!(resolved.requirement.chroma, Some(ChromaSubsampling::Yuv420));
        assert_eq!(resolved.requirement.width, Some(3_840));
        assert_eq!(resolved.requirement.height, Some(2_160));
    }

    /// Неполный AV1 без codec-private не должен молча считаться 8-bit SDR.
    #[test]
    fn incomplete_av1_without_codec_private_is_typed_unavailable() {
        let preflight = preflight_video_requirement(VideoCodec::Av1, None, None);

        assert!(matches!(
            preflight,
            VideoRequirementPreflight::Unavailable {
                reason: VideoRequirementPreflightUnavailable::MissingCodecPrivate {
                    codec: VideoCodec::Av1
                },
                ..
            }
        ));
    }

    /// H.265 `hvcC` должен уточнить Main10/P010, не потеряв container HDR color.
    #[test]
    fn h265_codec_private_resolves_main10_and_preserves_container_hdr() {
        let hdr_color = VideoColorMetadata::container(
            ColorRange::Limited,
            MatrixCoefficients::Bt2020,
            ColorPrimaries::Bt2020,
            TransferFunction::Pq,
            None,
        );
        let container_source = VideoMetadataSource::container(VideoCodec::H265)
            .with_resolution(3_840, 2_160)
            .with_color(hdr_color.clone());
        let hvcc = h265_hvcc(4, 2, 1, 10);

        let VideoRequirementPreflight::Resolved(resolved) =
            preflight_video_requirement(VideoCodec::H265, Some(container_source), Some(&hvcc))
        else {
            panic!("валидный hvcC должен завершить H.265 preflight");
        };

        assert_eq!(
            resolved.requirement.profile,
            Some(VideoProfile::H265(H265Profile::Main10))
        );
        assert_eq!(resolved.requirement.bit_depth, Some(BitDepth::Ten));
        assert_eq!(resolved.requirement.chroma, Some(ChromaSubsampling::Yuv420));
        assert_eq!(resolved.requirement.color, Some(hdr_color));
        assert!(resolved.requirement.requires_hdr_processing());
    }

    /// VP8 fixed contract заполняет поля, которые container обычно не сообщает.
    #[test]
    fn vp8_fixed_contract_is_capability_ready() {
        let VideoRequirementPreflight::Resolved(resolved) =
            preflight_video_requirement(VideoCodec::Vp8, None, None)
        else {
            panic!("VP8 fixed contract должен быть готов без packet probe");
        };

        assert_eq!(
            resolved.requirement.profile,
            Some(VideoProfile::Vp8(Vp8Profile::Version0To3))
        );
        assert_eq!(resolved.requirement.bit_depth, Some(BitDepth::Eight));
        assert_eq!(resolved.requirement.chroma, Some(ChromaSubsampling::Yuv420));
    }

    /// Собирает минимальный `hvcC` record с header metadata и без parameter sets.
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

    /// Помечает profile compatibility flag так же, как реальный `hvcC`.
    fn set_h265_profile_compatibility_flag(record_bytes: &mut [u8], profile_idc: u8) {
        let flag_index = usize::from(profile_idc);
        let byte_index = 2 + flag_index / 8;
        let bit_index = 7 - (flag_index % 8);
        record_bytes[byte_index] |= 1 << bit_index;
    }
}
