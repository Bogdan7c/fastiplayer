//! Typed ошибки raw FFmpeg boundary.

#[cfg(feature = "ffmpeg")]
use std::ffi::CStr;
#[cfg(feature = "ffmpeg")]
use std::os::raw::c_char;

use thiserror::Error;

/// Result alias для safe wrappers поверх FFmpeg FFI.
pub type FfiResult<T> = Result<T, FfmpegError>;

/// Backward-compatible alias для старого scaffold имени.
pub type FfmpegFfiError = FfmpegError;

/// Классификация FFmpeg `AVERROR` без передачи raw status наружу как `bool`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum FfmpegErrorKind {
    /// Операцию нужно повторить после смены направления send/receive.
    Again,

    /// Поток закончен или decoder уже flushed.
    EndOfFile,

    /// FFmpeg сообщил, что входные параметры изменились.
    InputChanged,

    /// FFmpeg сообщил, что выходные параметры изменились.
    OutputChanged,

    /// Входные данные битые или несовместимые с decoder-ом.
    InvalidData,

    /// Вызов нарушил контракт FFmpeg API.
    InvalidArgument,

    /// FFmpeg не смог выделить память.
    OutOfMemory,

    /// Decoder для codec id/name отсутствует.
    DecoderNotFound,

    /// FFmpeg вернул ошибку внешнего компонента.
    External,

    /// FFmpeg не смог точнее классифицировать ошибку.
    Unknown,

    /// Редкий `AVERROR`, который пока не имеет отдельной проектной ветки.
    Other,
}

/// Ошибка, которую FFI layer отдаёт наружу вместо raw negative status codes.
#[derive(Debug, Error, Clone, PartialEq, Eq)]
pub enum FfmpegError {
    /// Crate собран без optional FFmpeg feature.
    #[error("video-ffmpeg собран без feature `ffmpeg`")]
    FeatureDisabled,

    /// Операция ещё намеренно не подключена к playback path-у.
    #[error("FFmpeg operation `{operation}` ещё не реализована в scaffold")]
    OperationUnsupported {
        /// Имя операции, чтобы diagnostic не терял контекст.
        operation: &'static str,
    },

    /// FFmpeg allocator вернул null.
    #[error("FFmpeg allocation failed during `{operation}`")]
    AllocationFailed {
        /// Имя FFI операции или high-level действия.
        operation: &'static str,
    },

    /// Safe wrapper получил вход, который нельзя передать в FFmpeg корректно.
    #[error("invalid FFmpeg input for `{operation}`: {details}")]
    InvalidInput {
        /// Имя FFI операции или high-level действия.
        operation: &'static str,

        /// Человекочитаемая причина отказа.
        details: String,
    },

    /// Payload не помещается в `AVPacket.size`.
    #[error("encoded packet payload is too large for FFmpeg AVPacket: {payload_len} bytes")]
    PacketTooLarge {
        /// Длина compressed payload-а.
        payload_len: usize,
    },

    /// Decoder не найден по codec id/name.
    #[error("FFmpeg decoder `{codec}` was not found")]
    DecoderNotFound {
        /// Codec id/name из проектного request-а.
        codec: String,
    },

    /// Найденный decoder является hardware/hybrid backend-ом, а этот crate software-only.
    #[error("FFmpeg decoder `{codec}` rejected because hardware decode is not allowed here")]
    HardwareDecoderRejected {
        /// Имя decoder-а, которое вернул FFmpeg.
        codec: String,
    },

    /// FFmpeg вернул отрицательный `AVERROR`.
    #[error("FFmpeg operation `{operation}` failed with {kind:?} status {status}: {message}")]
    Status {
        /// Имя FFI операции или high-level действия.
        operation: &'static str,

        /// Raw negative `AVERROR` только для diagnostics и тестов mapping-а.
        status: i32,

        /// Проектная typed классификация.
        kind: FfmpegErrorKind,

        /// Текст из `av_strerror` при включённом FFmpeg feature или fallback.
        message: String,
    },
}

impl FfmpegError {
    /// Строит typed ошибку из отрицательного FFmpeg status code.
    #[must_use]
    pub fn from_averror(operation: &'static str, status: i32) -> Self {
        Self::Status {
            operation,
            status,
            kind: classify_averror(status),
            message: averror_message(status),
        }
    }

    /// Возвращает классификацию только для `Status` variant-а.
    #[must_use]
    pub const fn status_kind(&self) -> Option<FfmpegErrorKind> {
        match self {
            Self::Status { kind, .. } => Some(*kind),
            Self::FeatureDisabled
            | Self::OperationUnsupported { .. }
            | Self::AllocationFailed { .. }
            | Self::InvalidInput { .. }
            | Self::PacketTooLarge { .. }
            | Self::DecoderNotFound { .. }
            | Self::HardwareDecoderRejected { .. } => None,
        }
    }
}

/// Проектная классификация negative `AVERROR` values.
#[must_use]
pub const fn classify_averror(status: i32) -> FfmpegErrorKind {
    match status {
        AVERROR_EAGAIN_CODE => FfmpegErrorKind::Again,
        AVERROR_EOF_CODE => FfmpegErrorKind::EndOfFile,
        AVERROR_INPUT_CHANGED_CODE => FfmpegErrorKind::InputChanged,
        AVERROR_OUTPUT_CHANGED_CODE => FfmpegErrorKind::OutputChanged,
        AVERROR_INVALIDDATA_CODE => FfmpegErrorKind::InvalidData,
        AVERROR_EINVAL_CODE => FfmpegErrorKind::InvalidArgument,
        AVERROR_ENOMEM_CODE => FfmpegErrorKind::OutOfMemory,
        AVERROR_DECODER_NOT_FOUND_CODE => FfmpegErrorKind::DecoderNotFound,
        AVERROR_EXTERNAL_CODE => FfmpegErrorKind::External,
        AVERROR_UNKNOWN_CODE => FfmpegErrorKind::Unknown,
        _ => FfmpegErrorKind::Other,
    }
}

/// POSIX errno value, used by FFmpeg's `AVERROR(EAGAIN)`.
const ERRNO_EAGAIN: i32 = 11;

/// POSIX errno value, used by FFmpeg's `AVERROR(EINVAL)`.
const ERRNO_EINVAL: i32 = 22;

/// POSIX errno value, used by FFmpeg's `AVERROR(ENOMEM)`.
const ERRNO_ENOMEM: i32 = 12;

/// FFmpeg `AVERROR(EAGAIN)`.
pub const AVERROR_EAGAIN_CODE: i32 = av_error(ERRNO_EAGAIN);

/// FFmpeg `AVERROR(EINVAL)`.
pub const AVERROR_EINVAL_CODE: i32 = av_error(ERRNO_EINVAL);

/// FFmpeg `AVERROR(ENOMEM)`.
pub const AVERROR_ENOMEM_CODE: i32 = av_error(ERRNO_ENOMEM);

/// FFmpeg `AVERROR_EOF`.
pub const AVERROR_EOF_CODE: i32 = fferrtag(b'E', b'O', b'F', b' ');

/// FFmpeg `AVERROR_INPUT_CHANGED`.
pub const AVERROR_INPUT_CHANGED_CODE: i32 = -0x636e_6701;

/// FFmpeg `AVERROR_OUTPUT_CHANGED`.
pub const AVERROR_OUTPUT_CHANGED_CODE: i32 = -0x636e_6702;

/// FFmpeg `AVERROR_INVALIDDATA`.
pub const AVERROR_INVALIDDATA_CODE: i32 = fferrtag(b'I', b'N', b'D', b'A');

/// FFmpeg `AVERROR_DECODER_NOT_FOUND`.
pub const AVERROR_DECODER_NOT_FOUND_CODE: i32 = fferrtag(0xF8, b'D', b'E', b'C');

/// FFmpeg `AVERROR_EXTERNAL`.
pub const AVERROR_EXTERNAL_CODE: i32 = fferrtag(b'E', b'X', b'T', b' ');

/// FFmpeg `AVERROR_UNKNOWN`.
pub const AVERROR_UNKNOWN_CODE: i32 = fferrtag(b'U', b'N', b'K', b'N');

/// Const mirror of FFmpeg `AVERROR(e)` for supported Unix-like targets.
const fn av_error(errno: i32) -> i32 {
    -errno
}

/// Const mirror of FFmpeg `MKTAG`.
const fn mktag(a: u8, b: u8, c: u8, d: u8) -> i32 {
    (a as i32) | ((b as i32) << 8) | ((c as i32) << 16) | ((d as i32) << 24)
}

/// Const mirror of FFmpeg `FFERRTAG`.
const fn fferrtag(a: u8, b: u8, c: u8, d: u8) -> i32 {
    -mktag(a, b, c, d)
}

#[cfg(feature = "ffmpeg")]
fn averror_message(status: i32) -> String {
    let mut error_buffer = [0 as c_char; 128];

    // SAFETY: `error_buffer` живёт до конца функции, размер передаётся точно,
    // FFmpeg пишет C-строку не длиннее buffer size. Вызов не сохраняет pointer
    // и не требует синхронизации между threads.
    let _result = unsafe {
        ffmpeg_sys_next::av_strerror(status, error_buffer.as_mut_ptr(), error_buffer.len())
    };

    // SAFETY: `av_strerror` по контракту всегда оставляет в buffer
    // NUL-terminated diagnostic string, даже если конкретный code неизвестен.
    unsafe { CStr::from_ptr(error_buffer.as_ptr()) }
        .to_string_lossy()
        .into_owned()
}

#[cfg(not(feature = "ffmpeg"))]
fn averror_message(status: i32) -> String {
    format!("FFmpeg AVERROR status {status}")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn averror_mapping_keeps_distinct_decoder_states() {
        assert_eq!(
            classify_averror(AVERROR_EAGAIN_CODE),
            FfmpegErrorKind::Again
        );
        assert_eq!(
            classify_averror(AVERROR_EOF_CODE),
            FfmpegErrorKind::EndOfFile
        );
        assert_eq!(
            classify_averror(AVERROR_INPUT_CHANGED_CODE),
            FfmpegErrorKind::InputChanged
        );
        assert_eq!(
            classify_averror(AVERROR_OUTPUT_CHANGED_CODE),
            FfmpegErrorKind::OutputChanged
        );
        assert_eq!(
            classify_averror(AVERROR_INVALIDDATA_CODE),
            FfmpegErrorKind::InvalidData
        );
        assert_eq!(
            classify_averror(AVERROR_EXTERNAL_CODE),
            FfmpegErrorKind::External
        );
        assert_eq!(
            classify_averror(AVERROR_UNKNOWN_CODE),
            FfmpegErrorKind::Unknown
        );
    }

    #[test]
    fn averror_status_preserves_operation_code_and_kind() {
        let error = FfmpegError::from_averror("avcodec_receive_frame", AVERROR_EAGAIN_CODE);

        assert_eq!(error.status_kind(), Some(FfmpegErrorKind::Again));
        assert!(error.to_string().contains("avcodec_receive_frame"));
        assert!(error.to_string().contains("-11"));
    }

    #[cfg(feature = "ffmpeg")]
    #[test]
    fn local_averror_constants_match_ffmpeg_sys() {
        assert_eq!(
            AVERROR_EAGAIN_CODE,
            ffmpeg_sys_next::AVERROR(ffmpeg_sys_next::EAGAIN)
        );
        assert_eq!(AVERROR_EOF_CODE, ffmpeg_sys_next::AVERROR_EOF);
        assert_eq!(
            AVERROR_INVALIDDATA_CODE,
            ffmpeg_sys_next::AVERROR_INVALIDDATA
        );
        assert_eq!(AVERROR_EXTERNAL_CODE, ffmpeg_sys_next::AVERROR_EXTERNAL);
        assert_eq!(AVERROR_UNKNOWN_CODE, ffmpeg_sys_next::AVERROR_UNKNOWN);
    }
}
