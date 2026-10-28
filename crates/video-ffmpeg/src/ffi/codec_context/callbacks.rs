//! Callback-и, которые FFmpeg вызывает из `AVCodecContext` (вынесено из `codec_context`).
//!
//! `get_format` выбирает software pixel format из разрешённых adapter-ом, наш
//! `get_buffer2` (cpu-parity 08b) кладёт кадры в блоки `HostFrameBlockPool`. Оба читают
//! одно состояние — `CodecCallbacks`, на которое указывает `AVCodecContext.opaque`.

use std::sync::Arc;

use crate::ffi::direct_frame_buffers::allocate_frame_in_pool;
use crate::ffi::host_frame_memory::{HostFrameBlockPool, HostFrameFallbackRecord};
use crate::ffi::pixel_format::{SoftwarePixelFormatSet, av_pixel_format_is_software};

/// Состояние callback-ов FFmpeg, на которое указывает `AVCodecContext.opaque`.
#[derive(Debug)]
pub(super) struct CodecCallbacks {
    /// Выбор software pixel format в `get_format`.
    pub(super) format_negotiator: FormatNegotiator,
    /// Пул блоков нашего `get_buffer2`; `None` — кадры выделяет FFmpeg.
    pub(super) frame_block_pool: Option<Arc<HostFrameBlockPool>>,
}

impl CodecCallbacks {
    pub(super) fn as_mut_ptr(&mut self) -> *mut std::ffi::c_void {
        self as *mut Self as *mut std::ffi::c_void
    }
}

#[derive(Debug)]
pub(super) struct FormatNegotiator {
    accepted_pixel_formats: SoftwarePixelFormatSet,
}

impl FormatNegotiator {
    pub(super) fn new(accepted_pixel_formats: SoftwarePixelFormatSet) -> Self {
        Self {
            accepted_pixel_formats,
        }
    }

    fn accepts(&self, pixel_format: ffmpeg_sys_next::AVPixelFormat) -> bool {
        self.accepted_pixel_formats
            .contains_av_pixel_format(pixel_format)
            && av_pixel_format_is_software(pixel_format)
    }
}

pub(super) unsafe extern "C" fn select_software_pixel_format(
    context: *mut ffmpeg_sys_next::AVCodecContext,
    offered_formats: *const ffmpeg_sys_next::AVPixelFormat,
) -> ffmpeg_sys_next::AVPixelFormat {
    if context.is_null() || offered_formats.is_null() {
        return ffmpeg_sys_next::AVPixelFormat::AV_PIX_FMT_NONE;
    }

    // SAFETY: FFmpeg передаёт pointer на текущий `AVCodecContext`.
    // Мы читаем только `opaque`, который сами установили до `avcodec_open2`.
    let opaque = unsafe { (*context).opaque };

    if opaque.is_null() {
        return ffmpeg_sys_next::AVPixelFormat::AV_PIX_FMT_NONE;
    }

    // SAFETY: `opaque` указывает на `CodecCallbacks` в `CodecContext`.
    // Box не перемещается, живёт не меньше context-а, callback не вызывается
    // после `avcodec_free_context`. State immutable, поэтому thread-safe для
    // documented non-concurrent `get_format` calls.
    let format_negotiator = unsafe { &(*(opaque as *const CodecCallbacks)).format_negotiator };
    let mut format_index = 0usize;

    loop {
        // SAFETY: FFmpeg передаёт список `AVPixelFormat`, завершённый
        // `AV_PIX_FMT_NONE`. Loop останавливается на sentinel-е.
        let candidate = unsafe { *offered_formats.add(format_index) };

        if candidate == ffmpeg_sys_next::AVPixelFormat::AV_PIX_FMT_NONE {
            return ffmpeg_sys_next::AVPixelFormat::AV_PIX_FMT_NONE;
        }

        if format_negotiator.accepts(candidate) {
            return candidate;
        }

        format_index += 1;

        if format_index > 256 {
            return ffmpeg_sys_next::AVPixelFormat::AV_PIX_FMT_NONE;
        }
    }
}

/// `get_buffer2` для `SoftwareFrameMemory::ImportableBlocks`: кадр в блок нашего пула.
///
/// Если кадр не подходит (`DirectFrameBufferFallback`), отдаёт его обычному выделению
/// FFmpeg — так требует контракт `get_buffer2` и так кадр просто пойдёт через staging.
pub(super) unsafe extern "C" fn get_importable_frame_buffer(
    context: *mut ffmpeg_sys_next::AVCodecContext,
    frame: *mut ffmpeg_sys_next::AVFrame,
    flags: std::os::raw::c_int,
) -> std::os::raw::c_int {
    // SAFETY: FFmpeg передаёт живой context; `opaque` мы установили до `avcodec_open2`
    // на `CodecCallbacks`, который живёт дольше context-а.
    let pool = unsafe {
        let opaque = (*context).opaque;
        if opaque.is_null() {
            None
        } else {
            (*(opaque as *const CodecCallbacks))
                .frame_block_pool
                .as_ref()
        }
    };
    if let Some(pool) = pool {
        // SAFETY: context/frame переданы FFmpeg в `get_buffer2` и живы на время вызова.
        match unsafe { allocate_frame_in_pool(context, frame, pool) } {
            Ok(()) => return 0,
            Err(fallback) => {
                // Кадр уйдёт в память FFmpeg и покажется копией: причина — в лог,
                // один раз на декодер, иначе путь «без копии» молча не работал бы.
                if pool.record_fallback() == HostFrameFallbackRecord::FirstForPool {
                    log::warn!(
                        "software decoder frame does not fit an importable block ({fallback:?}): \
                         this and later such frames stay in FFmpeg memory and are uploaded with a CPU copy"
                    );
                }
            }
        }
    }
    // SAFETY: тот же context/frame/flags, что FFmpeg передал нам; documented fallback.
    unsafe { ffmpeg_sys_next::avcodec_default_get_buffer2(context, frame, flags) }
}

/// Можно ли декодеру дать свой `get_buffer2` (`AV_CODEC_CAP_DR1`).
pub(super) fn decoder_supports_custom_frame_buffers(
    decoder: *const ffmpeg_sys_next::AVCodec,
) -> bool {
    // SAFETY: caller передаёт non-null pointer из реестра FFmpeg.
    let capabilities = unsafe { (*decoder).capabilities };
    capabilities & ffmpeg_sys_next::AV_CODEC_CAP_DR1 as i32 != 0
}
