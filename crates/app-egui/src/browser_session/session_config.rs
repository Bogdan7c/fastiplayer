//! Настройки процесса браузерного режима: config.toml только на чтение.
//!
//! Браузерный режим делит config с обычным плеером (решение 46), но сам ничего на диск
//! не пишет (инвариант 7): ни default-файла при первом запуске, ни `.bak` битого файла,
//! ни миграции. Поэтому здесь нет `load_or_create`/`load_or_recover_at` — только
//! [`fastiplayer_config::load_from_path`], который файл лишь читает.
//!
//! Решение владельца сессии 03b: битый config не мешает смотреть — берутся значения по
//! умолчанию и пишется одно предупреждение в журнал. Путь к файлу в журнал не пишется:
//! он ведёт в личный каталог, а журнал лежит на диске; причина — короткой меткой.
//!
//! Разделение ответственности:
//! - [`BrowserSessionSettings::load_read_only`] — край процесса (ищет каталог config-а,
//!   пишет предупреждение в журнал);
//! - [`BrowserSessionSettings::load_read_only_from`] — чистое решение «что делать с этим
//!   файлом», возвращает предупреждение значением, чтобы тест видел его без журнала.

use std::io;
use std::path::Path;

use fastiplayer_config::{AppConfig, BrowserSessionBufferWindow, ConfigError, ConfigPaths};
use tracing::warn;

/// Откуда взялись настройки этого запуска.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum BrowserSettingsOrigin {
    /// Прочитан существующий config.toml.
    ConfigFile,
    /// Файла нет — обычная ситуация до первого запуска обычного плеера; молча defaults.
    DefaultsWithoutFile,
    /// Файл есть, но им пользоваться нельзя — defaults и предупреждение.
    DefaultsAfterBrokenConfig(BrokenConfigReason),
}

/// Почему config.toml не годится (метка для журнала, без пути и содержимого файла).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum BrokenConfigReason {
    /// Не удалось определить каталог config-а пользователя.
    ConfigDirectoryUnknown,
    /// Файл есть, но не читается (права, это каталог, ошибка ввода-вывода).
    Unreadable,
    /// Это не TOML или не та схема (опечатка, неизвестное поле, файл новой версии).
    NotMatchingSchema,
    /// TOML разобран, но значение вне допустимого диапазона.
    InvalidValue,
}

impl BrokenConfigReason {
    /// Стабильная метка для журнала (её читают e2e-тесты и разбор логов).
    pub(crate) const fn log_label(self) -> &'static str {
        match self {
            Self::ConfigDirectoryUnknown => "config_directory_unknown",
            Self::Unreadable => "unreadable",
            Self::NotMatchingSchema => "not_matching_schema",
            Self::InvalidValue => "invalid_value",
        }
    }

    /// Переводит ошибку чтения config-а в метку. `None` — файла просто нет (не поломка).
    fn classify(error: &ConfigError) -> Option<Self> {
        match error {
            ConfigError::ReadConfigFile { source, .. }
                if source.kind() == io::ErrorKind::NotFound =>
            {
                None
            }
            ConfigError::ProjectDirsUnavailable => Some(Self::ConfigDirectoryUnknown),
            ConfigError::ParseConfigFile { .. } => Some(Self::NotMatchingSchema),
            ConfigError::ValidateConfigFile { .. } | ConfigError::InvalidValue { .. } => {
                Some(Self::InvalidValue)
            }
            // Остальные варианты относятся к записи/генерации config-а и при чтении не
            // возникают; если появятся — честнее считать файл нечитаемым, чем молчать.
            _ => Some(Self::Unreadable),
        }
    }
}

/// Настройки браузерного режима на всё время процесса (снимок при старте).
///
/// Владеет проверенным `AppConfig`; наружу отдаёт осмысленные значения, а не поля
/// документа. `app_config()` нужен только сборке `PlayerWorkerConfig::from_app_config`,
/// чтобы планировщик и лимиты декодера собирались тем же кодом, что у обычного плеера.
#[derive(Debug, Clone)]
pub(crate) struct BrowserSessionSettings {
    /// Проверенный config (из файла или значения по умолчанию).
    app_config: AppConfig,
    /// Откуда он взялся.
    origin: BrowserSettingsOrigin,
}

impl BrowserSessionSettings {
    /// Значения по умолчанию без чтения диска.
    pub(crate) fn defaults() -> Self {
        Self::from_defaults(BrowserSettingsOrigin::DefaultsWithoutFile)
    }

    /// Настройки из готового config-а (тесты воспроизведения задают лимиты декодера).
    #[cfg(test)]
    pub(crate) fn from_app_config_for_test(app_config: AppConfig) -> Self {
        Self {
            app_config,
            origin: BrowserSettingsOrigin::ConfigFile,
        }
    }

    /// Край процесса: находит config.toml пользователя и читает его, ничего не создавая.
    ///
    /// Битый файл → defaults и одно предупреждение в журнале (без пути к файлу).
    pub(crate) fn load_read_only() -> Self {
        let settings = match ConfigPaths::discover() {
            Ok(paths) => Self::load_read_only_from(&paths.config_file),
            Err(error) => Self::from_load_error(&error),
        };
        if let BrowserSettingsOrigin::DefaultsAfterBrokenConfig(reason) = settings.origin {
            warn!(
                reason = reason.log_label(),
                "Browser session config is not usable; using default settings"
            );
        }
        settings
    }

    /// Читает конкретный файл только на чтение и решает, что делать при ошибке.
    ///
    /// Журнал не пишет: предупреждение возвращается в [`Self::origin`].
    pub(crate) fn load_read_only_from(config_file: &Path) -> Self {
        match fastiplayer_config::load_from_path(config_file) {
            Ok(loaded) => Self {
                app_config: loaded.config,
                origin: BrowserSettingsOrigin::ConfigFile,
            },
            Err(error) => Self::from_load_error(&error),
        }
    }

    /// Откуда взялись настройки (для журнала старта и тестов).
    pub(crate) const fn origin(&self) -> BrowserSettingsOrigin {
        self.origin
    }

    /// Проверенный config для сборки production-конфигурации плеера.
    pub(crate) const fn app_config(&self) -> &AppConfig {
        &self.app_config
    }

    /// Окно буфера источника «байты от страницы»: сколько держать вперёд и позади.
    pub(crate) fn buffer_window(&self) -> BrowserSessionBufferWindow {
        self.app_config.browser_session.buffer_window()
    }

    /// Устройство вывода звука, выбранное пользователем в обычном плеере.
    pub(crate) fn audio_output_device_id(&self) -> &str {
        &self.app_config.audio.output_device
    }

    /// Defaults после ошибки чтения: «нет файла» молча, всё остальное — с причиной.
    fn from_load_error(error: &ConfigError) -> Self {
        match BrokenConfigReason::classify(error) {
            None => Self::defaults(),
            Some(reason) => {
                Self::from_defaults(BrowserSettingsOrigin::DefaultsAfterBrokenConfig(reason))
            }
        }
    }

    /// Собирает настройки из значений по умолчанию с указанным происхождением.
    fn from_defaults(origin: BrowserSettingsOrigin) -> Self {
        Self {
            app_config: AppConfig::default(),
            origin,
        }
    }
}

#[cfg(test)]
mod tests;
