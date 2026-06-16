//! Safe placeholder for future `AVCodecContext` ownership.

use super::error::{FfiResult, unsupported};

/// Запрос на открытие FFmpeg codec context-а.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct FfmpegCodecContextRequest {
    /// Human-readable codec name для diagnostics scaffold-а.
    codec_name: String,
}

impl FfmpegCodecContextRequest {
    /// Создаёт request без обращения к FFmpeg registry.
    #[must_use]
    pub fn new(codec_name: impl Into<String>) -> Self {
        Self {
            codec_name: codec_name.into(),
        }
    }

    /// Возвращает codec name для error/reporting layers.
    #[must_use]
    pub fn codec_name(&self) -> &str {
        &self.codec_name
    }
}

/// Opaque owner для будущего `AVCodecContext`.
#[derive(Debug)]
pub struct FfmpegCodecContext {
    /// Поле закрывает struct literal снаружи crate-а.
    _private: (),
}

impl FfmpegCodecContext {
    /// Scaffold не открывает decoder context до реализации send/receive bridge.
    pub fn open(_request: &FfmpegCodecContextRequest) -> FfiResult<Self> {
        Err(unsupported("avcodec_open2"))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn codec_context_request_preserves_codec_name_for_diagnostics() {
        let request = FfmpegCodecContextRequest::new("h264");

        assert_eq!(request.codec_name(), "h264");
    }
}
