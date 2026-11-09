//! `config.toml` плеера внутри стенда.
use std::path::{Path, PathBuf};

use fastiplayer_config::{
    AppConfig, RenderProfile, VideoBackendPreference, VideoCodec, save_validated_atomic_at,
};

use super::error::EnvironmentError;
use super::options::PlayerConfigProfile;
use super::startup_marker;

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
        // Настроек нет: Auto задаёт умолчание; след старта Vulkan кладёт `write_config`.
        PlayerConfigProfile::Clean | PlayerConfigProfile::AutoWithVulkanStartupMarker => None,
        PlayerConfigProfile::Gles2Vaapi => {
            let mut config = AppConfig::default();
            // Рендер OpenGL ES (GLES2) — главная цель проекта (старое железо).
            config.render.profile = RenderProfile::OpenGles;
            // Аппаратное декодирование (VA-API), как у владельца.
            config.video.preferred_backend = VideoBackendPreference::Hardware;
            Some(config)
        }
        PlayerConfigProfile::H264FirstCodecOrder => {
            let mut config = AppConfig::default();
            // H.264 первым — потолок «ниже всех»; остальной порядок как по умолчанию.
            config.player.preferred_video_codec_order = vec![
                VideoCodec::H264,
                VideoCodec::Vp9,
                VideoCodec::Av1,
                VideoCodec::H265,
                VideoCodec::Vp8,
            ];
            Some(config)
        }
        PlayerConfigProfile::SmallFileWindow => {
            let mut config = AppConfig::default();
            // Минимумы проверки config (`network.*`: окно ≥ куска, первый кусок ≤ куска):
            // ~4 с ролика `progressive-long-h264-aac` (2 Мбит/с) впереди курсора, а не весь файл.
            config.network.read_ahead_mb = SMALL_WINDOW_READ_AHEAD_MB;
            config.network.prefetch_chunk_mb = SMALL_WINDOW_CHUNK_MB;
            config.network.prefetch_initial_chunk_kb = SMALL_WINDOW_INITIAL_CHUNK_KB;
            Some(config)
        }
        PlayerConfigProfile::ShortNetworkRetry => {
            let mut config = AppConfig::default();
            config.network.reconnect_wait_ms = SHORT_NETWORK_RETRY_MS;
            Some(config)
        }
    }
}

/// Бюджет повторов сети профиля `ShortNetworkRetry`, мс: пара попыток, и честный отказ.
const SHORT_NETWORK_RETRY_MS: u64 = 3_000;
/// Окно предзагрузки профиля `SmallFileWindow`, МиБ (минимум `network.read_ahead_mb`).
const SMALL_WINDOW_READ_AHEAD_MB: u64 = 1;
/// Кусок предзагрузки профиля `SmallFileWindow`, МиБ (минимум `network.prefetch_chunk_mb`).
const SMALL_WINDOW_CHUNK_MB: u64 = 1;
/// Первый кусок после открытия и перемотки, КиБ (как умолчание плеера: 64 КиБ).
const SMALL_WINDOW_INITIAL_CHUNK_KB: u64 = 64;

/// Записывает файл профиля в `<xdg_config>/fastiplayer/config.toml`. Возвращает путь, если писали.
pub(crate) fn write_config(
    xdg_config_home: &Path,
    profile: PlayerConfigProfile,
) -> Result<Option<PathBuf>, EnvironmentError> {
    let directory = xdg_config_home.join(APP_CONFIG_DIRECTORY);
    if profile == PlayerConfigProfile::AutoWithVulkanStartupMarker {
        startup_marker::plant_vulkan_unfinished_startup(&directory)?;
    }
    let Some(config) = config_for(profile) else {
        return Ok(None);
    };
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

    #[test]
    fn small_file_window_profile_passes_validation_and_shrinks_the_prefetch_window() {
        let dir = tempfile::tempdir().expect("temp dir");
        let path = write_config(dir.path(), PlayerConfigProfile::SmallFileWindow)
            .expect("the validated writer accepts the minimums")
            .expect("path");
        let loaded = load_from_path(&path).expect("production loader accepts the file");
        assert_eq!(loaded.config.network.read_ahead_mb, 1);
        assert_eq!(loaded.config.network.prefetch_chunk_mb, 1);
        assert_eq!(loaded.config.network.prefetch_initial_chunk_kb, 64);
        assert!(loaded.config.network.read_ahead_mb < AppConfig::default().network.read_ahead_mb);
    }

    #[test]
    fn h264_first_profile_writes_the_codec_order_the_player_reads() {
        let dir = tempfile::tempdir().expect("temp dir");
        let path = write_config(dir.path(), PlayerConfigProfile::H264FirstCodecOrder)
            .expect("write")
            .expect("path");
        let loaded = load_from_path(&path).expect("production loader accepts the file");
        assert_eq!(
            loaded.config.player.preferred_video_codec_order.first(),
            Some(&VideoCodec::H264)
        );
        // Рендер остаётся прежним: ограничение даёт только список кодеков.
        assert_eq!(loaded.config.render.profile, RenderProfile::Auto);
    }

    #[test]
    fn forecast_profile_keeps_auto_render_and_plants_the_marker() {
        let dir = tempfile::tempdir().expect("temp dir");
        let written = write_config(dir.path(), PlayerConfigProfile::AutoWithVulkanStartupMarker)
            .expect("write");
        // Настроек нет (Auto по умолчанию), а метка лежит рядом.
        assert_eq!(written, None);
        assert!(
            dir.path()
                .join("fastiplayer/renderer-startup.marker")
                .is_file()
        );
    }
}
