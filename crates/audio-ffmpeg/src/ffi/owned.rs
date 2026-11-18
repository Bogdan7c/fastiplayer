//! Владельцы сырых структур FFmpeg: каждая освобождается ровно один раз в `Drop`.
//!
//! Обёртки не `Send` сами по себе (сырые указатели). Право переезда между
//! потоками даёт только `AacCodecContext`, который владеет всеми тремя сразу
//! и обосновывает это в своём `unsafe impl Send`.

use std::ptr::NonNull;

use crate::error::AudioFfmpegError;

/// Владелец `AVCodecContext`.
pub(super) struct OwnedCodecContext(NonNull<ffmpeg_sys_next::AVCodecContext>);

impl OwnedCodecContext {
    /// Выделяет контекст для найденного декодера.
    pub(super) fn allocate(
        codec: NonNull<ffmpeg_sys_next::AVCodec>,
    ) -> Result<Self, AudioFfmpegError> {
        // SAFETY: `codec` — ненулевой указатель из `avcodec_find_decoder`;
        // функция возвращает новый контекст или null.
        let raw_context = unsafe { ffmpeg_sys_next::avcodec_alloc_context3(codec.as_ptr()) };
        NonNull::new(raw_context)
            .map(Self)
            .ok_or(AudioFfmpegError::AllocationFailed {
                operation: "avcodec_alloc_context3",
            })
    }

    pub(super) fn as_ptr(&self) -> *mut ffmpeg_sys_next::AVCodecContext {
        self.0.as_ptr()
    }
}

impl Drop for OwnedCodecContext {
    fn drop(&mut self) {
        let mut raw_context = self.0.as_ptr();
        // SAFETY: контекст принадлежит только этой обёртке; `avcodec_free_context`
        // освобождает и `extradata`, которую мы ему передали, и обнуляет указатель.
        unsafe { ffmpeg_sys_next::avcodec_free_context(&mut raw_context) };
    }
}

/// Владелец переиспользуемого `AVPacket`.
pub(super) struct OwnedPacket(NonNull<ffmpeg_sys_next::AVPacket>);

impl OwnedPacket {
    pub(super) fn allocate() -> Result<Self, AudioFfmpegError> {
        // SAFETY: функция без аргументов возвращает новый пакет или null.
        let raw_packet = unsafe { ffmpeg_sys_next::av_packet_alloc() };
        NonNull::new(raw_packet)
            .map(Self)
            .ok_or(AudioFfmpegError::AllocationFailed {
                operation: "av_packet_alloc",
            })
    }

    pub(super) fn as_ptr(&self) -> *mut ffmpeg_sys_next::AVPacket {
        self.0.as_ptr()
    }
}

impl Drop for OwnedPacket {
    fn drop(&mut self) {
        let mut raw_packet = self.0.as_ptr();
        // SAFETY: пакет принадлежит только этой обёртке; `av_packet_free`
        // сначала снимает ссылку на данные, потом освобождает структуру.
        unsafe { ffmpeg_sys_next::av_packet_free(&mut raw_packet) };
    }
}

/// Владелец переиспользуемого `AVFrame`.
pub(super) struct OwnedFrame(NonNull<ffmpeg_sys_next::AVFrame>);

impl OwnedFrame {
    pub(super) fn allocate() -> Result<Self, AudioFfmpegError> {
        // SAFETY: функция без аргументов возвращает новый кадр или null.
        let raw_frame = unsafe { ffmpeg_sys_next::av_frame_alloc() };
        NonNull::new(raw_frame)
            .map(Self)
            .ok_or(AudioFfmpegError::AllocationFailed {
                operation: "av_frame_alloc",
            })
    }

    pub(super) fn as_ptr(&self) -> *mut ffmpeg_sys_next::AVFrame {
        self.0.as_ptr()
    }
}

impl Drop for OwnedFrame {
    fn drop(&mut self) {
        let mut raw_frame = self.0.as_ptr();
        // SAFETY: кадр принадлежит только этой обёртке; `av_frame_free`
        // снимает ссылки на буферы и освобождает структуру.
        unsafe { ffmpeg_sys_next::av_frame_free(&mut raw_frame) };
    }
}
