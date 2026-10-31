//! Настройки процесса браузерного режима: чтение config.toml при старте и право записи.
//!
//! Браузерный режим делит config с обычным плеером (решение 46). С 04c окно пишет в него
//! только запоминаемое и настроенное пользователем (решение 135) — общим путём под замком
//! «только тронутые ключи», но ничего не чинит: ни default-файла при старте, ни `.bak`
//! битого файла, ни миграции на диске. Поэтому здесь нет `load_or_create`/
//! `load_or_recover_at` — только [`fastiplayer_config::read_shared_config_at`], который
//! файл лишь читает и классифицирует. Заодно этот же вердикт решает право записи:
//! - файл прочитан или его нет → [`ConfigSavePolicy::WriteToFile`] (файл создаст первая запись);
//! - файл битый → `SessionOnly(BrokenConfigLeftToRegularPlayer)` (чинит обычный плеер);
//! - файл новее схемы → `SessionOnly(NewerSchemaVersion)`; не читается → `SessionOnly(..)`.
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

use fastiplayer_config::{
    AppConfig, BrokenConfigProblem, BrowserSessionBufferWindow, ConfigPaths, ConfigSavePolicy,
    ConfigSessionOnlyReason, SharedConfigProblem, SharedConfigRead,
};
use tracing::warn;

mod file_source;

pub(crate) use self::file_source::{FileSourceSettings, FileSourceSettingsError};

/// Откуда взялись настройки этого запуска.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum BrowserSettingsOrigin {
    /// Прочитан существующий config.toml.
    ConfigFile,
    /// Файла нет — обычная ситуация до первого запуска обычного плеера; молча defaults.
    DefaultsWithoutFile,
    /// Файл есть, но им пользоваться нельзя — defaults и предупреждение.
    DefaultsAfterBrokenConfig(BrokenConfigReason),
    /// Файл записан более новой версией плеера — его не трогаем, живём на defaults.
    DefaultsAfterNewerConfig {
        /// Версия схемы, найденная в файле.
        found: i64,
    },
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

    /// Переводит проблему файла в метку для журнала.
    fn classify(problem: &SharedConfigProblem) -> Self {
        match problem {
            SharedConfigProblem::Broken(
                BrokenConfigProblem::InvalidTomlSyntax | BrokenConfigProblem::SchemaMismatch,
            ) => Self::NotMatchingSchema,
            SharedConfigProblem::Broken(BrokenConfigProblem::InvalidValue { .. }) => {
                Self::InvalidValue
            }
            // Каталог вместо файла, ошибка ввода-вывода, непредвиденный разбор — файл
            // нельзя прочитать; честнее назвать это так, чем молчать.
            SharedConfigProblem::NotRegularFile
            | SharedConfigProblem::ReadFailed { .. }
            | SharedConfigProblem::UnclassifiedParseFailure => Self::Unreadable,
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
    /// Можно ли окну писать config (решается вердиктом о файле при чтении).
    save_policy: ConfigSavePolicy,
}

impl BrowserSessionSettings {
    /// Значения по умолчанию без чтения диска (как «файла нет»: запись создаст его).
    pub(crate) fn defaults() -> Self {
        Self::from_defaults(
            BrowserSettingsOrigin::DefaultsWithoutFile,
            ConfigSavePolicy::WriteToFile,
        )
    }

    /// Настройки из готового config-а (тесты воспроизведения задают лимиты декодера).
    #[cfg(test)]
    pub(crate) fn from_app_config_for_test(app_config: AppConfig) -> Self {
        Self {
            app_config,
            origin: BrowserSettingsOrigin::ConfigFile,
            save_policy: ConfigSavePolicy::WriteToFile,
        }
    }

    /// Край процесса: находит config.toml пользователя и читает его, ничего не создавая.
    ///
    /// Битый файл → defaults и одно предупреждение в журнале (без пути к файлу).
    pub(crate) fn load_read_only() -> Self {
        let settings = match ConfigPaths::discover() {
            Ok(paths) => Self::load_read_only_from(&paths.config_file),
            Err(_directory_unknown) => Self::from_defaults(
                BrowserSettingsOrigin::DefaultsAfterBrokenConfig(
                    BrokenConfigReason::ConfigDirectoryUnknown,
                ),
                ConfigSavePolicy::SessionOnly(ConfigSessionOnlyReason::ConfigDirectoryUnknown),
            ),
        };
        match settings.origin {
            BrowserSettingsOrigin::DefaultsAfterBrokenConfig(reason) => warn!(
                reason = reason.log_label(),
                "Browser session config is not usable; using default settings"
            ),
            BrowserSettingsOrigin::DefaultsAfterNewerConfig { found } => warn!(
                found_schema_version = found,
                "Browser session config was written by a newer Fastiplayer; using default settings"
            ),
            BrowserSettingsOrigin::ConfigFile | BrowserSettingsOrigin::DefaultsWithoutFile => {}
        }
        settings
    }

    /// Читает конкретный файл только на чтение и решает, что делать с ним и с записью.
    ///
    /// Журнал не пишет: предупреждение возвращается в [`Self::origin`].
    pub(crate) fn load_read_only_from(config_file: &Path) -> Self {
        match fastiplayer_config::read_shared_config_at(config_file) {
            SharedConfigRead::Parsed(app_config) => Self {
                app_config: *app_config,
                origin: BrowserSettingsOrigin::ConfigFile,
                save_policy: ConfigSavePolicy::WriteToFile,
            },
            // Файла нет — обычная ситуация до первого запуска обычного плеера; первая
            // запись окна создаст файл (под замком, из defaults + свои ключи).
            SharedConfigRead::Missing => Self::defaults(),
            SharedConfigRead::Newer { found } => Self::from_defaults(
                BrowserSettingsOrigin::DefaultsAfterNewerConfig { found },
                ConfigSavePolicy::SessionOnly(ConfigSessionOnlyReason::NewerSchemaVersion {
                    found,
                }),
            ),
            SharedConfigRead::Unreadable { problem } => Self::from_defaults(
                BrowserSettingsOrigin::DefaultsAfterBrokenConfig(BrokenConfigReason::classify(
                    &problem,
                )),
                ConfigSavePolicy::SessionOnly(session_only_reason_for(problem)),
            ),
        }
    }

    /// Откуда взялись настройки (для журнала старта и тестов).
    pub(crate) const fn origin(&self) -> BrowserSettingsOrigin {
        self.origin
    }

    /// Право окна писать config: решено вердиктом о файле при чтении.
    pub(crate) fn save_policy(&self) -> ConfigSavePolicy {
        self.save_policy.clone()
    }

    /// Проверенный config для сборки production-конфигурации плеера.
    pub(crate) const fn app_config(&self) -> &AppConfig {
        &self.app_config
    }

    /// Окно буфера источника «байты от страницы»: сколько держать вперёд и позади.
    pub(crate) fn buffer_window(&self) -> BrowserSessionBufferWindow {
        self.app_config.browser_session.buffer_window()
    }

    /// Настройки файла `<video src>`: окно предзагрузки и повторы из `[network]`.
    ///
    /// # Errors
    /// Окно предзагрузки из config-а не собралось.
    pub(crate) fn file_source(&self) -> Result<FileSourceSettings, FileSourceSettingsError> {
        FileSourceSettings::from_network(&self.app_config.network)
    }

    /// Устройство вывода звука, выбранное пользователем в обычном плеере.
    pub(crate) fn audio_output_device_id(&self) -> &str {
        &self.app_config.audio.output_device
    }

    /// Собирает настройки из значений по умолчанию с указанным происхождением и правом записи.
    fn from_defaults(origin: BrowserSettingsOrigin, save_policy: ConfigSavePolicy) -> Self {
        Self {
            app_config: AppConfig::default(),
            origin,
            save_policy,
        }
    }
}

/// Причина «только на сеанс» для файла, который нельзя взять за основу записи.
fn session_only_reason_for(problem: SharedConfigProblem) -> ConfigSessionOnlyReason {
    match problem {
        SharedConfigProblem::Broken(broken) => {
            ConfigSessionOnlyReason::BrokenConfigLeftToRegularPlayer(broken)
        }
        SharedConfigProblem::NotRegularFile => ConfigSessionOnlyReason::ConfigPathIsNotFile,
        SharedConfigProblem::ReadFailed { error_kind } => {
            ConfigSessionOnlyReason::ConfigFileUnreadable { error_kind }
        }
        // Разбор упал не из-за содержимого: файл не трогаем, класс ошибки неизвестен.
        SharedConfigProblem::UnclassifiedParseFailure => {
            ConfigSessionOnlyReason::ConfigFileUnreadable {
                error_kind: io::ErrorKind::Other,
            }
        }
    }
}

#[cfg(test)]
mod tests;
