//! Project-owned software pixel format boundary for FFmpeg negotiation.

use video_frame_contract::VideoFramePixelLayout;

use super::error::{FfiResult, FfmpegError};

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
        SoftwarePixelFormat::from_av_pixel_format(format)
            .is_some_and(|software_format| self.contains(software_format))
    }
}

#[cfg(feature = "ffmpeg")]
#[must_use]
pub(crate) fn av_pixel_format_is_software(format: ffmpeg_sys_next::AVPixelFormat) -> bool {
    // SAFETY: `av_pix_fmt_desc_get` только читает registry FFmpeg и возвращает
    // borrowed descriptor или null. Descriptor не сохраняется и используется
    // только в этом thread до конца функции.
    let descriptor = unsafe { ffmpeg_sys_next::av_pix_fmt_desc_get(format) };

    if descriptor.is_null() {
        return false;
    }

    // SAFETY: null уже обработан; descriptor принадлежит FFmpeg global registry,
    // читается immutable и не требует освобождения caller-ом.
    let descriptor_flags = unsafe { (*descriptor).flags };

    descriptor_flags & ffmpeg_sys_next::AV_PIX_FMT_FLAG_HWACCEL as u64 == 0
}

#[cfg(test)]
mod tests {
    use super::*;

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
}
