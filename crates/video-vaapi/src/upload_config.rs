/// Конфигурация пути доставки decoded frame в wgpu-текстуры.
///
/// По умолчанию используется DMA-BUF zero-copy путь.
///
/// CPU upload остаётся диагностическим fallback-режимом: его можно явно включить
/// через `VIDEOPLAYER_ZERO_COPY_DMA_BUF=0`, если нужно сравнить поведение с
/// копированием через `wgpu::Queue::write_texture`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct UploadConfig {
    /// Разрешает zero-copy импорт DMA-BUF в Vulkan texture.
    pub enable_dma_buf_zero_copy: bool,
}

impl UploadConfig {
    /// Имя переменной окружения, которая управляет zero-copy путём.
    pub const ZERO_COPY_ENV_VAR: &'static str = "VIDEOPLAYER_ZERO_COPY_DMA_BUF";

    /// Читает конфигурацию из окружения процесса.
    ///
    /// Если переменная не задана, zero-copy включён по умолчанию.
    /// Значения `0`, `false`, `no`, `off` явно выключают zero-copy.
    /// Значения `1`, `true`, `yes`, `on` явно оставляют zero-copy включённым.
    pub fn from_env() -> Self {
        let zero_copy_value = std::env::var(Self::ZERO_COPY_ENV_VAR).ok();
        let enable_dma_buf_zero_copy = zero_copy_value
            .as_deref()
            .map(Self::parse_zero_copy_flag)
            .unwrap_or(true);

        Self {
            enable_dma_buf_zero_copy,
        }
    }

    /// Разбирает текстовое значение feature flag без паники и unwrap.
    ///
    /// Неизвестные значения сохраняют новый production default: zero-copy включён.
    /// Это защищает запуск от случайных нестандартных значений окружения.
    fn parse_zero_copy_flag(value: &str) -> bool {
        if Self::is_disabled_value(value) {
            return false;
        }

        if Self::is_enabled_value(value) {
            return true;
        }

        true
    }

    /// Проверяет, что текстовое значение явно выключает zero-copy.
    fn is_disabled_value(value: &str) -> bool {
        matches!(
            value.trim().to_ascii_lowercase().as_str(),
            "0" | "false" | "no" | "off"
        )
    }

    /// Проверяет, что текстовое значение явно включает zero-copy.
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
            assert!(UploadConfig::parse_zero_copy_flag(enabled_value));
        }
    }

    /// Проверяем все поддерживаемые значения, которые выключают zero-copy.
    #[test]
    fn disabled_values_are_accepted() {
        for disabled_value in ["0", "false", "FALSE", "off", "no", " no "] {
            assert!(!UploadConfig::parse_zero_copy_flag(disabled_value));
        }
    }

    /// Проверяем, что неизвестные значения сохраняют production default.
    #[test]
    fn unknown_values_keep_zero_copy_default() {
        for default_value in ["", "maybe", "enabled", "auto"] {
            assert!(UploadConfig::parse_zero_copy_flag(default_value));
        }
    }
}
