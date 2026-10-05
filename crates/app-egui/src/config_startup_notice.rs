//! Что сказать пользователю об итоге загрузки config-а (UX edge cases, сессия 05).
//!
//! Решение о файле настроек принимает config-хранилище (`fastiplayer_config::
//! load_or_recover_at`): переименовать повреждённый файл в копию, работать без записи и
//! т. п. Этот модуль только переводит готовый typed итог в:
//! - текст предупреждения для окна (по-русски, без путей к каталогам и имён Rust-типов);
//! - одну строку лога с техническими деталями.
//!
//! Ввод-вывод и решения здесь не выполняются: функции чистые и тестируются без диска.

use std::io;

use fastiplayer_config::{
    BrokenConfigProblem, BrokenConfigRecovery, ConfigLoadOrigin, ConfigSavePolicy,
    ConfigSessionOnlyReason, ConfigWriteProblem, LoadedConfig,
};
use tracing::{info, warn};

/// Текст временного уведомления, когда настройку применили, но в файл не записали.
pub(crate) const SETTINGS_KEPT_FOR_SESSION_ONLY_MESSAGE: &str = "Настройки применены, но не сохранятся после выхода: файл настроек в этом запуске не изменяется";

/// Хвост сообщений о работе без записи — одинаковый, чтобы пользователь узнавал смысл.
const CHANGES_WILL_NOT_BE_SAVED: &str =
    "Используются настройки по умолчанию; изменения в этом запуске не сохранятся.";

/// Текст предупреждения для окна, если об итоге загрузки нужно сказать пользователю.
///
/// `None` — всё штатно (файл загружен или создан впервые), предупреждать не о чем.
pub(crate) fn config_startup_warning(loaded_config: &LoadedConfig) -> Option<String> {
    let session_only_reason = match &loaded_config.save_policy {
        ConfigSavePolicy::WriteToFile => None,
        ConfigSavePolicy::SessionOnly(reason) => Some(reason),
    };
    match (&loaded_config.origin, session_only_reason) {
        (ConfigLoadOrigin::LoadedExisting | ConfigLoadOrigin::CreatedDefault, None) => None,
        (ConfigLoadOrigin::RecoveredFromBroken(recovery), None) => {
            Some(recovered_message(recovery))
        }
        (ConfigLoadOrigin::RecoveredFromBroken(recovery), Some(_)) => Some(format!(
            "{} Новый файл настроек записать не удалось — изменения в этом запуске не сохранятся.",
            recovered_message(recovery)
        )),
        (_, Some(reason)) => Some(session_only_message(reason)),
        // Defaults в памяти всегда идут с запретом записи; без него это ошибка загрузчика,
        // но молчать о defaults всё равно нельзя.
        (ConfigLoadOrigin::DefaultsInMemory, None) => {
            Some("Используются настройки по умолчанию.".to_string())
        }
    }
}

/// Пишет итог загрузки config-а в лог (после инициализации tracing).
pub(crate) fn log_config_load_outcome(loaded_config: &LoadedConfig) {
    match (&loaded_config.origin, &loaded_config.save_policy) {
        (ConfigLoadOrigin::LoadedExisting, ConfigSavePolicy::WriteToFile) => {
            info!("Config fastiplayer загружен");
        }
        (ConfigLoadOrigin::CreatedDefault, ConfigSavePolicy::WriteToFile) => {
            info!("Config fastiplayer создан с настройками по умолчанию");
        }
        (origin, save_policy) => {
            warn!(
                origin = ?origin,
                save_policy = ?save_policy,
                "Config fastiplayer не загружен штатно; работаем на настройках по умолчанию"
            );
        }
    }
}

/// «Файл был повреждён, сохранена копия …».
fn recovered_message(recovery: &BrokenConfigRecovery) -> String {
    format!(
        "Файл настроек был повреждён ({}). Сохранена копия {}, используются настройки по умолчанию.",
        broken_problem_text(&recovery.problem),
        recovery.backup_file_name
    )
}

/// Короткое человеческое объяснение поломки.
fn broken_problem_text(problem: &BrokenConfigProblem) -> String {
    match problem {
        BrokenConfigProblem::InvalidTomlSyntax => "ошибка в тексте файла".to_string(),
        BrokenConfigProblem::SchemaMismatch => "неизвестный или неверный параметр".to_string(),
        BrokenConfigProblem::InvalidValue { field } => {
            format!("недопустимое значение параметра {field}")
        }
    }
}

/// Сообщение для работы без записи; файл на диске в этих случаях не тронут.
fn session_only_message(reason: &ConfigSessionOnlyReason) -> String {
    let cause = match reason {
        ConfigSessionOnlyReason::NewerSchemaVersion { .. } => {
            "Файл настроек создан более новой версией Fastiplayer и оставлен без изменений."
                .to_string()
        }
        ConfigSessionOnlyReason::ConfigFileUnreadable { error_kind } => format!(
            "Не удалось прочитать файл настроек ({}).",
            io_error_kind_text(*error_kind)
        ),
        ConfigSessionOnlyReason::ConfigPathIsNotFile => {
            "На месте файла настроек находится папка или другой объект, а не файл.".to_string()
        }
        ConfigSessionOnlyReason::BackupFailed { error_kind } => format!(
            "Файл настроек повреждён, но сохранить его копию не удалось ({}); файл оставлен без изменений.",
            io_error_kind_text(*error_kind)
        ),
        ConfigSessionOnlyReason::DefaultConfigNotWritten(problem) => format!(
            "Не удалось создать файл настроек ({}).",
            write_problem_text(*problem)
        ),
    };
    format!("{cause} {CHANGES_WILL_NOT_BE_SAVED}")
}

/// Причина неудачной записи defaults простыми словами.
fn write_problem_text(problem: ConfigWriteProblem) -> String {
    match problem {
        ConfigWriteProblem::CreateDirectory(error_kind) => {
            format!(
                "папка настроек не создана: {}",
                io_error_kind_text(error_kind)
            )
        }
        ConfigWriteProblem::Write(failure) => failure.to_string(),
    }
}

/// Частые классы I/O ошибок по-русски; остальные — стандартный текст ОС.
fn io_error_kind_text(error_kind: io::ErrorKind) -> String {
    match error_kind {
        io::ErrorKind::PermissionDenied => "нет доступа".to_string(),
        io::ErrorKind::ReadOnlyFilesystem => "диск только для чтения".to_string(),
        io::ErrorKind::StorageFull => "диск заполнен".to_string(),
        io::ErrorKind::NotADirectory => "часть пути — файл, а не папка".to_string(),
        other => io::Error::from(other).to_string(),
    }
}

#[cfg(test)]
mod tests {
    use std::path::PathBuf;

    use fastiplayer_config::AppConfig;

    use super::*;

    /// Итог загрузки с заданными origin и правом записи.
    fn loaded(origin: ConfigLoadOrigin, save_policy: ConfigSavePolicy) -> LoadedConfig {
        LoadedConfig {
            config: AppConfig::default(),
            path: PathBuf::from("/private/home/user/.config/fastiplayer/config.toml"),
            origin,
            save_policy,
        }
    }

    /// Штатная загрузка и первый запуск — без предупреждений.
    #[test]
    fn normal_outcomes_produce_no_warning() {
        for origin in [
            ConfigLoadOrigin::LoadedExisting,
            ConfigLoadOrigin::CreatedDefault,
        ] {
            assert_eq!(
                config_startup_warning(&loaded(origin, ConfigSavePolicy::WriteToFile)),
                None
            );
        }
    }

    /// Восстановление называет копию по имени и не раскрывает каталог.
    #[test]
    fn recovered_warning_names_backup_without_directory() {
        let warning = config_startup_warning(&loaded(
            ConfigLoadOrigin::RecoveredFromBroken(BrokenConfigRecovery {
                problem: BrokenConfigProblem::InvalidTomlSyntax,
                backup_file_name: "config.toml.broken-2026-10-05_14-30-12.bak".to_string(),
            }),
            ConfigSavePolicy::WriteToFile,
        ))
        .expect("о восстановлении предупреждаем");

        assert_eq!(
            warning,
            "Файл настроек был повреждён (ошибка в тексте файла). Сохранена копия \
             config.toml.broken-2026-10-05_14-30-12.bak, используются настройки по умолчанию."
        );
        assert!(!warning.contains("/private/home"));
    }

    /// Файл новой версии: явно сказано, что файл не тронут и изменения не сохранятся.
    #[test]
    fn newer_version_warning_explains_session_only_mode() {
        let warning = config_startup_warning(&loaded(
            ConfigLoadOrigin::DefaultsInMemory,
            ConfigSavePolicy::SessionOnly(ConfigSessionOnlyReason::NewerSchemaVersion {
                found: 99,
            }),
        ))
        .expect("о новой версии предупреждаем");

        assert!(warning.contains("более новой версией"));
        assert!(warning.contains("оставлен без изменений"));
        assert!(warning.contains("изменения в этом запуске не сохранятся"));
    }

    /// Нечитаемый файл: причина по-русски, без имён Rust-типов.
    #[test]
    fn unreadable_warning_uses_human_reason() {
        let warning = config_startup_warning(&loaded(
            ConfigLoadOrigin::DefaultsInMemory,
            ConfigSavePolicy::SessionOnly(ConfigSessionOnlyReason::ConfigFileUnreadable {
                error_kind: io::ErrorKind::PermissionDenied,
            }),
        ))
        .expect("о нечитаемом файле предупреждаем");

        assert!(warning.contains("Не удалось прочитать файл настроек (нет доступа)"));
        assert!(!warning.contains("PermissionDenied"));
    }

    /// Восстановили, но записать defaults не смогли: оба факта в одном сообщении.
    #[test]
    fn recovered_without_default_file_mentions_both_facts() {
        let warning = config_startup_warning(&loaded(
            ConfigLoadOrigin::RecoveredFromBroken(BrokenConfigRecovery {
                problem: BrokenConfigProblem::InvalidValue {
                    field: "ui.sidebar.width_points",
                },
                backup_file_name: "config.toml.broken-x.bak".to_string(),
            }),
            ConfigSavePolicy::SessionOnly(ConfigSessionOnlyReason::DefaultConfigNotWritten(
                ConfigWriteProblem::CreateDirectory(io::ErrorKind::PermissionDenied),
            )),
        ))
        .expect("предупреждаем");

        assert!(warning.contains("Сохранена копия config.toml.broken-x.bak"));
        assert!(warning.contains("ui.sidebar.width_points"));
        assert!(warning.contains("изменения в этом запуске не сохранятся"));
    }
}
