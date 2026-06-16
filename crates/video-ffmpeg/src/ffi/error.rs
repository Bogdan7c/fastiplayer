//! Typed ошибки raw FFmpeg boundary.

use thiserror::Error;

/// Result alias для safe wrappers поверх FFmpeg FFI.
pub type FfiResult<T> = Result<T, FfmpegFfiError>;

/// Ошибка, которую FFI layer отдаёт наружу вместо raw negative status codes.
#[derive(Debug, Error, Clone, PartialEq, Eq)]
pub enum FfmpegFfiError {
    /// Crate собран без optional FFmpeg feature.
    #[error("video-ffmpeg собран без feature `ffmpeg`")]
    FeatureDisabled,

    /// Текущая сессия добавляет scaffold, но не реализует decode operation.
    #[error("FFmpeg operation `{operation}` ещё не реализована в scaffold")]
    OperationUnsupported {
        /// Имя операции, чтобы diagnostic не терял контекст.
        operation: &'static str,
    },

    /// FFmpeg вернул отрицательный status code.
    #[error("FFmpeg operation `{operation}` failed with status {status}")]
    RawStatus {
        /// Имя FFI операции или high-level действия.
        operation: &'static str,

        /// Raw FFmpeg status code.
        status: i32,
    },
}

/// Явный helper для операций, которые появятся только в следующих сессиях.
pub(crate) fn unsupported(operation: &'static str) -> FfmpegFfiError {
    FfmpegFfiError::OperationUnsupported { operation }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn unsupported_error_keeps_operation_name() {
        let error = unsupported("avcodec_send_packet");

        assert_eq!(
            error.to_string(),
            "FFmpeg operation `avcodec_send_packet` ещё не реализована в scaffold"
        );
    }
}
