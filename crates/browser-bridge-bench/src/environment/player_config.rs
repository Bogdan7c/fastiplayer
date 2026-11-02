//! `config.toml` плеера внутри стенда.
use std::path::{Path, PathBuf};

use fastiplayer_config::{
    AppConfig, RenderProfile, VideoBackendPreference, save_validated_atomic_at,
};

use super::error::EnvironmentError;
use super::options::PlayerConfigProfile;

/// Имя каталога приложения внутри `XDG_CONFIG_HOME` (константа слоя config плеера).
const APP_CONFIG_DIRECTORY: &str = "fastiplayer";

/// Настройки профиля: `None` — файла нет, плеер берёт умолчания.
///
/// Профиль строится боевым путём: полный `AppConfig::default()` (единственный владелец
/// актуальной схемы и её версии) плюс только те поля, которыми профиль отличается.
/// Так при смене схемы стенд не надо править, а файл пишет тот же валидирующий
/// атомарный писатель, что и сам плеер.
///
/// `Clean` не пишет ничего: это и есть «чистая установка», первый запуск пользователя
/// без `config.toml`; плеер сам создаёт настройки по умолчанию.
pub(crate) fn config_for(profile: PlayerConfigProfile) -> Option<AppConfig> {
    match profile {
        PlayerConfigProfile::Clean => None,
        PlayerConfigProfile::Gles2Vaapi => {
            let mut config = AppConfig::default();
            // Рендер OpenGL ES (GLES2) — главная цель проекта (старое железо).
            config.render.profile = RenderProfile::OpenGles;
            // Аппаратное декодирование (VA-API), как у владельца.
            config.video.preferred_backend = VideoBackendPreference::Hardware;
            Some(config)
        }
    }
}

/// Записывает файл профиля в `<xdg_config>/fastiplayer/config.toml`. Возвращает путь, если писали.
pub(crate) fn write_config(
    xdg_config_home: &Path,
    profile: PlayerConfigProfile,
) -> Result<Option<PathBuf>, EnvironmentError> {
    let Some(config) = config_for(profile) else {
        return Ok(None);
    };
    let directory = xdg_config_home.join(APP_CONFIG_DIRECTORY);
    std::fs::create_dir_all(&directory).map_err(|source| EnvironmentError::Io {
        action: "create player config directory",
        path: directory.clone(),
        source,
    })?;
    let path = directory.join("config.toml");
    save_validated_atomic_at(&path, &config).map_err(|error| EnvironmentError::PlayerConfig {
        path: path.clone(),
        message: error.to_string(),
    })?;
    Ok(Some(path))
}

#[cfg(test)]
mod tests {
    use super::*;
    use fastiplayer_config::{CURRENT_SCHEMA_VERSION, load_from_path};

    #[test]
    fn clean_profile_writes_nothing() {
        let dir = tempfile::tempdir().expect("temp dir");
        let written = write_config(dir.path(), PlayerConfigProfile::Clean).expect("write");
        assert_eq!(written, None);
        assert!(!dir.path().join("fastiplayer").exists());
    }

    #[test]
    fn gles2_profile_loads_with_current_schema_and_overrides() {
        let dir = tempfile::tempdir().expect("temp dir");
        let path = write_config(dir.path(), PlayerConfigProfile::Gles2Vaapi)
            .expect("write")
            .expect("path");
        assert_eq!(path, dir.path().join("fastiplayer/config.toml"));
        // Боевой загрузчик плеера принимает файл без миграций и видит переопределения.
        let loaded = load_from_path(&path).expect("production loader accepts the file");
        assert_eq!(loaded.config.schema_version, CURRENT_SCHEMA_VERSION);
        assert_eq!(loaded.config.render.profile, RenderProfile::OpenGles);
        assert_eq!(
            loaded.config.video.preferred_backend,
            VideoBackendPreference::Hardware
        );
    }
}
