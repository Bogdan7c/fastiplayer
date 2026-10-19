//! Версии и базовое API FFmpeg, с которым процесс реально слинкован.
//!
//! Плеер линкуется с libavcodec/libavutil напрямую: если библиотеки нет,
//! процесс не стартует ещё до `main`. Поэтому проверять «есть ли FFmpeg»
//! поиском `.so` по системе бессмысленно и даже вредно — так находится
//! чужой FFmpeg (например, системный рядом с нашим vendored). Здесь мы
//! спрашиваем ровно те библиотеки, что загружены в процесс.

/// Снимок прилинкованного FFmpeg без raw-указателей наружу.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct LinkedFfmpegRuntime {
    /// Упакованная версия libavcodec (`AV_VERSION_INT`).
    pub avcodec_packed_version: u32,
    /// Упакованная версия libavutil (`AV_VERSION_INT`).
    pub avutil_packed_version: u32,
    /// libavcodec знает программный декодер H.264.
    pub has_h264_decoder: bool,
    /// libavutil отдаёт описание базового planar YUV 4:2:0.
    pub has_yuv420p_descriptor: bool,
}

/// Опрашивает прилинкованные libavcodec/libavutil.
///
/// Есть только в сборке с FFmpeg; сама структура доступна всегда, чтобы
/// классификацию результата можно было тестировать и без FFmpeg.
#[cfg(feature = "ffmpeg")]
#[must_use]
pub fn linked_ffmpeg_runtime() -> LinkedFfmpegRuntime {
    // SAFETY: функции без аргументов из C API FFmpeg (`unsigned fn(void)`),
    // библиотеки загружены динамическим загрузчиком до старта процесса.
    let avcodec_packed_version = unsafe { ffmpeg_sys_next::avcodec_version() };
    // SAFETY: см. выше, `unsigned avutil_version(void)`.
    let avutil_packed_version = unsafe { ffmpeg_sys_next::avutil_version() };

    // SAFETY: аргумент — валидное значение enum; null-результат обрабатывается.
    let h264_decoder = unsafe {
        ffmpeg_sys_next::avcodec_find_decoder(ffmpeg_sys_next::AVCodecID::AV_CODEC_ID_H264)
    };
    // SAFETY: аргумент — валидное значение enum; null-результат обрабатывается.
    let yuv420p_descriptor = unsafe {
        ffmpeg_sys_next::av_pix_fmt_desc_get(ffmpeg_sys_next::AVPixelFormat::AV_PIX_FMT_YUV420P)
    };

    LinkedFfmpegRuntime {
        avcodec_packed_version,
        avutil_packed_version,
        has_h264_decoder: !h264_decoder.is_null(),
        has_yuv420p_descriptor: !yuv420p_descriptor.is_null(),
    }
}
