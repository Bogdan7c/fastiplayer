/// Конфигурация пути доставки decoded frame в wgpu-текстуры.
///
/// По умолчанию используется CPU upload, потому что он копирует уже
/// синхронизированные NV12-плоскости через `wgpu::Queue::write_texture`
/// и не зависит от корректности Vulkan external memory import.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct UploadConfig {
    /// Разрешает экспериментальный zero-copy импорт DMA-BUF в Vulkan texture.
    pub enable_dma_buf_zero_copy: bool,
}

impl UploadConfig {
    /// Имя переменной окружения, которая явно включает zero-copy путь.
    pub const ZERO_COPY_ENV_VAR: &'static str = "VIDEOPLAYER_ZERO_COPY_DMA_BUF";

    /// Читает конфигурацию из окружения процесса.
    ///
    /// Значения `1`, `true`, `yes`, `on` включают zero-copy.
    /// Любое другое значение оставляет безопасный CPU upload.
    pub fn from_env() -> Self {
        let zero_copy_value = std::env::var(Self::ZERO_COPY_ENV_VAR).ok();
        let enable_dma_buf_zero_copy = zero_copy_value
            .as_deref()
            .map(Self::is_enabled_value)
            .unwrap_or(false);

        Self {
            enable_dma_buf_zero_copy,
        }
    }

    /// Проверяет текстовое значение feature flag без паники и unwrap.
    fn is_enabled_value(value: &str) -> bool {
        matches!(
            value.trim().to_ascii_lowercase().as_str(),
            "1" | "true" | "yes" | "on"
        )
    }
}

#[cfg(test)]
mod tests {
    use super::UploadConfig;

    /// Проверяем все поддерживаемые значения, которые включают zero-copy.
    #[test]
    fn enabled_values_are_accepted() {
        for enabled_value in ["1", "true", "TRUE", "yes", "on", " on "] {
            assert!(UploadConfig::is_enabled_value(enabled_value));
        }
    }

    /// Проверяем, что неизвестные значения не включают экспериментальный путь.
    #[test]
    fn unknown_values_keep_safe_cpu_upload() {
        for disabled_value in ["0", "false", "off", "no", "", "maybe"] {
            assert!(!UploadConfig::is_enabled_value(disabled_value));
        }
    }
}
