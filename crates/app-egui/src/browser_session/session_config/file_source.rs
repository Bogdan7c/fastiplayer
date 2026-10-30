//! Настройки файла `<video src>` из секции `[network]` (сессия 03c моста).
//!
//! Решение владельца: файл браузера читается как обычная progressive-ссылка — то же
//! окно предзагрузки и те же повторы после сбоя сети, что у ссылок в обычном плеере.
//! Новых настроек не заводится: модуль только ЧИТАЕТ `[network]` (`read_ahead_mb`,
//! `prefetch_chunk_mb`, `prefetch_initial_chunk_kb`, `reconnect_wait_ms`) и переводит
//! их в типы источника. Перевод КиБ/МиБ в байты — общий с обычными ссылками
//! (`media_source_open::direct_progressive_open::prefetch_config`), второй копии нет.

use std::time::Duration;

use fastiplayer_config::NetworkConfig;
use media_prefetch::PrefetchConfig;
use pushed_media_source::{PushedFilePolicy, PushedFilePolicyError, RangeRetryPolicy};

/// Почему настройки файла не собрались.
#[derive(Debug, thiserror::Error)]
pub(crate) enum FileSourceSettingsError {
    /// Окно предзагрузки из `[network]` не годится (переполнение или вне пределов).
    #[error("the network prefetch settings are not usable for a browser file: {0}")]
    Prefetch(String),
    /// Пределы файла не сошлись (первый кусок больше запроса и т. п.).
    #[error("the browser file source policy is not valid: {0}")]
    Policy(#[from] PushedFilePolicyError),
}

/// Что `[network]` говорит о файле: окно предзагрузки и бюджет повторов.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) struct FileSourceSettings {
    /// Окно предзагрузки (то же, что у обычного progressive HTTP).
    prefetch: PrefetchConfig,
    /// Сколько ждать возвращения сети (`network.reconnect_wait_ms`; 0 — не ждать).
    reconnect_budget: Duration,
}

impl FileSourceSettings {
    /// Настройки из проверенной секции `[network]`.
    ///
    /// # Errors
    /// Окно предзагрузки не собралось (для проверенного config-а не бывает, но
    /// ошибка не глотается).
    pub(crate) fn from_network(network: &NetworkConfig) -> Result<Self, FileSourceSettingsError> {
        let prefetch = media_source_open::direct_progressive_open::prefetch_config(network)
            .map_err(|error| FileSourceSettingsError::Prefetch(format!("{error:#}")))?;
        Ok(Self {
            prefetch,
            reconnect_budget: Duration::from_millis(network.reconnect_wait_ms),
        })
    }

    /// Окно предзагрузки: им же главный цикл оборачивает открытый файл в фоновый демуксер.
    pub(crate) const fn prefetch(&self) -> PrefetchConfig {
        self.prefetch
    }

    /// Бюджет ожидания сети.
    pub(crate) const fn reconnect_budget(&self) -> Duration {
        self.reconnect_budget
    }

    /// Пределы источника файла: окно предзагрузки и повторы по бюджету.
    ///
    /// # Errors
    /// Пределы не сошлись (кусок prefetch не помещается в запрос).
    pub(crate) fn policy(&self) -> Result<PushedFilePolicy, FileSourceSettingsError> {
        Ok(PushedFilePolicy::new(
            self.prefetch,
            RangeRetryPolicy::with_budget(self.reconnect_budget),
        )?)
    }
}
