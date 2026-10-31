//! Настройки окна браузерного режима: те же, что у обычного окна, и общая доска config.
//!
//! Решение 135: с 04c окно браузерного режима пишет config — громкость, ширина сайдбара,
//! стиль субтитров, Apply в окне настроек, размер и фуллскрин окна. Запись идёт общим
//! путём под замком «только тронутые ключи» (`ConfigSaveTarget::update_shared_file`);
//! чужие ключи берутся с диска. Право записи решает чтение при старте
//! ([`BrowserSessionSettings::save_policy`]): нормальный файл или его отсутствие — пишем;
//! битый или новее схемы — только на сеанс (браузерный процесс файл не чинит).

use std::path::PathBuf;

use fastiplayer_config::{ConfigLoadOrigin, ConfigPaths, LoadedConfig};

use super::super::session_config::{BrowserSessionSettings, BrowserSettingsOrigin};

/// Config для `SettingsRuntime` окна: прочитанные значения + право записи по вердикту о файле.
pub(super) fn browser_window_loaded_config(settings: &BrowserSessionSettings) -> LoadedConfig {
    let path = ConfigPaths::discover()
        .map(|paths| paths.config_file)
        .unwrap_or_default();
    browser_window_loaded_config_at(settings, path)
}

/// Чистая сборка (для тестов — без поиска каталога пользователя).
fn browser_window_loaded_config_at(
    settings: &BrowserSessionSettings,
    path: PathBuf,
) -> LoadedConfig {
    let origin = match settings.origin() {
        BrowserSettingsOrigin::ConfigFile => ConfigLoadOrigin::LoadedExisting,
        BrowserSettingsOrigin::DefaultsWithoutFile
        | BrowserSettingsOrigin::DefaultsAfterBrokenConfig(_)
        | BrowserSettingsOrigin::DefaultsAfterNewerConfig { .. } => {
            ConfigLoadOrigin::DefaultsInMemory
        }
    };
    LoadedConfig {
        config: settings.app_config().clone(),
        path,
        origin,
        save_policy: settings.save_policy(),
    }
}

#[cfg(test)]
mod tests {
    use std::fs;

    use fastiplayer_config::{
        AppConfig, BrokenConfigProblem, ConfigSavePolicy, ConfigSessionOnlyReason,
    };

    use super::*;

    /// Загружает настройки из файла с данным содержимым и собирает config окна.
    fn loaded_from(contents: Option<&[u8]>) -> (LoadedConfig, tempfile::TempDir) {
        let directory = tempfile::tempdir().expect("temporary config directory");
        let path = directory.path().join("config.toml");
        if let Some(bytes) = contents {
            fs::write(&path, bytes).expect("write config");
        }
        let settings = BrowserSessionSettings::load_read_only_from(&path);
        (browser_window_loaded_config_at(&settings, path), directory)
    }

    /// Нормальный файл → окно пишет, значения из файла.
    #[test]
    fn a_readable_file_lets_the_window_write() {
        let mut config = AppConfig::default();
        config.audio.output_device = "usb".to_string();
        let text = config.to_pretty_toml().expect("serialize");
        let (loaded, _directory) = loaded_from(Some(text.as_bytes()));

        assert_eq!(loaded.save_policy, ConfigSavePolicy::WriteToFile);
        assert_eq!(loaded.origin, ConfigLoadOrigin::LoadedExisting);
        assert_eq!(loaded.config.audio.output_device, "usb");
        assert!(matches!(
            loaded.save_target().policy(),
            ConfigSavePolicy::WriteToFile
        ));
    }

    /// Файла нет → окно пишет (первая запись создаст файл), значения по умолчанию.
    #[test]
    fn a_missing_file_lets_the_window_write() {
        let (loaded, directory) = loaded_from(None);

        assert_eq!(loaded.save_policy, ConfigSavePolicy::WriteToFile);
        assert_eq!(loaded.config, AppConfig::default());
        assert!(
            !directory.path().join("config.toml").exists(),
            "reading must not create the file"
        );
    }

    /// Битый файл → только на сеанс с типизированной причиной; байты файла целы.
    #[test]
    fn a_broken_file_is_left_to_the_regular_player() {
        let broken = b"[audio\noutput_device = \"usb\"\n";
        let (loaded, directory) = loaded_from(Some(broken));

        assert_eq!(
            loaded.save_policy,
            ConfigSavePolicy::SessionOnly(
                ConfigSessionOnlyReason::BrokenConfigLeftToRegularPlayer(
                    BrokenConfigProblem::InvalidTomlSyntax
                )
            )
        );
        assert_eq!(loaded.origin, ConfigLoadOrigin::DefaultsInMemory);
        assert_eq!(
            fs::read(directory.path().join("config.toml")).expect("read back"),
            broken
        );
    }

    /// Файл новее схемы → только на сеанс с версией из файла.
    #[test]
    fn a_newer_file_keeps_the_session_read_only() {
        let (loaded, _directory) = loaded_from(Some(b"schema_version = 999\nunknown_key = 1\n"));

        assert_eq!(
            loaded.save_policy,
            ConfigSavePolicy::SessionOnly(ConfigSessionOnlyReason::NewerSchemaVersion {
                found: 999
            })
        );
    }

    /// Каталог вместо файла → только на сеанс («не файл»).
    #[test]
    fn a_directory_instead_of_the_file_keeps_the_session_read_only() {
        let directory = tempfile::tempdir().expect("temporary config directory");
        let path = directory.path().join("config.toml");
        fs::create_dir(&path).expect("directory in place of the file");
        let settings = BrowserSessionSettings::load_read_only_from(&path);

        let loaded = browser_window_loaded_config_at(&settings, path);

        assert_eq!(
            loaded.save_policy,
            ConfigSavePolicy::SessionOnly(ConfigSessionOnlyReason::ConfigPathIsNotFile)
        );
    }
}
