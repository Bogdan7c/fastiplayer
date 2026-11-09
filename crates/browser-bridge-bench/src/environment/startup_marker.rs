//! Метка «запуск рендера не дошёл до первого кадра», подложенная плееру в config-каталог.
//!
//! Зачем: обычный плеер ставит эту метку перед созданием рендера и снимает после первого
//! кадра; если процесс упал, метка остаётся, и следующий запуск (включая окно браузерного
//! режима) не повторяет упавший рендер. Сценарий `codecs` подкладывает такую метку, чтобы
//! проверить прогноз «Auto + Vulkan упал => GLES2 => метка кодеков {h264}» целиком.
//!
//! Формат зеркалит `app-egui::app_shell::renderer_startup_marker` (стенд не линкует крейты
//! плеера, это запрещено правилами зависимостей): файл содержит `stable_id` семейства рендера,
//! имя файла даёт `ConfigPaths::renderer_startup_marker_file` (крейт config уже зависимость).

use std::path::Path;

use fastiplayer_config::ConfigPaths;

use super::error::EnvironmentError;

/// `RendererFamily::WgpuVulkan.stable_id()` из `render-shell-api`; сверяется тестом ниже.
const WGPU_VULKAN_STABLE_ID: &str = "wgpu-vulkan";

/// Кладёт в `config_directory` метку «старт Vulkan не завершён».
pub(super) fn plant_vulkan_unfinished_startup(
    config_directory: &Path,
) -> Result<(), EnvironmentError> {
    std::fs::create_dir_all(config_directory).map_err(|source| EnvironmentError::Io {
        action: "create player config directory",
        path: config_directory.to_path_buf(),
        source,
    })?;
    let marker_path = ConfigPaths::from_config_dir(config_directory).renderer_startup_marker_file();
    std::fs::write(&marker_path, WGPU_VULKAN_STABLE_ID).map_err(|source| EnvironmentError::Io {
        action: "write renderer startup marker",
        path: marker_path,
        source,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn marker_is_written_where_the_player_reads_it_with_the_vulkan_id() {
        let dir = tempfile::tempdir().expect("temp dir");
        let config_directory = dir.path().join("fastiplayer");
        plant_vulkan_unfinished_startup(&config_directory).expect("plant");
        let marker = config_directory.join("renderer-startup.marker");
        assert_eq!(
            std::fs::read_to_string(marker).expect("marker exists"),
            "wgpu-vulkan"
        );
    }

    /// Зеркало не должно разойтись с игроком: сверяем литерал с источником правды.
    #[test]
    fn mirrored_stable_id_matches_the_player_catalog_source() {
        let catalog = std::fs::read_to_string(concat!(
            env!("CARGO_MANIFEST_DIR"),
            "/../render-shell-api/src/catalog.rs"
        ))
        .expect("catalog source");
        assert!(
            catalog.contains(&format!("Self::WgpuVulkan => \"{WGPU_VULKAN_STABLE_ID}\"")),
            "the player's stable id for Vulkan changed; update the bench mirror"
        );
    }
}
