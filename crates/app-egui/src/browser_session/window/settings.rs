//! Настройки окна браузерного режима: те же, что у обычного окна, но «только на сеанс».
//!
//! Решение 122: настройки браузерного режима станут «общей доской» с обычным плеером в
//! 04c. До неё окно config НЕ пишет вовсе: громкость, ширина сайдбара, стиль субтитров,
//! Apply в окне настроек — всё действует до закрытия окна и на диск не попадает
//! (`ConfigSavePolicy::SessionOnly`, причина `BrowserSessionReadOnly`). Читается config
//! тем же безопасным путём, что и раньше (`BrowserSessionSettings::load_read_only`:
//! ни default-файла, ни `.bak`, ни миграции).

use std::path::PathBuf;

use fastiplayer_config::{
    ConfigLoadOrigin, ConfigPaths, ConfigSavePolicy, ConfigSessionOnlyReason, LoadedConfig,
};

use super::super::session_config::{BrowserSessionSettings, BrowserSettingsOrigin};

/// Config для `SettingsRuntime` окна: прочитанные значения + запрет записи.
pub(super) fn session_only_loaded_config(settings: &BrowserSessionSettings) -> LoadedConfig {
    // Путь нужен только сообщениям и сверке в `SettingsRuntime`; записи по нему не будет.
    let path = ConfigPaths::discover()
        .map(|paths| paths.config_file)
        .unwrap_or_default();
    session_only_loaded_config_at(settings, path)
}

/// Чистая сборка (для тестов — без поиска каталога пользователя).
fn session_only_loaded_config_at(settings: &BrowserSessionSettings, path: PathBuf) -> LoadedConfig {
    let origin = match settings.origin() {
        BrowserSettingsOrigin::ConfigFile => ConfigLoadOrigin::LoadedExisting,
        BrowserSettingsOrigin::DefaultsWithoutFile
        | BrowserSettingsOrigin::DefaultsAfterBrokenConfig(_) => ConfigLoadOrigin::DefaultsInMemory,
    };
    LoadedConfig {
        config: settings.app_config().clone(),
        path,
        origin,
        save_policy: ConfigSavePolicy::SessionOnly(ConfigSessionOnlyReason::BrowserSessionReadOnly),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_window_config_is_read_only_whatever_was_read() {
        let settings = BrowserSessionSettings::defaults();
        let loaded = session_only_loaded_config_at(&settings, PathBuf::from("config.toml"));
        assert_eq!(
            loaded.save_policy,
            ConfigSavePolicy::SessionOnly(ConfigSessionOnlyReason::BrowserSessionReadOnly)
        );
        assert_eq!(loaded.origin, ConfigLoadOrigin::DefaultsInMemory);
        assert_eq!(&loaded.config, settings.app_config());
        // Цель сохранения переносит запрет до места записи.
        assert!(matches!(
            loaded.save_target().policy(),
            ConfigSavePolicy::SessionOnly(_)
        ));
    }
}
