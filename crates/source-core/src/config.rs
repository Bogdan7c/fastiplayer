use std::time::Duration;

use rustiplayer_config::NetworkConfig;

use crate::{SourceError, SourceResult};

/// Source-настройки, нормализованные из пользовательского `network` config.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SourceRuntimeConfig {
    /// Общий RAM budget на один media source.
    memory_cache_bytes: u64,

    /// Read-ahead budget для future cache manager.
    read_ahead_bytes: u64,

    /// Timeout установления HTTP соединения.
    connect_timeout: Duration,

    /// Timeout чтения HTTP ответа.
    read_timeout: Duration,
}

impl SourceRuntimeConfig {
    /// Конвертирует validated `NetworkConfig` в runtime-настройки source слоя.
    pub fn from_network_config(network_config: &NetworkConfig) -> SourceResult<Self> {
        let memory_cache_bytes = network_config
            .memory_cache_mb
            .checked_mul(1024)
            .and_then(|megabytes| megabytes.checked_mul(1024))
            .ok_or(SourceError::InvalidConfig {
                field: "network.memory_cache_mb",
                message: "значение не помещается в байтовый budget".to_string(),
            })?;
        let read_ahead_bytes = network_config
            .read_ahead_mb
            .checked_mul(1024)
            .and_then(|megabytes| megabytes.checked_mul(1024))
            .ok_or(SourceError::InvalidConfig {
                field: "network.read_ahead_mb",
                message: "значение не помещается в байтовый budget".to_string(),
            })?;
        if network_config.connect_timeout_ms == 0 {
            return Err(SourceError::InvalidConfig {
                field: "network.connect_timeout_ms",
                message: "timeout подключения должен быть положительным".to_string(),
            });
        }

        if network_config.read_timeout_ms == 0 {
            return Err(SourceError::InvalidConfig {
                field: "network.read_timeout_ms",
                message: "timeout чтения должен быть положительным".to_string(),
            });
        }

        Ok(Self {
            memory_cache_bytes,
            read_ahead_bytes,
            connect_timeout: Duration::from_millis(network_config.connect_timeout_ms),
            read_timeout: Duration::from_millis(network_config.read_timeout_ms),
        })
    }

    /// Возвращает RAM budget на один media source.
    #[must_use]
    pub const fn memory_cache_bytes(&self) -> u64 {
        self.memory_cache_bytes
    }

    /// Возвращает read-ahead budget.
    #[must_use]
    pub const fn read_ahead_bytes(&self) -> u64 {
        self.read_ahead_bytes
    }

    /// Возвращает timeout подключения к HTTP source.
    #[must_use]
    pub const fn connect_timeout(&self) -> Duration {
        self.connect_timeout
    }

    /// Возвращает timeout чтения HTTP source.
    #[must_use]
    pub const fn read_timeout(&self) -> Duration {
        self.read_timeout
    }

    /// Создаёт компактный config для unit-тестов cache/source логики.
    #[cfg(test)]
    pub(crate) const fn for_tests(
        memory_cache_bytes: u64,
        connect_timeout: Duration,
        read_timeout: Duration,
    ) -> Self {
        Self {
            memory_cache_bytes,
            read_ahead_bytes: memory_cache_bytes,
            connect_timeout,
            read_timeout,
        }
    }
}

#[cfg(test)]
mod tests {
    use rustiplayer_config::NetworkConfig;

    use super::*;

    /// Проверяет конвертацию всех network budgets в bytes/runtime durations.
    #[test]
    fn source_runtime_config_keeps_cache_readahead_and_timeouts() {
        let config = NetworkConfig {
            memory_cache_mb: 2,
            read_ahead_mb: 3,
            prefetch_initial_chunk_kb: 64,
            prefetch_chunk_mb: 1,
            connect_timeout_ms: 4,
            read_timeout_ms: 5,
        };

        let runtime = SourceRuntimeConfig::from_network_config(&config).expect("config valid");

        assert_eq!(runtime.memory_cache_bytes(), 2 * 1024 * 1024);
        assert_eq!(runtime.read_ahead_bytes(), 3 * 1024 * 1024);
        assert_eq!(runtime.connect_timeout(), Duration::from_millis(4));
        assert_eq!(runtime.read_timeout(), Duration::from_millis(5));
    }
}
