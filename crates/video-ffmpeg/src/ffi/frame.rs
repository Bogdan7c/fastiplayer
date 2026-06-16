//! Safe RAII owner for refcounted `AVFrame` data.

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
            }
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
}
