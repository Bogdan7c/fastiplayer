//! Project-owned software pixel format boundary for FFmpeg negotiation.

use thiserror::Error;
use video_frame_contract::VideoFramePixelLayout;

use super::error::{FfiResult, FfmpegError};

/// Typed reason why an FFmpeg pixel format is not accepted by software decode.
#[derive(Debug, Error, Clone, PartialEq, Eq)]
pub enum FfmpegPixelFormatRejection {
    /// FFmpeg не знает descriptor для этого numeric format code.
    #[error("unknown FFmpeg pixel format code {format_code}")]
    UnknownPixelFormat {
        /// Raw `AVPixelFormat` numeric value for diagnostics.
        format_code: i32,
    },

    /// Hardware frames are owned by native hardware backend path.
    #[error("FFmpeg pixel format {format_code} is hardware-backed")]
    HardwarePixelFormat {
        /// Raw `AVPixelFormat` numeric value for diagnostics.
        format_code: i32,
    },

    /// RGB output would imply CPU color conversion or another renderer contract.
    #[error("FFmpeg pixel format {format_code} is RGB")]
    RgbPixelFormat {
        /// Raw `AVPixelFormat` numeric value for diagnostics.
        format_code: i32,
    },

    /// Paletted output is not a direct planar YUV frame contract.
    #[error("FFmpeg pixel format {format_code} is paletted")]
    PalettedPixelFormat {
        /// Raw `AVPixelFormat` numeric value for diagnostics.
        format_code: i32,
    },

    /// Bitstream-packed pixel formats do not expose simple visible Y/U/V rows.
    #[error("FFmpeg pixel format {format_code} is bitstream-packed")]
    BitstreamPixelFormat {
        /// Raw `AVPixelFormat` numeric value for diagnostics.
        format_code: i32,
    },

    /// Floating-point formats are outside the explicit v1 YUV integer matrix.
    #[error("FFmpeg pixel format {format_code} is floating-point")]
    FloatingPointPixelFormat {
        /// Raw `AVPixelFormat` numeric value for diagnostics.
        format_code: i32,
    },

    /// Alpha formats add a component the current video contract does not own.
    #[error("FFmpeg pixel format {format_code} carries alpha")]
    AlphaPixelFormat {
        /// Raw `AVPixelFormat` numeric value for diagnostics.
        format_code: i32,
    },

    /// Big-endian integer formats do not match the explicit little-endian host layouts.
    #[error("FFmpeg pixel format {format_code} is big-endian")]
    BigEndianPixelFormat {
        /// Raw `AVPixelFormat` numeric value for diagnostics.
        format_code: i32,
    },

    /// Format is CPU-visible but not one of the explicit v1 host-planar YUV layouts.
    #[error("FFmpeg pixel format {format_code} is not in the v1 software YUV matrix")]
    UnsupportedSoftwarePixelFormat {
        /// Raw `AVPixelFormat` numeric value for diagnostics.
        format_code: i32,
    },
}

/// Software pixel format, который adapter разрешает FFmpeg decoder-у.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum SoftwarePixelFormat {
    /// FFmpeg `AV_PIX_FMT_YUV420P`.
    Yuv420Planar8,

    /// FFmpeg `AV_PIX_FMT_YUV420P10LE`.
    Yuv420Planar10Le,

    /// FFmpeg `AV_PIX_FMT_YUV420P12LE`.
    Yuv420Planar12Le,

    /// FFmpeg `AV_PIX_FMT_YUV422P`.
    Yuv422Planar8,

    /// FFmpeg `AV_PIX_FMT_YUV422P10LE`.
    Yuv422Planar10Le,

    /// FFmpeg `AV_PIX_FMT_YUV422P12LE`.
    Yuv422Planar12Le,

    /// FFmpeg `AV_PIX_FMT_YUV444P`.
    Yuv444Planar8,

    /// FFmpeg `AV_PIX_FMT_YUV444P10LE`.
    Yuv444Planar10Le,
}

impl SoftwarePixelFormat {
    /// Converts neutral frame contract layout into FFmpeg software format.
    #[must_use]
    pub const fn from_frame_pixel_layout(layout: VideoFramePixelLayout) -> Option<Self> {
        match layout {
            VideoFramePixelLayout::Yuv420Planar8 => Some(Self::Yuv420Planar8),
            VideoFramePixelLayout::Yuv420Planar10Le => Some(Self::Yuv420Planar10Le),
            VideoFramePixelLayout::Yuv420Planar12Le => Some(Self::Yuv420Planar12Le),
            VideoFramePixelLayout::Yuv422Planar8 => Some(Self::Yuv422Planar8),
            VideoFramePixelLayout::Yuv422Planar10Le => Some(Self::Yuv422Planar10Le),
            VideoFramePixelLayout::Yuv422Planar12Le => Some(Self::Yuv422Planar12Le),
            VideoFramePixelLayout::Yuv444Planar8 => Some(Self::Yuv444Planar8),
            VideoFramePixelLayout::Yuv444Planar10Le => Some(Self::Yuv444Planar10Le),
            VideoFramePixelLayout::Nv12
            | VideoFramePixelLayout::P010
            | VideoFramePixelLayout::Rgba8 => None,
        }
    }

    /// Converts this FFmpeg-side format back to the neutral frame layout.
    #[must_use]
    pub const fn frame_pixel_layout(self) -> VideoFramePixelLayout {
        match self {
            Self::Yuv420Planar8 => VideoFramePixelLayout::Yuv420Planar8,
            Self::Yuv420Planar10Le => VideoFramePixelLayout::Yuv420Planar10Le,
            Self::Yuv420Planar12Le => VideoFramePixelLayout::Yuv420Planar12Le,
            Self::Yuv422Planar8 => VideoFramePixelLayout::Yuv422Planar8,
            Self::Yuv422Planar10Le => VideoFramePixelLayout::Yuv422Planar10Le,
            Self::Yuv422Planar12Le => VideoFramePixelLayout::Yuv422Planar12Le,
            Self::Yuv444Planar8 => VideoFramePixelLayout::Yuv444Planar8,
            Self::Yuv444Planar10Le => VideoFramePixelLayout::Yuv444Planar10Le,
        }
    }

    /// Stable label для diagnostics.
    #[must_use]
    pub const fn diagnostic_label(self) -> &'static str {
        self.frame_pixel_layout().diagnostic_label()
    }

    /// Converts from raw FFmpeg enum only inside the FFI boundary.
    #[cfg(feature = "ffmpeg")]
    #[must_use]
    pub(crate) const fn from_av_pixel_format(
        format: ffmpeg_sys_next::AVPixelFormat,
    ) -> Option<Self> {
        match format {
            ffmpeg_sys_next::AVPixelFormat::AV_PIX_FMT_YUV420P => Some(Self::Yuv420Planar8),
            ffmpeg_sys_next::AVPixelFormat::AV_PIX_FMT_YUV420P10LE => Some(Self::Yuv420Planar10Le),
            ffmpeg_sys_next::AVPixelFormat::AV_PIX_FMT_YUV420P12LE => Some(Self::Yuv420Planar12Le),
            ffmpeg_sys_next::AVPixelFormat::AV_PIX_FMT_YUV422P => Some(Self::Yuv422Planar8),
            ffmpeg_sys_next::AVPixelFormat::AV_PIX_FMT_YUV422P10LE => Some(Self::Yuv422Planar10Le),
            ffmpeg_sys_next::AVPixelFormat::AV_PIX_FMT_YUV422P12LE => Some(Self::Yuv422Planar12Le),
            ffmpeg_sys_next::AVPixelFormat::AV_PIX_FMT_YUV444P => Some(Self::Yuv444Planar8),
            ffmpeg_sys_next::AVPixelFormat::AV_PIX_FMT_YUV444P10LE => Some(Self::Yuv444Planar10Le),
            _ => None,
        }
    }

    /// Converts from raw FFmpeg enum and preserves a typed rejection reason.
    #[cfg(feature = "ffmpeg")]
    pub(crate) fn try_from_av_pixel_format(
        format: ffmpeg_sys_next::AVPixelFormat,
    ) -> Result<Self, FfmpegPixelFormatRejection> {
        validate_av_pixel_format_descriptor(format)?;

        Self::from_av_pixel_format(format).ok_or(
            FfmpegPixelFormatRejection::UnsupportedSoftwarePixelFormat {
                format_code: format as i32,
            },
        )
    }

    /// Converts raw `AVFrame.format` integer without constructing invalid Rust enum values.
    #[cfg(feature = "ffmpeg")]
    #[must_use]
    pub(crate) fn from_av_format_code(format_code: i32) -> Option<Self> {
        match format_code {
            code if code == ffmpeg_sys_next::AVPixelFormat::AV_PIX_FMT_YUV420P as i32 => {
                Some(Self::Yuv420Planar8)
            }
            code if code == ffmpeg_sys_next::AVPixelFormat::AV_PIX_FMT_YUV420P10LE as i32 => {
                Some(Self::Yuv420Planar10Le)
            }
            code if code == ffmpeg_sys_next::AVPixelFormat::AV_PIX_FMT_YUV420P12LE as i32 => {
                Some(Self::Yuv420Planar12Le)
            }
            code if code == ffmpeg_sys_next::AVPixelFormat::AV_PIX_FMT_YUV422P as i32 => {
                Some(Self::Yuv422Planar8)
            }
            code if code == ffmpeg_sys_next::AVPixelFormat::AV_PIX_FMT_YUV422P10LE as i32 => {
                Some(Self::Yuv422Planar10Le)
            }
            code if code == ffmpeg_sys_next::AVPixelFormat::AV_PIX_FMT_YUV422P12LE as i32 => {
                Some(Self::Yuv422Planar12Le)
            }
            code if code == ffmpeg_sys_next::AVPixelFormat::AV_PIX_FMT_YUV444P as i32 => {
                Some(Self::Yuv444Planar8)
            }
            code if code == ffmpeg_sys_next::AVPixelFormat::AV_PIX_FMT_YUV444P10LE as i32 => {
                Some(Self::Yuv444Planar10Le)
            }
            _ => None,
        }
    }
}

/// Immutable allowlist, которым `get_format` ограничивает FFmpeg decoder.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SoftwarePixelFormatSet {
    /// Разрешённые software formats без FFmpeg raw enum в public API.
    accepted_formats: Vec<SoftwarePixelFormat>,
}

impl SoftwarePixelFormatSet {
    /// Создаёт allowlist и отсекает duplicates без изменения порядка.
    pub fn new(accepted_formats: impl IntoIterator<Item = SoftwarePixelFormat>) -> FfiResult<Self> {
        let mut unique_formats = Vec::new();

        for accepted_format in accepted_formats {
            if !unique_formats.contains(&accepted_format) {
                unique_formats.push(accepted_format);
            }
        }

        if unique_formats.is_empty() {
            return Err(FfmpegError::InvalidInput {
                operation: "software pixel format allowlist",
                details: "allowlist must contain at least one software pixel format".to_owned(),
            });
        }

        Ok(Self {
            accepted_formats: unique_formats,
        })
    }

    /// V1 host-planar matrix, supported by the current neutral contracts.
    #[must_use]
    pub fn v1_host_planar() -> Self {
        Self {
            accepted_formats: vec![
                SoftwarePixelFormat::Yuv420Planar8,
                SoftwarePixelFormat::Yuv420Planar10Le,
                SoftwarePixelFormat::Yuv420Planar12Le,
                SoftwarePixelFormat::Yuv422Planar8,
                SoftwarePixelFormat::Yuv422Planar10Le,
                SoftwarePixelFormat::Yuv422Planar12Le,
                SoftwarePixelFormat::Yuv444Planar8,
                SoftwarePixelFormat::Yuv444Planar10Le,
            ],
        }
    }

    /// Builds allowlist from neutral renderer-intersected layouts.
    pub fn from_frame_pixel_layouts(
        layouts: impl IntoIterator<Item = VideoFramePixelLayout>,
    ) -> FfiResult<Self> {
        Self::new(
            layouts
                .into_iter()
                .filter_map(SoftwarePixelFormat::from_frame_pixel_layout),
        )
    }

    /// Checks whether a project-owned software pixel format is accepted.
    #[must_use]
    pub fn contains(&self, format: SoftwarePixelFormat) -> bool {
        self.accepted_formats.contains(&format)
    }

    /// Exposes accepted project-owned formats without raw FFmpeg values.
    pub fn iter(&self) -> impl Iterator<Item = SoftwarePixelFormat> + '_ {
        self.accepted_formats.iter().copied()
    }

    /// Checks raw FFmpeg candidate only inside the FFI boundary.
    #[cfg(feature = "ffmpeg")]
    #[must_use]
    pub(crate) fn contains_av_pixel_format(&self, format: ffmpeg_sys_next::AVPixelFormat) -> bool {
        SoftwarePixelFormat::try_from_av_pixel_format(format)
            .ok()
            .is_some_and(|software_format| self.contains(software_format))
    }
}

#[cfg(feature = "ffmpeg")]
#[must_use]
pub(crate) fn av_pixel_format_is_software(format: ffmpeg_sys_next::AVPixelFormat) -> bool {
    validate_av_pixel_format_descriptor(format).is_ok()
}

#[cfg(feature = "ffmpeg")]
fn validate_av_pixel_format_descriptor(
    format: ffmpeg_sys_next::AVPixelFormat,
) -> Result<(), FfmpegPixelFormatRejection> {
    // SAFETY: `av_pix_fmt_desc_get` только читает registry FFmpeg и возвращает
    // borrowed descriptor или null. Descriptor не сохраняется и используется
    // только в этом thread до конца функции.
    let descriptor = unsafe { ffmpeg_sys_next::av_pix_fmt_desc_get(format) };

    if descriptor.is_null() {
        return Err(FfmpegPixelFormatRejection::UnknownPixelFormat {
            format_code: format as i32,
        });
    }

    // SAFETY: null уже обработан; descriptor принадлежит FFmpeg global registry,
    // читается immutable и не требует освобождения caller-ом.
    let descriptor_flags = unsafe { (*descriptor).flags };

    let format_code = format as i32;

    reject_flag(
        descriptor_flags,
        ffmpeg_sys_next::AV_PIX_FMT_FLAG_HWACCEL,
        FfmpegPixelFormatRejection::HardwarePixelFormat { format_code },
    )?;
    reject_flag(
        descriptor_flags,
        ffmpeg_sys_next::AV_PIX_FMT_FLAG_RGB,
        FfmpegPixelFormatRejection::RgbPixelFormat { format_code },
    )?;
    reject_flag(
        descriptor_flags,
        ffmpeg_sys_next::AV_PIX_FMT_FLAG_PAL,
        FfmpegPixelFormatRejection::PalettedPixelFormat { format_code },
    )?;
    reject_flag(
        descriptor_flags,
        ffmpeg_sys_next::AV_PIX_FMT_FLAG_BITSTREAM,
        FfmpegPixelFormatRejection::BitstreamPixelFormat { format_code },
    )?;
    reject_flag(
        descriptor_flags,
        ffmpeg_sys_next::AV_PIX_FMT_FLAG_FLOAT,
        FfmpegPixelFormatRejection::FloatingPointPixelFormat { format_code },
    )?;
    reject_flag(
        descriptor_flags,
        ffmpeg_sys_next::AV_PIX_FMT_FLAG_ALPHA,
        FfmpegPixelFormatRejection::AlphaPixelFormat { format_code },
    )?;
    reject_flag(
        descriptor_flags,
        ffmpeg_sys_next::AV_PIX_FMT_FLAG_BE,
        FfmpegPixelFormatRejection::BigEndianPixelFormat { format_code },
    )?;

    Ok(())
}

#[cfg(feature = "ffmpeg")]
fn reject_flag(
    descriptor_flags: u64,
    forbidden_flag: i32,
    rejection: FfmpegPixelFormatRejection,
) -> Result<(), FfmpegPixelFormatRejection> {
    if descriptor_flags & forbidden_flag as u64 == 0 {
        Ok(())
    } else {
        Err(rejection)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use video_frame_contract::VideoFramePixelLayout;

    #[test]
    fn allowlist_deduplicates_without_losing_order() {
        let formats = SoftwarePixelFormatSet::new([
            SoftwarePixelFormat::Yuv420Planar8,
            SoftwarePixelFormat::Yuv420Planar8,
            SoftwarePixelFormat::Yuv422Planar8,
        ])
        .expect("non-empty allowlist should be valid");

        let collected_formats = formats.iter().collect::<Vec<_>>();

        assert_eq!(
            collected_formats,
            vec![
                SoftwarePixelFormat::Yuv420Planar8,
                SoftwarePixelFormat::Yuv422Planar8
            ]
        );
    }

    #[test]
    fn allowlist_rejects_empty_adapter_policy() {
        let error = SoftwarePixelFormatSet::new([]).expect_err("empty allowlist must fail");

        assert!(matches!(error, FfmpegError::InvalidInput { .. }));
    }

    #[test]
    fn neutral_layout_mapping_accepts_only_explicit_host_planar_yuv() {
        let mapped =
            SoftwarePixelFormat::from_frame_pixel_layout(VideoFramePixelLayout::Yuv422Planar12Le);
        let rejected = SoftwarePixelFormat::from_frame_pixel_layout(VideoFramePixelLayout::Nv12);

        assert_eq!(mapped, Some(SoftwarePixelFormat::Yuv422Planar12Le));
        assert_eq!(rejected, None);
    }

    #[cfg(feature = "ffmpeg")]
    #[test]
    fn ffmpeg_pix_fmt_maps_supported_formats_to_expected_layouts() {
        let cases = [
            (
                ffmpeg_sys_next::AVPixelFormat::AV_PIX_FMT_YUV420P,
                VideoFramePixelLayout::Yuv420Planar8,
            ),
            (
                ffmpeg_sys_next::AVPixelFormat::AV_PIX_FMT_YUV420P10LE,
                VideoFramePixelLayout::Yuv420Planar10Le,
            ),
            (
                ffmpeg_sys_next::AVPixelFormat::AV_PIX_FMT_YUV420P12LE,
                VideoFramePixelLayout::Yuv420Planar12Le,
            ),
            (
                ffmpeg_sys_next::AVPixelFormat::AV_PIX_FMT_YUV422P,
                VideoFramePixelLayout::Yuv422Planar8,
            ),
            (
                ffmpeg_sys_next::AVPixelFormat::AV_PIX_FMT_YUV422P10LE,
                VideoFramePixelLayout::Yuv422Planar10Le,
            ),
            (
                ffmpeg_sys_next::AVPixelFormat::AV_PIX_FMT_YUV422P12LE,
                VideoFramePixelLayout::Yuv422Planar12Le,
            ),
            (
                ffmpeg_sys_next::AVPixelFormat::AV_PIX_FMT_YUV444P,
                VideoFramePixelLayout::Yuv444Planar8,
            ),
            (
                ffmpeg_sys_next::AVPixelFormat::AV_PIX_FMT_YUV444P10LE,
                VideoFramePixelLayout::Yuv444Planar10Le,
            ),
        ];

        for (ffmpeg_format, expected_layout) in cases {
            let software_format =
                SoftwarePixelFormat::try_from_av_pixel_format(ffmpeg_format).unwrap();

            assert_eq!(software_format.frame_pixel_layout(), expected_layout);
        }
    }

    #[cfg(feature = "ffmpeg")]
    #[test]
    fn ffmpeg_pix_fmt_rejects_hardware_rgb_and_unsupported_yuv_formats() {
        let hardware = SoftwarePixelFormat::try_from_av_pixel_format(
            ffmpeg_sys_next::AVPixelFormat::AV_PIX_FMT_VAAPI,
        )
        .expect_err("hardware frames must stay outside FFmpeg software adapter");
        let rgb = SoftwarePixelFormat::try_from_av_pixel_format(
            ffmpeg_sys_next::AVPixelFormat::AV_PIX_FMT_RGB24,
        )
        .expect_err("RGB output would require a conversion path");
        let unsupported_yuv = SoftwarePixelFormat::try_from_av_pixel_format(
            ffmpeg_sys_next::AVPixelFormat::AV_PIX_FMT_NV12,
        )
        .expect_err("semi-planar NV12 is not an explicit host-planar v1 layout");

        assert!(matches!(
            hardware,
            FfmpegPixelFormatRejection::HardwarePixelFormat { .. }
        ));
        assert!(matches!(
            rgb,
            FfmpegPixelFormatRejection::RgbPixelFormat { .. }
        ));
        assert!(matches!(
            unsupported_yuv,
            FfmpegPixelFormatRejection::UnsupportedSoftwarePixelFormat { .. }
        ));
    }

    #[cfg(feature = "ffmpeg")]
    #[test]
    fn ffmpeg_pix_fmt_rejects_big_endian_alpha_float_and_bitstream_formats() {
        let big_endian = SoftwarePixelFormat::try_from_av_pixel_format(
            ffmpeg_sys_next::AVPixelFormat::AV_PIX_FMT_YUV420P10BE,
        )
        .expect_err("v1 host layouts are little-endian");
        let alpha = SoftwarePixelFormat::try_from_av_pixel_format(
            ffmpeg_sys_next::AVPixelFormat::AV_PIX_FMT_YUVA420P,
        )
        .expect_err("alpha is not part of the current YUV frame contract");
        let floating_point = SoftwarePixelFormat::try_from_av_pixel_format(
            ffmpeg_sys_next::AVPixelFormat::AV_PIX_FMT_GBRPF32LE,
        )
        .expect_err("float formats are not in the integer YUV matrix");
        let bitstream = SoftwarePixelFormat::try_from_av_pixel_format(
            ffmpeg_sys_next::AVPixelFormat::AV_PIX_FMT_MONOBLACK,
        )
        .expect_err("bitstream-packed formats do not expose Y/U/V rows");

        assert!(matches!(
            big_endian,
            FfmpegPixelFormatRejection::BigEndianPixelFormat { .. }
        ));
        assert!(matches!(
            alpha,
            FfmpegPixelFormatRejection::AlphaPixelFormat { .. }
        ));
        assert!(matches!(
            floating_point,
            FfmpegPixelFormatRejection::RgbPixelFormat { .. }
                | FfmpegPixelFormatRejection::FloatingPointPixelFormat { .. }
        ));
        assert!(matches!(
            bitstream,
            FfmpegPixelFormatRejection::BitstreamPixelFormat { .. }
        ));
    }
}
