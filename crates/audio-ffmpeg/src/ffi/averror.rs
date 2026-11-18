//! Коды ошибок FFmpeg (`AVERROR`) → типизированные ошибки crate-а.

use std::ffi::CStr;

use crate::error::AudioFfmpegError;

/// `AVERROR(EAGAIN)`: декодеру нужен следующий пакет / кадров больше нет.
pub(super) const AVERROR_EAGAIN: i32 = ffmpeg_sys_next::AVERROR(libc::EAGAIN);
/// `AVERROR_EOF`: декодер полностью опустошён.
pub(super) const AVERROR_EOF: i32 = ffmpeg_sys_next::AVERROR_EOF;
/// `AVERROR_INVALIDDATA`: битые данные пакета.
const AVERROR_INVALIDDATA: i32 = ffmpeg_sys_next::AVERROR_INVALIDDATA;

/// `AVERROR(ENOMEM)`: FFmpeg не хватило памяти.
const AVERROR_ENOMEM: i32 = ffmpeg_sys_next::AVERROR(libc::ENOMEM);
/// `AVERROR(EINVAL)`: неверный вызов API (не данные пакета).
const AVERROR_EINVAL: i32 = ffmpeg_sys_next::AVERROR(libc::EINVAL);

/// Длина буфера для `av_strerror` (FFmpeg: `AV_ERROR_MAX_STRING_SIZE` = 64).
const ERROR_MESSAGE_BUFFER_BYTES: usize = 64;

/// Переводит отрицательный код FFmpeg в ошибку: битые данные — ошибка пакета,
/// остальное — ошибка операции с исходным кодом.
pub(super) fn error_from_averror(operation: &'static str, code: i32) -> AudioFfmpegError {
    let message = averror_message(code);
    if code == AVERROR_INVALIDDATA {
        AudioFfmpegError::CorruptPacket { message }
    } else {
        AudioFfmpegError::Ffmpeg {
            operation,
            code,
            message,
        }
    }
}

/// Ошибка на пути декодирования пакета (`avcodec_send_packet` / `avcodec_receive_frame`).
///
/// Встроенный AAC-декодер на битых данных возвращает не только
/// `AVERROR_INVALIDDATA`: при ошибке заголовка кадра FFmpeg 8.1 отдаёт
/// внутренний отрицательный код без текста (проверено пробой на мусоре `0xFF`).
/// Поэтому здесь всё, кроме нехватки памяти и неверного вызова API, — ошибка
/// этого пакета: декодер остаётся годным для следующих.
pub(super) fn packet_error_from_averror(operation: &'static str, code: i32) -> AudioFfmpegError {
    match code {
        AVERROR_ENOMEM => AudioFfmpegError::AllocationFailed { operation },
        AVERROR_EINVAL => error_from_averror(operation, code),
        _ => AudioFfmpegError::CorruptPacket {
            message: format!("{operation}: {} ({code})", averror_message(code)),
        },
    }
}

/// Текст ошибки FFmpeg; при неизвестном коде FFmpeg сам пишет общий текст.
fn averror_message(code: i32) -> String {
    let mut message_buffer = [0 as libc::c_char; ERROR_MESSAGE_BUFFER_BYTES];
    // SAFETY: буфер выделен на стеке и его длина передана FFmpeg; `av_strerror`
    // всегда завершает строку нулём в пределах переданной длины.
    let status = unsafe {
        ffmpeg_sys_next::av_strerror(code, message_buffer.as_mut_ptr(), message_buffer.len())
    };
    if status < 0 {
        return format!("unknown FFmpeg error {code}");
    }
    // SAFETY: после успешного `av_strerror` буфер содержит строку с нулём в конце.
    let message = unsafe { CStr::from_ptr(message_buffer.as_ptr()) };
    message.to_string_lossy().into_owned()
}
