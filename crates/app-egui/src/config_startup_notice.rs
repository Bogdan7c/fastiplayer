//! Что сказать пользователю об итоге загрузки config-а (UX edge cases, сессия 05).
//!
//! Решение о файле настроек принимает config-хранилище (`fastiplayer_config::
//! load_or_recover_at`): переименовать повреждённый файл в копию, работать без записи и
//! т. п. Этот модуль только переводит готовый typed итог в:
//! - текст предупреждения для окна (по-английски, без путей к каталогам и имён Rust-типов);
//! - одну строку лога с техническими деталями.
//!
//! Ввод-вывод и решения здесь не выполняются: функции чистые и тестируются без диска.

use std::io;

use fastiplayer_config::{
    BrokenConfigProblem, BrokenConfigRecovery, ConfigLoadOrigin, ConfigLockFailure,
    ConfigSavePolicy, ConfigSessionOnlyReason, ConfigWriteProblem, LoadedConfig,
};
use tracing::{info, warn};

/// Текст временного уведомления, когда настройку применили, но в файл не записали.
pub(crate) const SETTINGS_KEPT_FOR_SESSION_ONLY_MESSAGE: &str = "Settings applied for this session only: the settings file cannot be modified during this session";

/// Хвост сообщений о работе без записи — одинаковый, чтобы пользователь узнавал смысл.
const CHANGES_WILL_NOT_BE_SAVED: &str =
    "Using default settings; changes made during this session will not be saved.";

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
            "{} Could not write a new settings file; changes made during this session will not be saved.",
            recovered_message(recovery)
        )),
        (_, Some(reason)) => Some(session_only_message(reason)),
        // Defaults в памяти всегда идут с запретом записи; без него это ошибка загрузчика,
        // но молчать о defaults всё равно нельзя.
        (ConfigLoadOrigin::DefaultsInMemory, None) => Some("Using default settings.".to_string()),
    }
}

/// Пишет итог загрузки config-а в лог (после инициализации tracing).
pub(crate) fn log_config_load_outcome(loaded_config: &LoadedConfig) {
    match (&loaded_config.origin, &loaded_config.save_policy) {
        (ConfigLoadOrigin::LoadedExisting, ConfigSavePolicy::WriteToFile) => {
            info!("Fastiplayer configuration loaded");
        }
        (ConfigLoadOrigin::CreatedDefault, ConfigSavePolicy::WriteToFile) => {
            info!("Fastiplayer configuration created with default settings");
        }
        (origin, save_policy) => {
            warn!(
                origin = ?origin,
                save_policy = ?save_policy,
                "Fastiplayer configuration could not be loaded normally; using default settings"
            );
        }
    }
}

/// «Файл был повреждён, сохранена копия …».
fn recovered_message(recovery: &BrokenConfigRecovery) -> String {
    format!(
        "The settings file was corrupted ({}). A backup was saved as {}; using default settings.",
        broken_problem_text(&recovery.problem),
        recovery.backup_file_name
    )
}

/// Короткое человеческое объяснение поломки.
fn broken_problem_text(problem: &BrokenConfigProblem) -> String {
    match problem {
        BrokenConfigProblem::InvalidTomlSyntax => "file syntax error".to_string(),
        BrokenConfigProblem::SchemaMismatch => "unknown or invalid setting".to_string(),
        BrokenConfigProblem::InvalidValue { field } => {
            format!("invalid value for setting {field}")
        }
    }
}

/// Сообщение для работы без записи; файл на диске в этих случаях не тронут.
fn session_only_message(reason: &ConfigSessionOnlyReason) -> String {
    let cause = match reason {
        ConfigSessionOnlyReason::NewerSchemaVersion { .. } => {
            "The settings file was created by a newer version of Fastiplayer and was left unchanged."
                .to_string()
        }
        ConfigSessionOnlyReason::ConfigFileUnreadable { error_kind } => format!(
            "Could not read the settings file ({}).",
            io_error_kind_text(*error_kind)
        ),
        ConfigSessionOnlyReason::ConfigPathIsNotFile => {
            "The settings file path points to a folder or another non-file object.".to_string()
        }
        ConfigSessionOnlyReason::BackupFailed { error_kind } => format!(
            "The settings file is corrupted, and a backup could not be saved ({}); the file was left unchanged.",
            io_error_kind_text(*error_kind)
        ),
        ConfigSessionOnlyReason::DefaultConfigNotWritten(problem) => format!(
            "Could not create the settings file ({}).",
            write_problem_text(*problem)
        ),
        ConfigSessionOnlyReason::BrowserSessionReadOnly => {
            "Browser mode does not save settings yet.".to_string()
        }
        // Замок записи config-а при старте не взят, а файла нет или он битый: без замка
        // каталог не трогаем (решение 135 моста, `fastiplayer-config::store::recovery`).
        ConfigSessionOnlyReason::ConfigLockUnavailable(ConfigLockFailure::Busy { .. }) => {
            "Another Fastiplayer window kept the settings file busy during startup; the file was left unchanged."
                .to_string()
        }
        ConfigSessionOnlyReason::ConfigLockUnavailable(ConfigLockFailure::Unavailable(_)) => {
            "The settings lock file next to config.toml cannot be used; the settings file was left unchanged."
                .to_string()
        }
    };
    format!("{cause} {CHANGES_WILL_NOT_BE_SAVED}")
}

/// Причина неудачной записи defaults простыми словами.
fn write_problem_text(problem: ConfigWriteProblem) -> String {
    match problem {
        ConfigWriteProblem::CreateDirectory(error_kind) => {
            format!(
                "settings folder could not be created: {}",
                io_error_kind_text(error_kind)
            )
        }
        ConfigWriteProblem::Write(failure) => failure.to_string(),
    }
}

/// Частые классы I/O ошибок по-английски; остальные — стандартный текст ОС.
fn io_error_kind_text(error_kind: io::ErrorKind) -> String {
    match error_kind {
        io::ErrorKind::PermissionDenied => "access denied".to_string(),
        io::ErrorKind::ReadOnlyFilesystem => "read-only disk".to_string(),
        io::ErrorKind::StorageFull => "disk full".to_string(),
        io::ErrorKind::NotADirectory => {
            "part of the path is a file instead of a folder".to_string()
        }
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
        .expect("Recovery Warning");

        assert_eq!(
            warning,
            "The settings file was corrupted (file syntax error). A backup was saved as config.toml.broken-2026-10-05_14-30-12.bak; using default settings."
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
        .expect("The new version is being warned");

        assert!(warning.contains("a newer version"));
        assert!(warning.contains("left unchanged"));
        assert!(warning.contains("changes made during this session will not be saved"));
    }

    /// Нечитаемый файл: причина по-английски, без имён Rust-типов.
    #[test]
    fn unreadable_warning_uses_human_reason() {
        let warning = config_startup_warning(&loaded(
            ConfigLoadOrigin::DefaultsInMemory,
            ConfigSavePolicy::SessionOnly(ConfigSessionOnlyReason::ConfigFileUnreadable {
                error_kind: io::ErrorKind::PermissionDenied,
            }),
        ))
        .expect("Unreadable File Warning");

        assert!(warning.contains("Could not read the settings file (access denied)"));
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
        .expect("warned");

        assert!(warning.contains("A backup was saved as config.toml.broken-x.bak"));
        assert!(warning.contains("ui.sidebar.width_points"));
        assert!(warning.contains("changes made during this session will not be saved"));
    }
}
