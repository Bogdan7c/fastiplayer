//! Чистая проекция video track metadata в decoder/capability contracts.
//!
//! Модуль не меняет session state и не выбирает backend. Он только строит
//! typed requirement/config, уточняет допустимость packet probe и сохраняет
//! прежнее отображение neutral capability/decoder отказов в player errors.

use capability_core::{UnsupportedVideoRequirement, VideoCapabilityRejection};
use codec_core::{
    BitDepth, ChromaSubsampling, VideoCodec, VideoDecodeRequirement, VideoMetadataSource,
    resolve_video_metadata,
    unsupported_requirement_can_be_refined_by_packet_probe as codec_requirement_can_be_refined_by_packet_probe,
};
use media_core::TrackInfo;
use video_core::{VideoStreamConfigRejection, VideoStreamConfigResult, VideoStreamDecodeConfig};
use video_frame_contract::{DmaBufImageLayout, VideoFrameContract};

use crate::{PlayerError, PlayerErrorKind, PlayerResult};

use crate::session::video_packet_framing::{
    h264_packetization_from_track, h265_packetization_from_track,
};

/// Строит decoder stream config из уже принятого track requirement.
pub(in crate::session) fn video_stream_decode_config_from_track(
    track: &TrackInfo,
    requirement: &VideoDecodeRequirement,
    frame_contract: VideoFrameContract,
) -> PlayerResult<VideoStreamDecodeConfig> {
    let display_orientation = track
        .video
        .as_ref()
        .map(|metadata| metadata.orientation)
        .unwrap_or_default();
    let mut config =
        VideoStreamDecodeConfig::from_requirement(track.id, requirement, frame_contract)
            .with_codec_private(track.codec_private.clone())
            .with_display_orientation(display_orientation);

    match requirement.codec {
        VideoCodec::H264 => {
            config = config.with_packetization(h264_packetization_from_track(track)?);
        }
        VideoCodec::H265 => {
            config = config.with_packetization(h265_packetization_from_track(track)?);
        }
        _ => {}
    }

    Ok(config)
}

/// Возвращает explicit fallback contract только для no-capability legacy path.
///
/// Bit depth у VP9/HEVC часто неизвестен из container-а и приходит только из
/// bitstream probe. Для HDR/PQ 4:2:0 потоков это всегда 10-bit P010, поэтому
/// нельзя по умолчанию падать в NV12: иначе decoder сконфигурируется под NV12
/// и упадёт на реальном P010 DMA-BUF export-е до того, как refinement уточнит
/// bit depth.
pub(in crate::session) fn fallback_frame_contract_for_unprobed_requirement(
    requirement: &VideoDecodeRequirement,
) -> VideoFrameContract {
    let chroma_allows_p010 = !matches!(
        requirement.chroma,
        Some(ChromaSubsampling::Yuv422 | ChromaSubsampling::Yuv444)
    );
    let prefers_p010 = chroma_allows_p010
        && match requirement.bit_depth {
            Some(bit_depth) => bit_depth == BitDepth::Ten,
            None => requirement.requires_hdr_processing(),
        };

    if prefers_p010 {
        VideoFrameContract::dma_buf_p010(DmaBufImageLayout::SeparateLayers)
    } else {
        VideoFrameContract::dma_buf_nv12(DmaBufImageLayout::ComposedLayers)
    }
}

/// Переводит decoder configure outcome в player policy без мутации selection state.
pub(super) fn player_result_from_stream_config_result(
    result: VideoStreamConfigResult,
) -> PlayerResult<()> {
    match result {
        VideoStreamConfigResult::AbsentDecoder
        | VideoStreamConfigResult::Configured
        | VideoStreamConfigResult::Unchanged
        | VideoStreamConfigResult::Cleared => Ok(()),
        VideoStreamConfigResult::Unsupported(rejection) => {
            Err(player_error_from_config_rejection(rejection))
        }
        VideoStreamConfigResult::Backpressure(reason) => Err(PlayerError::new(
            PlayerErrorKind::RuntimeError,
            format!("Video decoder stream configure backpressure: {reason}"),
        )),
        VideoStreamConfigResult::Fatal(error) => Err(PlayerError::new(
            PlayerErrorKind::RuntimeError,
            format!("Video decoder stream configure failed: {error}"),
        )),
    }
}

/// Сохраняет существующую policy: unsupported track можно пропустить, runtime failure — нет.
pub(in crate::session) fn can_try_next_video_track_after_error(
    error_kind: &PlayerErrorKind,
) -> bool {
    matches!(
        error_kind,
        PlayerErrorKind::UnsupportedVideoCodec
            | PlayerErrorKind::UnsupportedVideoProfile
            | PlayerErrorKind::UnsupportedVideoBitDepth
            | PlayerErrorKind::UnsupportedVideoChroma
            | PlayerErrorKind::UnsupportedHdrMode
            | PlayerErrorKind::UnsupportedRenderFormat
    )
}

/// Мапит neutral decoder-stream отказ в существующие категории player errors.
fn player_error_from_config_rejection(rejection: VideoStreamConfigRejection) -> PlayerError {
    let kind = match &rejection {
        VideoStreamConfigRejection::UnsupportedCodec { .. }
        | VideoStreamConfigRejection::MissingPacketization { .. }
        | VideoStreamConfigRejection::InvalidCodecPrivate { .. }
        | VideoStreamConfigRejection::BackendUnsupported { .. } => {
            PlayerErrorKind::UnsupportedVideoCodec
        }
        VideoStreamConfigRejection::UnsupportedProfile { .. } => {
            PlayerErrorKind::UnsupportedVideoProfile
        }
        VideoStreamConfigRejection::UnsupportedBitDepth { .. } => {
            PlayerErrorKind::UnsupportedVideoBitDepth
        }
        VideoStreamConfigRejection::UnsupportedChroma { .. } => {
            PlayerErrorKind::UnsupportedVideoChroma
        }
        VideoStreamConfigRejection::UnsupportedSurfaceFormat { .. }
        | VideoStreamConfigRejection::UnsupportedFrameContract { .. } => {
            PlayerErrorKind::UnsupportedRenderFormat
        }
    };

    PlayerError::new(kind, rejection.to_string())
}

/// Строит минимальное decode requirement из container track metadata.
pub(super) fn video_requirement_from_track(track: &TrackInfo) -> Option<VideoDecodeRequirement> {
    let codec = VideoCodec::from_container_codec_id(&track.codec_id)?;
    let Some(container_source) = video_metadata_source_from_track(track) else {
        return Some(VideoDecodeRequirement::new(codec));
    };

    Some(resolve_video_metadata(codec, Some(container_source), None).requirement)
}

/// Проверяет, что отказ относится к metadata, которую codec packet probe может уточнить.
pub(super) fn unsupported_requirement_can_be_refined_by_packet_probe(
    unsupported_requirement: &UnsupportedVideoRequirement,
) -> bool {
    let is_metadata_rejection = matches!(
        unsupported_requirement.rejections.first(),
        Some(VideoCapabilityRejection::InvalidHdrMetadata { .. })
            | Some(VideoCapabilityRejection::InsufficientStreamMetadata { .. })
    );

    codec_requirement_can_be_refined_by_packet_probe(
        &unsupported_requirement.requirement,
        is_metadata_rejection,
    )
}

/// Собирает codec-neutral resolver source из typed video metadata track-а.
pub(in crate::session) fn video_metadata_source_from_track(
    track: &TrackInfo,
) -> Option<VideoMetadataSource> {
    let codec = VideoCodec::from_container_codec_id(&track.codec_id)?;
    let video = track.video.as_ref()?;
    let mut source = VideoMetadataSource::container(codec);
    source.profile = video.profile;
    source.bit_depth = video.bit_depth;
    source.chroma = video.chroma;
    source.width = video.coded_width;
    source.height = video.coded_height;
    if let Some(color) = &video.color {
        source = source.with_color(color.clone());
    }
    Some(source)
}

/// Переводит structured capability error в player error model.
pub(in crate::session) fn player_error_from_unsupported_requirement(
    error: UnsupportedVideoRequirement,
) -> PlayerError {
    let kind = match error.rejections.first() {
        Some(VideoCapabilityRejection::UnsupportedCodec { .. }) => {
            PlayerErrorKind::UnsupportedVideoCodec
        }
        Some(VideoCapabilityRejection::UnsupportedProfile { .. }) => {
            PlayerErrorKind::UnsupportedVideoProfile
        }
        Some(VideoCapabilityRejection::UnsupportedBitDepth { .. }) => {
            PlayerErrorKind::UnsupportedVideoBitDepth
        }
        Some(VideoCapabilityRejection::UnsupportedChroma { .. }) => {
            PlayerErrorKind::UnsupportedVideoChroma
        }
        Some(VideoCapabilityRejection::UnsupportedHdrRenderer { .. }) => {
            PlayerErrorKind::UnsupportedHdrMode
        }
        Some(VideoCapabilityRejection::InvalidHdrMetadata { .. }) => {
            PlayerErrorKind::UnsupportedHdrMode
        }
        Some(VideoCapabilityRejection::P010NotRenderable { .. }) if error.requirement.hdr => {
            PlayerErrorKind::UnsupportedHdrMode
        }
        Some(VideoCapabilityRejection::NoAvailableRenderer)
        | Some(VideoCapabilityRejection::UnsupportedBackendFrameTransfer { .. })
        | Some(VideoCapabilityRejection::UnsupportedDmaBufImageLayout { .. })
        | Some(VideoCapabilityRejection::UnsupportedRenderFrameFormat { .. })
        | Some(VideoCapabilityRejection::UnsupportedRenderFrameTransfer { .. })
        | Some(VideoCapabilityRejection::RenderTextureSizeExceeded { .. })
        | Some(VideoCapabilityRejection::P010NotRenderable { .. }) => {
            PlayerErrorKind::UnsupportedRenderFormat
        }
        Some(VideoCapabilityRejection::NoAvailableBackend)
        | Some(VideoCapabilityRejection::UnsupportedDecodeFormat { .. })
        | Some(VideoCapabilityRejection::InsufficientStreamMetadata { .. })
        | None => PlayerErrorKind::HardwareDecoderUnavailable,
    };

    PlayerError::new(kind, error.user_message())
}

#[cfg(test)]
mod tests {
    use super::*;
    use codec_core::{
        ColorPrimaries, ColorRange, MatrixCoefficients, TransferFunction, VideoColorMetadata,
    };
    use video_frame_contract::VideoFramePixelLayout;

    fn bt2020_pq_container() -> VideoColorMetadata {
        VideoColorMetadata::container(
            ColorRange::Limited,
            MatrixCoefficients::Bt2020,
            ColorPrimaries::Bt2020,
            TransferFunction::Pq,
            None,
        )
    }

    #[test]
    fn hdr_unprobed_requirement_falls_back_to_p010() {
        let requirement =
            VideoDecodeRequirement::new(VideoCodec::Vp9).with_color(bt2020_pq_container());

        let contract = fallback_frame_contract_for_unprobed_requirement(&requirement);

        assert_eq!(contract.pixel_layout, VideoFramePixelLayout::P010);
    }

    #[test]
    fn sdr_unprobed_requirement_falls_back_to_nv12() {
        let requirement = VideoDecodeRequirement::new(VideoCodec::Vp9);

        let contract = fallback_frame_contract_for_unprobed_requirement(&requirement);

        assert_eq!(contract.pixel_layout, VideoFramePixelLayout::Nv12);
    }

    #[test]
    fn explicit_ten_bit_requirement_falls_back_to_p010() {
        let requirement = VideoDecodeRequirement::new(VideoCodec::H265)
            .with_bit_depth(BitDepth::Ten)
            .with_chroma(ChromaSubsampling::Yuv420);

        let contract = fallback_frame_contract_for_unprobed_requirement(&requirement);

        assert_eq!(contract.pixel_layout, VideoFramePixelLayout::P010);
    }

    #[test]
    fn explicit_eight_bit_requirement_falls_back_to_nv12() {
        let requirement = VideoDecodeRequirement::new(VideoCodec::H265)
            .with_bit_depth(BitDepth::Eight)
            .with_chroma(ChromaSubsampling::Yuv420);

        let contract = fallback_frame_contract_for_unprobed_requirement(&requirement);

        assert_eq!(contract.pixel_layout, VideoFramePixelLayout::Nv12);
    }
}
