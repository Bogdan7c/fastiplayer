//! Safe RAII owner for refcounted `AVFrame` data.

#[cfg(feature = "ffmpeg")]
use std::os::raw::c_int;
#[cfg(feature = "ffmpeg")]
use std::ptr::NonNull;
#[cfg(feature = "ffmpeg")]
use std::slice;

use super::error::{FfiResult, FfmpegError};
use super::pixel_format::SoftwarePixelFormat;

/// Количество `AVFrame::data` / `AVFrame::linesize` slots.
pub const AV_FRAME_DATA_POINTERS: usize = 8;

/// Opaque owner для refcounted `AVFrame`.
#[derive(Debug)]
pub struct OwnedAvFrame {
    /// Raw frame живёт только внутри FFI boundary.
    #[cfg(feature = "ffmpeg")]
    raw_frame: NonNull<ffmpeg_sys_next::AVFrame>,

    /// Marker, чтобы type существовал в default build-е без FFmpeg headers/libs.
    #[cfg(not(feature = "ffmpeg"))]
    _feature_disabled: (),
}

#[cfg(feature = "ffmpeg")]
// SAFETY: `OwnedAvFrame` является exclusive RAII owner-ом. Safe access, который
// может mutate/unref frame, требует `&mut self`, а shared row borrows требуют
// `&self` и не раскрывают raw pointer-ы. Перенос owner-а в decoder thread
// безопасен; concurrent shared mutation остаётся невозможной, потому что тип
// не реализует `Sync`.
unsafe impl Send for OwnedAvFrame {}

/// Backward-compatible alias для старого scaffold имени.
pub type FfmpegFrame = OwnedAvFrame;

/// Timestamp fields, copied out of `AVFrame`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct FrameTimestamps {
    /// `AVFrame.best_effort_timestamp`.
    pub best_effort_timestamp: i64,

    /// `AVFrame.pts`.
    pub pts: i64,

    /// `AVFrame.pkt_dts`.
    pub packet_dts: i64,

    /// `AVFrame.duration`.
    pub duration: i64,
}

/// FFmpeg `AVRational`, copied out as plain integers.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct FrameRational {
    /// Rational numerator.
    pub numerator: i32,

    /// Rational denominator.
    pub denominator: i32,
}

impl FrameRational {
    /// Creates a copied rational value without exposing FFmpeg types.
    #[must_use]
    pub const fn new(numerator: i32, denominator: i32) -> Self {
        Self {
            numerator,
            denominator,
        }
    }
}

/// FFmpeg mastering-display side data copied out of `AVFrame`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct FrameMasteringDisplayMetadata {
    /// Whether FFmpeg reported display primaries and white point.
    pub has_primaries: bool,

    /// Whether FFmpeg reported min/max mastering luminance.
    pub has_luminance: bool,

    /// CIE 1931 xy display primaries in FFmpeg r/g/b order.
    pub display_primaries: [[FrameRational; 2]; 3],

    /// CIE 1931 xy white point.
    pub white_point: [FrameRational; 2],

    /// Minimum mastering display luminance in cd/m^2.
    pub min_luminance: FrameRational,

    /// Maximum mastering display luminance in cd/m^2.
    pub max_luminance: FrameRational,
}

/// FFmpeg content-light side data copied out of `AVFrame`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct FrameContentLightMetadata {
    /// MaxCLL in nits.
    pub max_content_light_level: u32,

    /// MaxFALL in nits.
    pub max_frame_average_light_level: u32,
}

#[cfg(feature = "ffmpeg")]
#[repr(C)]
struct AvMasteringDisplayMetadataMirror {
    display_primaries: [[ffmpeg_sys_next::AVRational; 2]; 3],
    white_point: [ffmpeg_sys_next::AVRational; 2],
    min_luminance: ffmpeg_sys_next::AVRational,
    max_luminance: ffmpeg_sys_next::AVRational,
    has_primaries: c_int,
    has_luminance: c_int,
}

#[cfg(feature = "ffmpeg")]
#[repr(C)]
struct AvContentLightMetadataMirror {
    max_cll: u32,
    max_fall: u32,
}

/// Color metadata fields, copied out without exposing FFmpeg enum types.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct FrameColorMetadata {
    /// FFmpeg `AVColorRange` numeric value.
    pub color_range: i32,

    /// FFmpeg `AVColorPrimaries` numeric value.
    pub color_primaries: i32,

    /// FFmpeg `AVColorTransferCharacteristic` numeric value.
    pub color_transfer: i32,

    /// FFmpeg `AVColorSpace` numeric value.
    pub color_space: i32,

    /// FFmpeg `AVChromaLocation` numeric value.
    pub chroma_location: i32,

    /// FFmpeg `AV_FRAME_DATA_MASTERING_DISPLAY_METADATA`, if present.
    pub mastering_display_metadata: Option<FrameMasteringDisplayMetadata>,

    /// FFmpeg `AV_FRAME_DATA_CONTENT_LIGHT_LEVEL`, if present.
    pub content_light_metadata: Option<FrameContentLightMetadata>,
}

impl OwnedAvFrame {
    /// Allocates an empty `AVFrame` owner for decode receive/ref operations.
    pub fn new() -> FfiResult<Self> {
        #[cfg(not(feature = "ffmpeg"))]
        {
            Err(FfmpegError::FeatureDisabled)
        }

        #[cfg(feature = "ffmpeg")]
        {
            // SAFETY: FFmpeg allocator возвращает owned `AVFrame*` или null.
            // Pointer сразу заворачивается в `NonNull` и освобождается через
            // `av_frame_free` в Drop.
            let raw_frame = unsafe { ffmpeg_sys_next::av_frame_alloc() };
            let raw_frame = NonNull::new(raw_frame).ok_or(FfmpegError::AllocationFailed {
                operation: "av_frame_alloc",
            })?;

            Ok(Self { raw_frame })
        }
    }

    /// Compatibility name для decode receive buffer allocation.
    pub fn allocate_for_decode() -> FfiResult<Self> {
        Self::new()
    }

    /// Allocates a new frame and references the same refcounted data as `source`.
    pub fn ref_from(source: &Self) -> FfiResult<Self> {
        let referenced_frame = Self::new()?;

        #[cfg(not(feature = "ffmpeg"))]
        {
            let _source = source;
            let _referenced_frame = referenced_frame;
            Err(FfmpegError::FeatureDisabled)
        }

        #[cfg(feature = "ffmpeg")]
        {
            // SAFETY: оба frame pointer-а валидны и принадлежат RAII wrappers.
            // `av_frame_ref` создаёт новый reference к buffers source-а; source
            // не mutates и может быть unref/drop после успешного вызова.
            let status = unsafe {
                ffmpeg_sys_next::av_frame_ref(
                    referenced_frame.raw_frame.as_ptr(),
                    source.raw_frame.as_ptr(),
                )
            };

            if status < 0 {
                return Err(FfmpegError::from_averror("av_frame_ref", status));
            }

            Ok(referenced_frame)
        }
    }

    /// Convenience wrapper around `av_frame_ref`.
    pub fn try_clone_ref(&self) -> FfiResult<Self> {
        Self::ref_from(self)
    }

    /// Releases referenced buffers while keeping the `AVFrame` allocation reusable.
    pub fn unref(&mut self) {
        #[cfg(feature = "ffmpeg")]
        {
            // SAFETY: wrapper единолично владеет frame pointer-ом. `&mut self`
            // гарантирует, что active borrowed plane slices через safe API уже
            // закончились до release.
            unsafe { ffmpeg_sys_next::av_frame_unref(self.raw_frame.as_ptr()) };
        }
    }

    /// Visible width from `AVFrame`.
    #[must_use]
    pub fn width(&self) -> i32 {
        #[cfg(not(feature = "ffmpeg"))]
        {
            0
        }

        #[cfg(feature = "ffmpeg")]
        {
            // SAFETY: raw pointer валиден в течение `&self`; читаем plain field.
            unsafe { self.raw_frame.as_ref().width }
        }
    }

    /// Visible height from `AVFrame`.
    #[must_use]
    pub fn height(&self) -> i32 {
        #[cfg(not(feature = "ffmpeg"))]
        {
            0
        }

        #[cfg(feature = "ffmpeg")]
        {
            // SAFETY: raw pointer валиден в течение `&self`; читаем plain field.
            unsafe { self.raw_frame.as_ref().height }
        }
    }

    /// Raw numeric pixel format from `AVFrame.format`.
    #[must_use]
    pub fn raw_format_code(&self) -> i32 {
        #[cfg(not(feature = "ffmpeg"))]
        {
            0
        }

        #[cfg(feature = "ffmpeg")]
        {
            // SAFETY: raw pointer валиден в течение `&self`; читаем plain field.
            unsafe { self.raw_frame.as_ref().format }
        }
    }

    /// Known software pixel format if it belongs to the v1 adapter matrix.
    #[must_use]
    pub fn software_format(&self) -> Option<SoftwarePixelFormat> {
        #[cfg(not(feature = "ffmpeg"))]
        {
            None
        }

        #[cfg(feature = "ffmpeg")]
        {
            SoftwarePixelFormat::from_av_format_code(self.raw_format_code())
        }
    }

    /// Plane line size in bytes, if the plane slot exists.
    #[must_use]
    pub fn linesize(&self, plane_index: usize) -> Option<i32> {
        if plane_index >= AV_FRAME_DATA_POINTERS {
            return None;
        }

        #[cfg(not(feature = "ffmpeg"))]
        {
            Some(0)
        }

        #[cfg(feature = "ffmpeg")]
        {
            // SAFETY: index bounds проверены выше; raw pointer валиден в течение `&self`.
            Some(unsafe { self.raw_frame.as_ref().linesize[plane_index] })
        }
    }

    /// Возвращает address plane data без raw pointer type в public API.
    #[must_use]
    pub fn plane_data_address(&self, plane_index: usize) -> Option<usize> {
        if plane_index >= AV_FRAME_DATA_POINTERS {
            return None;
        }

        #[cfg(not(feature = "ffmpeg"))]
        {
            None
        }

        #[cfg(feature = "ffmpeg")]
        {
            // SAFETY: index bounds проверены выше; raw pointer валиден в течение `&self`.
            let data_pointer = unsafe { self.raw_frame.as_ref().data[plane_index] };

            NonNull::new(data_pointer).map(|pointer| pointer.as_ptr() as usize)
        }
    }

    /// Borrows one visible row from a plane after caller computed row width.
    pub fn plane_row_data(
        &self,
        plane_index: usize,
        row_index: usize,
        visible_row_bytes: usize,
    ) -> FfiResult<Option<&[u8]>> {
        if plane_index >= AV_FRAME_DATA_POINTERS {
            return Err(FfmpegError::InvalidInput {
                operation: "AVFrame plane data access",
                details: format!(
                    "plane index {plane_index} is outside 0..{AV_FRAME_DATA_POINTERS}"
                ),
            });
        }

        #[cfg(not(feature = "ffmpeg"))]
        {
            let _row_index = row_index;
            let _visible_row_bytes = visible_row_bytes;
            Ok(None)
        }

        #[cfg(feature = "ffmpeg")]
        {
            // SAFETY: index bounds проверены выше; raw pointer валиден в течение `&self`.
            let data_pointer = unsafe { self.raw_frame.as_ref().data[plane_index] };

            if data_pointer.is_null() {
                return Ok(None);
            }

            let line_size = self.linesize(plane_index).unwrap_or_default();
            if line_size < 0 {
                return Err(FfmpegError::InvalidInput {
                    operation: "AVFrame plane row access",
                    details: "negative FFmpeg linesize is not supported by this safe accessor"
                        .to_owned(),
                });
            }

            let line_size = line_size as usize;
            if visible_row_bytes > line_size {
                return Err(FfmpegError::InvalidInput {
                    operation: "AVFrame plane row access",
                    details: format!(
                        "visible row has {visible_row_bytes} bytes but linesize is {line_size}"
                    ),
                });
            }

            let row_offset =
                row_index
                    .checked_mul(line_size)
                    .ok_or_else(|| FfmpegError::InvalidInput {
                        operation: "AVFrame plane row access",
                        details: format!("row index {row_index} overflows linesize {line_size}"),
                    })?;

            if visible_row_bytes == 0 {
                return Ok(Some(&[]));
            }

            // SAFETY: `OwnedAvFrame` владеет refcounted `AVFrame` lifetime-ом.
            // Caller передаёт row index и visible row width, рассчитанные из
            // layout/dimensions; accessor проверяет, что visible row не шире
            // stride. FFmpeg decoder/get_buffer contract гарантирует, что строка
            // доступна пока frame не unref/drop. `&self` не позволяет safe-кодом
            // вызвать `unref` во время borrow.
            Ok(Some(unsafe {
                slice::from_raw_parts(data_pointer.add(row_offset).cast_const(), visible_row_bytes)
            }))
        }
    }

    /// Timestamps copied from the frame.
    #[must_use]
    pub fn timestamps(&self) -> FrameTimestamps {
        #[cfg(not(feature = "ffmpeg"))]
        {
            FrameTimestamps {
                best_effort_timestamp: 0,
                pts: 0,
                packet_dts: 0,
                duration: 0,
            }
        }

        #[cfg(feature = "ffmpeg")]
        {
            // SAFETY: raw pointer валиден в течение `&self`; читаем plain fields.
            let frame = unsafe { self.raw_frame.as_ref() };

            FrameTimestamps {
                best_effort_timestamp: frame.best_effort_timestamp,
                pts: frame.pts,
                packet_dts: frame.pkt_dts,
                duration: frame.duration,
            }
        }
    }

    /// Color metadata copied from the frame.
    #[must_use]
    pub fn color_metadata(&self) -> FrameColorMetadata {
        #[cfg(not(feature = "ffmpeg"))]
        {
            FrameColorMetadata {
                color_range: 0,
                color_primaries: 0,
                color_transfer: 0,
                color_space: 0,
                chroma_location: 0,
                mastering_display_metadata: None,
                content_light_metadata: None,
            }
        }

        #[cfg(feature = "ffmpeg")]
        {
            // SAFETY: raw pointer валиден в течение `&self`; читаем plain fields.
            let frame = unsafe { self.raw_frame.as_ref() };

            FrameColorMetadata {
                color_range: frame.color_range as i32,
                color_primaries: frame.color_primaries as i32,
                color_transfer: frame.color_trc as i32,
                color_space: frame.colorspace as i32,
                chroma_location: frame.chroma_location as i32,
                mastering_display_metadata: frame_mastering_display_metadata(
                    self.raw_frame.as_ptr(),
                ),
                content_light_metadata: frame_content_light_metadata(self.raw_frame.as_ptr()),
            }
        }
    }

    #[cfg(feature = "ffmpeg")]
    pub(crate) fn as_mut_ptr(&mut self) -> *mut ffmpeg_sys_next::AVFrame {
        self.raw_frame.as_ptr()
    }

    /// Allocates a small real video frame for AVFrame-backed resource tests.
    #[cfg(all(test, feature = "ffmpeg"))]
    pub(crate) fn new_test_video_frame(
        format: SoftwarePixelFormat,
        width: i32,
        height: i32,
        alignment: i32,
    ) -> FfiResult<Self> {
        Self::new_test_video_frame_with_av_pixel_format(
            test_av_pixel_format(format),
            width,
            height,
            alignment,
        )
    }

    /// Allocates an NV12 frame that resource-table tests must reject.
    #[cfg(all(test, feature = "ffmpeg"))]
    pub(crate) fn new_test_unsupported_nv12_frame(
        width: i32,
        height: i32,
        alignment: i32,
    ) -> FfiResult<Self> {
        Self::new_test_video_frame_with_av_pixel_format(
            ffmpeg_sys_next::AVPixelFormat::AV_PIX_FMT_NV12,
            width,
            height,
            alignment,
        )
    }

    #[cfg(all(test, feature = "ffmpeg"))]
    fn new_test_video_frame_with_av_pixel_format(
        format: ffmpeg_sys_next::AVPixelFormat,
        width: i32,
        height: i32,
        alignment: i32,
    ) -> FfiResult<Self> {
        let frame = Self::new()?;

        // SAFETY: test owns a fresh AVFrame and sets required video fields
        // before `av_frame_get_buffer`, matching FFmpeg allocation rules.
        unsafe {
            let raw_frame = frame.raw_frame.as_ptr();
            (*raw_frame).format = format as i32;
            (*raw_frame).width = width;
            (*raw_frame).height = height;

            let status = ffmpeg_sys_next::av_frame_get_buffer(raw_frame, alignment);
            if status < 0 {
                return Err(FfmpegError::from_averror("av_frame_get_buffer", status));
            }
        }

        Ok(frame)
    }

    /// Writes visible test bytes into one AVFrame row without exposing raw pointers.
    #[cfg(all(test, feature = "ffmpeg"))]
    pub(crate) fn write_test_plane_row(
        &mut self,
        plane_index: usize,
        row_index: usize,
        row_bytes: &[u8],
    ) -> FfiResult<()> {
        if plane_index >= AV_FRAME_DATA_POINTERS {
            return Err(FfmpegError::InvalidInput {
                operation: "AVFrame test row write",
                details: format!(
                    "plane index {plane_index} is outside 0..{AV_FRAME_DATA_POINTERS}"
                ),
            });
        }

        let line_size = self
            .linesize(plane_index)
            .ok_or_else(|| FfmpegError::InvalidInput {
                operation: "AVFrame test row write",
                details: format!("plane index {plane_index} has no linesize slot"),
            })?;
        let line_size = usize::try_from(line_size).map_err(|_| FfmpegError::InvalidInput {
            operation: "AVFrame test row write",
            details: format!("plane index {plane_index} has negative linesize {line_size}"),
        })?;
        if row_bytes.len() > line_size {
            return Err(FfmpegError::InvalidInput {
                operation: "AVFrame test row write",
                details: format!(
                    "row has {} bytes but plane {plane_index} linesize is {line_size}",
                    row_bytes.len()
                ),
            });
        }

        // SAFETY: index bounds were checked; destination row is inside the
        // FFmpeg-allocated buffer by construction of row_index/linesize in tests.
        unsafe {
            let data_pointer = self.raw_frame.as_ref().data[plane_index];
            if data_pointer.is_null() {
                return Err(FfmpegError::InvalidInput {
                    operation: "AVFrame test row write",
                    details: format!("plane {plane_index} has null data pointer"),
                });
            }
            let row_offset = row_index.checked_mul(line_size).ok_or_else(|| {
                FfmpegError::InvalidInput {
                    operation: "AVFrame test row write",
                    details: format!(
                        "row index {row_index} overflows plane {plane_index} linesize {line_size}"
                    ),
                }
            })?;
            std::ptr::copy_nonoverlapping(
                row_bytes.as_ptr(),
                data_pointer.add(row_offset),
                row_bytes.len(),
            );
        }

        Ok(())
    }

    /// Corrupts one test plane data pointer for descriptor rejection tests.
    #[cfg(all(test, feature = "ffmpeg"))]
    pub(crate) fn clear_test_plane_data(&mut self, plane_index: usize) {
        if plane_index < AV_FRAME_DATA_POINTERS {
            // SAFETY: test-only mutation of plain AVFrame metadata field.
            unsafe { self.raw_frame.as_mut().data[plane_index] = std::ptr::null_mut() };
        }
    }

    /// Corrupts one test plane linesize for descriptor rejection tests.
    #[cfg(all(test, feature = "ffmpeg"))]
    pub(crate) fn set_test_linesize(&mut self, plane_index: usize, line_size: i32) {
        if plane_index < AV_FRAME_DATA_POINTERS {
            // SAFETY: test-only mutation of plain AVFrame metadata field.
            unsafe { self.raw_frame.as_mut().linesize[plane_index] = line_size };
        }
    }
}

#[cfg(feature = "ffmpeg")]
fn frame_mastering_display_metadata(
    raw_frame: *const ffmpeg_sys_next::AVFrame,
) -> Option<FrameMasteringDisplayMetadata> {
    // SAFETY: `raw_frame` is borrowed from a live `OwnedAvFrame`. FFmpeg returns
    // a side-data pointer owned by that frame; this function copies plain values
    // before returning and never stores FFmpeg pointers.
    let side_data = unsafe {
        ffmpeg_sys_next::av_frame_get_side_data(
            raw_frame,
            ffmpeg_sys_next::AVFrameSideDataType::AV_FRAME_DATA_MASTERING_DISPLAY_METADATA,
        )
    };
    let side_data = NonNull::new(side_data)?;

    // SAFETY: Context7/FFmpeg docs define this side-data payload layout as
    // `AVMasteringDisplayMetadata` for the requested enum value. The Rust
    // binding does not export that typedef, so we copy through a local
    // repr(C) mirror that stays private to this FFI boundary.
    let metadata =
        unsafe { (side_data.as_ref().data as *const AvMasteringDisplayMetadataMirror).as_ref()? };

    Some(FrameMasteringDisplayMetadata {
        has_primaries: metadata.has_primaries != 0,
        has_luminance: metadata.has_luminance != 0,
        display_primaries: [
            [
                frame_rational_from_av(metadata.display_primaries[0][0]),
                frame_rational_from_av(metadata.display_primaries[0][1]),
            ],
            [
                frame_rational_from_av(metadata.display_primaries[1][0]),
                frame_rational_from_av(metadata.display_primaries[1][1]),
            ],
            [
                frame_rational_from_av(metadata.display_primaries[2][0]),
                frame_rational_from_av(metadata.display_primaries[2][1]),
            ],
        ],
        white_point: [
            frame_rational_from_av(metadata.white_point[0]),
            frame_rational_from_av(metadata.white_point[1]),
        ],
        min_luminance: frame_rational_from_av(metadata.min_luminance),
        max_luminance: frame_rational_from_av(metadata.max_luminance),
    })
}

#[cfg(feature = "ffmpeg")]
fn frame_content_light_metadata(
    raw_frame: *const ffmpeg_sys_next::AVFrame,
) -> Option<FrameContentLightMetadata> {
    // SAFETY: `raw_frame` is borrowed from a live `OwnedAvFrame`; side data is
    // copied immediately into neutral integers.
    let side_data = unsafe {
        ffmpeg_sys_next::av_frame_get_side_data(
            raw_frame,
            ffmpeg_sys_next::AVFrameSideDataType::AV_FRAME_DATA_CONTENT_LIGHT_LEVEL,
        )
    };
    let side_data = NonNull::new(side_data)?;

    // SAFETY: FFmpeg defines this payload layout as `AVContentLightMetadata`
    // for `AV_FRAME_DATA_CONTENT_LIGHT_LEVEL`. The local repr(C) mirror stays
    // inside `video-ffmpeg`.
    let metadata =
        unsafe { (side_data.as_ref().data as *const AvContentLightMetadataMirror).as_ref()? };

    Some(FrameContentLightMetadata {
        max_content_light_level: metadata.max_cll,
        max_frame_average_light_level: metadata.max_fall,
    })
}

#[cfg(feature = "ffmpeg")]
fn frame_rational_from_av(value: ffmpeg_sys_next::AVRational) -> FrameRational {
    FrameRational::new(value.num, value.den)
}

#[cfg(all(test, feature = "ffmpeg"))]
const fn test_av_pixel_format(format: SoftwarePixelFormat) -> ffmpeg_sys_next::AVPixelFormat {
    match format {
        SoftwarePixelFormat::Yuv420Planar8 => ffmpeg_sys_next::AVPixelFormat::AV_PIX_FMT_YUV420P,
        SoftwarePixelFormat::Yuv420Planar10Le => {
            ffmpeg_sys_next::AVPixelFormat::AV_PIX_FMT_YUV420P10LE
        }
        SoftwarePixelFormat::Yuv420Planar12Le => {
            ffmpeg_sys_next::AVPixelFormat::AV_PIX_FMT_YUV420P12LE
        }
        SoftwarePixelFormat::Yuv422Planar8 => ffmpeg_sys_next::AVPixelFormat::AV_PIX_FMT_YUV422P,
        SoftwarePixelFormat::Yuv422Planar10Le => {
            ffmpeg_sys_next::AVPixelFormat::AV_PIX_FMT_YUV422P10LE
        }
        SoftwarePixelFormat::Yuv422Planar12Le => {
            ffmpeg_sys_next::AVPixelFormat::AV_PIX_FMT_YUV422P12LE
        }
        SoftwarePixelFormat::Yuv444Planar8 => ffmpeg_sys_next::AVPixelFormat::AV_PIX_FMT_YUV444P,
        SoftwarePixelFormat::Yuv444Planar10Le => {
            ffmpeg_sys_next::AVPixelFormat::AV_PIX_FMT_YUV444P10LE
        }
    }
}

#[cfg(feature = "ffmpeg")]
impl Drop for OwnedAvFrame {
    fn drop(&mut self) {
        let mut frame_to_free = self.raw_frame.as_ptr();

        // SAFETY: pointer получен из `av_frame_alloc` и ещё не освобождён.
        // FFmpeg unrefs buffers, frees frame struct and writes null into local
        // variable; наружу этот local pointer не отдаётся.
        unsafe { ffmpeg_sys_next::av_frame_free(&mut frame_to_free) };
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn frame_allocation_reports_feature_disabled_without_ffmpeg() {
        if cfg!(feature = "ffmpeg") {
            return;
        }

        let error = OwnedAvFrame::new().expect_err("default build has no FFmpeg FFI");

        assert_eq!(error, FfmpegError::FeatureDisabled);
    }

    #[cfg(feature = "ffmpeg")]
    #[test]
    fn frame_ref_keeps_refcounted_data_alive_after_source_unref() {
        let mut source_frame = OwnedAvFrame::new().expect("frame allocation should succeed");

        // SAFETY: test owns the fresh frame. Fields are set before
        // `av_frame_get_buffer`, as FFmpeg requires for video buffer allocation.
        unsafe {
            let raw_frame = source_frame.raw_frame.as_ptr();
            (*raw_frame).format = ffmpeg_sys_next::AVPixelFormat::AV_PIX_FMT_YUV420P as i32;
            (*raw_frame).width = 2;
            (*raw_frame).height = 2;

            let status = ffmpeg_sys_next::av_frame_get_buffer(raw_frame, 32);
            assert_eq!(status, 0, "av_frame_get_buffer should allocate data");

            let y_plane = (*raw_frame).data[0];
            let y_stride = (*raw_frame).linesize[0] as usize;
            *y_plane.add(0) = 10;
            *y_plane.add(1) = 11;
            *y_plane.add(y_stride) = 20;
            *y_plane.add(y_stride + 1) = 21;
        }

        let referenced_frame = source_frame
            .try_clone_ref()
            .expect("av_frame_ref should reference allocated frame data");
        let referenced_y_plane_address = referenced_frame.plane_data_address(0);

        source_frame.unref();

        assert_eq!(referenced_frame.width(), 2);
        assert_eq!(referenced_frame.height(), 2);
        assert_eq!(
            referenced_frame.software_format(),
            Some(SoftwarePixelFormat::Yuv420Planar8)
        );
        assert_eq!(
            referenced_frame.plane_data_address(0),
            referenced_y_plane_address
        );
        assert!(
            referenced_frame
                .plane_row_data(0, 0, 2)
                .expect("valid plane index")
                .is_some()
        );
        assert_eq!(
            referenced_frame
                .plane_row_data(0, 0, 2)
                .expect("valid first row")
                .expect("first row should exist"),
            &[10, 11]
        );
        assert_eq!(
            referenced_frame
                .plane_row_data(0, 1, 2)
                .expect("valid second row")
                .expect("second row should exist"),
            &[20, 21]
        );
    }

    #[cfg(feature = "ffmpeg")]
    #[test]
    fn frame_color_metadata_reads_hdr_side_data() {
        let frame = OwnedAvFrame::new().expect("frame allocation should succeed");

        // SAFETY: test owns the fresh AVFrame. Side-data buffers are allocated
        // by FFmpeg for the exact payload sizes and filled through local
        // repr(C) mirrors that match FFmpeg 8.1 headers.
        unsafe {
            let mastering_side_data = ffmpeg_sys_next::av_frame_new_side_data(
                frame.raw_frame.as_ptr(),
                ffmpeg_sys_next::AVFrameSideDataType::AV_FRAME_DATA_MASTERING_DISPLAY_METADATA,
                std::mem::size_of::<AvMasteringDisplayMetadataMirror>(),
            );
            assert!(
                !mastering_side_data.is_null(),
                "mastering side data allocation should succeed"
            );
            let mastering_metadata =
                (*mastering_side_data).data as *mut AvMasteringDisplayMetadataMirror;
            *mastering_metadata = AvMasteringDisplayMetadataMirror {
                display_primaries: [
                    [av_rational(34_000, 50_000), av_rational(16_000, 50_000)],
                    [av_rational(13_250, 50_000), av_rational(34_500, 50_000)],
                    [av_rational(7_500, 50_000), av_rational(3_000, 50_000)],
                ],
                white_point: [av_rational(15_635, 50_000), av_rational(16_450, 50_000)],
                min_luminance: av_rational(5, 1_000),
                max_luminance: av_rational(1_000, 1),
                has_primaries: 1,
                has_luminance: 1,
            };

            let content_light_side_data = ffmpeg_sys_next::av_frame_new_side_data(
                frame.raw_frame.as_ptr(),
                ffmpeg_sys_next::AVFrameSideDataType::AV_FRAME_DATA_CONTENT_LIGHT_LEVEL,
                std::mem::size_of::<AvContentLightMetadataMirror>(),
            );
            assert!(
                !content_light_side_data.is_null(),
                "content-light side data allocation should succeed"
            );
            let content_light_metadata =
                (*content_light_side_data).data as *mut AvContentLightMetadataMirror;
            *content_light_metadata = AvContentLightMetadataMirror {
                max_cll: 1_000,
                max_fall: 400,
            };
        }

        let color_metadata = frame.color_metadata();
        let mastering_display_metadata = color_metadata
            .mastering_display_metadata
            .expect("mastering display side data should be copied");
        let content_light_metadata = color_metadata
            .content_light_metadata
            .expect("content light side data should be copied");

        assert!(mastering_display_metadata.has_primaries);
        assert!(mastering_display_metadata.has_luminance);
        assert_eq!(
            mastering_display_metadata.max_luminance,
            FrameRational::new(1_000, 1)
        );
        assert_eq!(
            mastering_display_metadata.min_luminance,
            FrameRational::new(5, 1_000)
        );
        assert_eq!(content_light_metadata.max_content_light_level, 1_000);
        assert_eq!(content_light_metadata.max_frame_average_light_level, 400);
    }

    #[cfg(feature = "ffmpeg")]
    #[test]
    fn frame_unref_releases_plane_addresses() {
        let mut frame = OwnedAvFrame::new().expect("frame allocation should succeed");

        // SAFETY: test owns the fresh frame. Fields are set before
        // `av_frame_get_buffer`, as FFmpeg requires for video buffer allocation.
        unsafe {
            let raw_frame = frame.raw_frame.as_ptr();
            (*raw_frame).format = ffmpeg_sys_next::AVPixelFormat::AV_PIX_FMT_YUV420P as i32;
            (*raw_frame).width = 2;
            (*raw_frame).height = 2;

            let status = ffmpeg_sys_next::av_frame_get_buffer(raw_frame, 32);
            assert_eq!(status, 0, "av_frame_get_buffer should allocate data");
        }

        assert!(frame.plane_data_address(0).is_some());

        frame.unref();

        assert!(frame.plane_data_address(0).is_none());
    }

    #[cfg(feature = "ffmpeg")]
    const fn av_rational(num: i32, den: i32) -> ffmpeg_sys_next::AVRational {
        ffmpeg_sys_next::AVRational { num, den }
    }
}
