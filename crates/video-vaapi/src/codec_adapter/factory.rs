use super::h264::{H264VaapiCodecAdapter, H264VaapiStreamConfig};
use super::h265::{H265VaapiCodecAdapter, H265VaapiStreamConfig};
use super::vp9::Vp9VaapiCodecAdapter;
use super::*;
/// Factory/registry production adapter-ов VAAPI backend-а.
#[derive(Debug, Default, Clone, Copy)]
pub(crate) struct VaapiCodecAdapterFactory;

impl VaapiCodecAdapterFactory {
    /// Создаёт production adapter для текущего backend default-а.
    pub(crate) fn create_default_adapter(
        display: Rc<Display>,
    ) -> Result<Box<dyn VaapiCodecAdapter>> {
        Ok(Box::new(Vp9VaapiCodecAdapter::new(display)?))
    }

    /// Создаёт adapter, который соответствует уже принятому stream config-у.
    pub(crate) fn create_adapter_for_config(
        display: Rc<Display>,
        config: &VideoStreamDecodeConfig,
    ) -> Result<Box<dyn VaapiCodecAdapter>> {
        if let Some(rejection) = Self::stream_config_rejection(config) {
            return Err(anyhow::anyhow!(
                "Unsupported VA-API stream config: {rejection}"
            ));
        }

        match config.codec {
            VideoCodec::Vp9 => Ok(Box::new(Vp9VaapiCodecAdapter::new(display)?)),
            VideoCodec::H264 => Ok(Box::new(H264VaapiCodecAdapter::new(display, config)?)),
            VideoCodec::H265 => Ok(Box::new(H265VaapiCodecAdapter::new(display, config)?)),
            codec @ (VideoCodec::Av1 | VideoCodec::Vp8) => Err(anyhow::anyhow!(
                "VA-API adapter factory has no implemented adapter for {codec}"
            )),
        }
    }

    /// Возвращает typed отказ, если stream config не входит в implemented adapter matrix.
    pub(crate) fn stream_config_rejection(
        config: &VideoStreamDecodeConfig,
    ) -> Option<VideoStreamConfigRejection> {
        match config.codec {
            VideoCodec::Vp9 => reject_unsupported_vp9_config(config),
            VideoCodec::H264 => reject_unsupported_h264_config(config),
            VideoCodec::H265 => reject_unsupported_h265_config(config),
            codec @ (VideoCodec::Av1 | VideoCodec::Vp8) => {
                Some(VideoStreamConfigRejection::UnsupportedCodec { codec })
            }
        }
    }

    /// Проверяет, что probed hardware format имеет production adapter в этом crate-е.
    pub(crate) fn supports_decode_format(format: &SupportedVideoDecodeFormat) -> bool {
        match (format.codec, format.profile) {
            (VideoCodec::Vp9, VideoProfile::Vp9(Vp9Profile::Profile0)) => {
                format.bit_depth == BitDepth::Eight && format.chroma == ChromaSubsampling::Yuv420
            }
            (VideoCodec::Vp9, VideoProfile::Vp9(Vp9Profile::Profile2)) => {
                format.bit_depth == BitDepth::Ten && format.chroma == ChromaSubsampling::Yuv420
            }
            (VideoCodec::H264, VideoProfile::H264(_)) => {
                format.bit_depth == BitDepth::Eight && format.chroma == ChromaSubsampling::Yuv420
            }
            (VideoCodec::H265, VideoProfile::H265(H265Profile::Main)) => {
                format.bit_depth == BitDepth::Eight && format.chroma == ChromaSubsampling::Yuv420
            }
            (VideoCodec::H265, VideoProfile::H265(H265Profile::Main10)) => {
                format.bit_depth == BitDepth::Ten && format.chroma == ChromaSubsampling::Yuv420
            }
            _ => false,
        }
    }
}

/// Валидирует VP9 config против production adapter matrix.
fn reject_unsupported_vp9_config(
    config: &VideoStreamDecodeConfig,
) -> Option<VideoStreamConfigRejection> {
    if let Some(rejection) = reject_unsupported_frame_contract(config) {
        return Some(rejection);
    }

    let surface_format = Some(config.frame_contract.pixel_layout);

    if let Some(profile) = config.profile {
        let profile_rejection = match profile {
            VideoProfile::Vp9(Vp9Profile::Profile0) => {
                reject_optional_bit_depth(config.bit_depth, BitDepth::Eight)
                    .or_else(|| reject_optional_chroma(config.chroma, ChromaSubsampling::Yuv420))
                    .or_else(|| {
                        reject_optional_surface(surface_format, VideoFramePixelLayout::Nv12)
                    })
            }
            VideoProfile::Vp9(Vp9Profile::Profile2) => {
                reject_optional_bit_depth(config.bit_depth, BitDepth::Ten)
                    .or_else(|| reject_optional_chroma(config.chroma, ChromaSubsampling::Yuv420))
                    .or_else(|| {
                        reject_optional_surface(surface_format, VideoFramePixelLayout::P010)
                    })
            }
            VideoProfile::Vp9(_) => {
                Some(VideoStreamConfigRejection::UnsupportedProfile { profile })
            }
            profile => Some(VideoStreamConfigRejection::UnsupportedProfile { profile }),
        };
        if profile_rejection.is_some() {
            return profile_rejection;
        }
    } else if let Some(rejection) = reject_vp9_without_profile(config) {
        return Some(rejection);
    }

    if config.packetization.is_some() {
        return Some(VideoStreamConfigRejection::BackendUnsupported {
            reason: "VP9 VA-API adapter does not accept codec-specific packetization metadata"
                .to_string(),
        });
    }

    None
}

/// Валидирует VP9 config, когда profile ещё не доказан до packet-level refinement.
fn reject_vp9_without_profile(
    config: &VideoStreamDecodeConfig,
) -> Option<VideoStreamConfigRejection> {
    if let Some(bit_depth) = config.bit_depth
        && !matches!(bit_depth, BitDepth::Eight | BitDepth::Ten)
    {
        return Some(VideoStreamConfigRejection::UnsupportedBitDepth { bit_depth });
    }

    if let Some(chroma) = config.chroma
        && chroma != ChromaSubsampling::Yuv420
    {
        return Some(VideoStreamConfigRejection::UnsupportedChroma { chroma });
    }

    let surface_format = config.frame_contract.pixel_layout;
    if !matches!(
        surface_format,
        VideoFramePixelLayout::Nv12 | VideoFramePixelLayout::P010
    ) {
        return Some(VideoStreamConfigRejection::UnsupportedSurfaceFormat { surface_format });
    }

    None
}

/// Валидирует H.264 stream config против production adapter-а.
fn reject_unsupported_h264_config(
    config: &VideoStreamDecodeConfig,
) -> Option<VideoStreamConfigRejection> {
    if let Some(rejection) = reject_unsupported_frame_contract(config) {
        return Some(rejection);
    }

    if let Some(profile) = config.profile
        && !matches!(profile, VideoProfile::H264(_))
    {
        return Some(VideoStreamConfigRejection::UnsupportedProfile { profile });
    }

    if let Some(rejection) = reject_optional_bit_depth(config.bit_depth, BitDepth::Eight)
        .or_else(|| reject_optional_chroma(config.chroma, ChromaSubsampling::Yuv420))
        .or_else(|| {
            reject_optional_surface(
                Some(config.frame_contract.pixel_layout),
                VideoFramePixelLayout::Nv12,
            )
        })
    {
        return Some(rejection);
    }

    if !matches!(
        config.packetization,
        Some(VideoStreamPacketization::H264(_))
    ) {
        return Some(VideoStreamConfigRejection::MissingPacketization {
            codec: VideoCodec::H264,
        });
    }

    H264VaapiStreamConfig::from_decode_config(config).err()
}

/// Валидирует H.265 stream config против подготовленного VAAPI adapter path-а.
fn reject_unsupported_h265_config(
    config: &VideoStreamDecodeConfig,
) -> Option<VideoStreamConfigRejection> {
    if let Some(rejection) = reject_unsupported_frame_contract(config) {
        return Some(rejection);
    }

    if let Some(rejection) = reject_h265_declared_format(
        config.profile,
        config.bit_depth,
        config.chroma,
        Some(config.frame_contract.pixel_layout),
    ) {
        return Some(rejection);
    }

    if !matches!(
        config.packetization,
        Some(VideoStreamPacketization::H265(_))
    ) {
        return Some(VideoStreamConfigRejection::MissingPacketization {
            codec: VideoCodec::H265,
        });
    }

    if let Some(rejection) = reject_h265_codec_private_requirement(config) {
        return Some(rejection);
    }

    H265VaapiStreamConfig::from_decode_config(config).err()
}

/// Проверяет уже известные HEVC profile/format поля без требования полного hvcC.
fn reject_h265_declared_format(
    profile: Option<VideoProfile>,
    bit_depth: Option<BitDepth>,
    chroma: Option<ChromaSubsampling>,
    surface_format: Option<VideoFramePixelLayout>,
) -> Option<VideoStreamConfigRejection> {
    match profile {
        Some(VideoProfile::H265(H265Profile::Main)) => {
            reject_optional_bit_depth(bit_depth, BitDepth::Eight)
                .or_else(|| reject_optional_chroma(chroma, ChromaSubsampling::Yuv420))
                .or_else(|| reject_optional_surface(surface_format, VideoFramePixelLayout::Nv12))
        }
        Some(VideoProfile::H265(H265Profile::Main10)) => {
            reject_optional_bit_depth(bit_depth, BitDepth::Ten)
                .or_else(|| reject_optional_chroma(chroma, ChromaSubsampling::Yuv420))
                .or_else(|| reject_optional_surface(surface_format, VideoFramePixelLayout::P010))
        }
        Some(unsupported_profile) => Some(VideoStreamConfigRejection::UnsupportedProfile {
            profile: unsupported_profile,
        }),
        None => reject_h265_without_profile(bit_depth, chroma, surface_format),
    }
}

/// Валидирует HEVC config, когда profile ещё придёт из in-band SPS.
fn reject_h265_without_profile(
    bit_depth: Option<BitDepth>,
    chroma: Option<ChromaSubsampling>,
    surface_format: Option<VideoFramePixelLayout>,
) -> Option<VideoStreamConfigRejection> {
    if let Some(bit_depth) = bit_depth
        && !matches!(bit_depth, BitDepth::Eight | BitDepth::Ten)
    {
        return Some(VideoStreamConfigRejection::UnsupportedBitDepth { bit_depth });
    }

    if let Some(chroma) = chroma
        && chroma != ChromaSubsampling::Yuv420
    {
        return Some(VideoStreamConfigRejection::UnsupportedChroma { chroma });
    }

    if let Some(surface_format) = surface_format
        && !matches!(
            surface_format,
            VideoFramePixelLayout::Nv12 | VideoFramePixelLayout::P010
        )
    {
        return Some(VideoStreamConfigRejection::UnsupportedSurfaceFormat { surface_format });
    }

    match (bit_depth, surface_format) {
        (Some(BitDepth::Eight), Some(surface_format @ VideoFramePixelLayout::P010))
        | (Some(BitDepth::Ten), Some(surface_format @ VideoFramePixelLayout::Nv12)) => {
            Some(VideoStreamConfigRejection::UnsupportedSurfaceFormat { surface_format })
        }
        _ => None,
    }
}

/// Проверяет hvcC header/SPS, если codec_private есть, но не требует VPS/SPS/PPS arrays.
fn reject_h265_codec_private_requirement(
    config: &VideoStreamDecodeConfig,
) -> Option<VideoStreamConfigRejection> {
    let codec_private = config
        .codec_private
        .as_deref()
        .filter(|bytes| !bytes.is_empty())?;

    let requirement =
        match h265_decode_requirement_from_hevc_decoder_configuration_record(codec_private) {
            Ok(requirement) => requirement,
            Err(error) => {
                return Some(VideoStreamConfigRejection::InvalidCodecPrivate {
                    codec: VideoCodec::H265,
                    reason: error.to_string(),
                });
            }
        };

    reject_h265_declared_format(
        requirement.profile,
        requirement.bit_depth,
        requirement.chroma,
        video_frame_pixel_layout_from_decode_requirement(&requirement),
    )
}

/// Проверяет, что VAAPI stream config требует hardware DMA-BUF output.
fn reject_unsupported_frame_contract(
    config: &VideoStreamDecodeConfig,
) -> Option<VideoStreamConfigRejection> {
    match config.frame_contract.transfer_path {
        VideoFrameTransferPath::HardwareZeroCopy {
            handle: HardwareFrameHandle::DmaBuf { .. },
        } => None,
        VideoFrameTransferPath::SoftwareHostUpload => {
            Some(VideoStreamConfigRejection::UnsupportedFrameContract {
                frame_contract: config.frame_contract,
            })
        }
    }
}

/// Проверяет optional bit depth на точное expected значение.
fn reject_optional_bit_depth(
    bit_depth: Option<BitDepth>,
    expected: BitDepth,
) -> Option<VideoStreamConfigRejection> {
    bit_depth
        .filter(|bit_depth| *bit_depth != expected)
        .map(|bit_depth| VideoStreamConfigRejection::UnsupportedBitDepth { bit_depth })
}

/// Проверяет optional chroma на точное expected значение.
fn reject_optional_chroma(
    chroma: Option<ChromaSubsampling>,
    expected: ChromaSubsampling,
) -> Option<VideoStreamConfigRejection> {
    chroma
        .filter(|chroma| *chroma != expected)
        .map(|chroma| VideoStreamConfigRejection::UnsupportedChroma { chroma })
}

/// Проверяет optional decoded surface format на точное expected значение.
fn reject_optional_surface(
    surface_format: Option<VideoFramePixelLayout>,
    expected: VideoFramePixelLayout,
) -> Option<VideoStreamConfigRejection> {
    surface_format
        .filter(|surface_format| *surface_format != expected)
        .map(
            |surface_format| VideoStreamConfigRejection::UnsupportedSurfaceFormat {
                surface_format,
            },
        )
}
