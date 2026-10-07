//! Validation настроек drag & drop плейлиста (`playlist.dropped_folder_*`).

use crate::{
    AppConfig, ConfigResult, MAX_DROPPED_FOLDER_MAX_DEPTH, MAX_DROPPED_FOLDER_MAX_FILES,
    MIN_DROPPED_FOLDER_MAX_DEPTH, MIN_DROPPED_FOLDER_MAX_FILES,
};

use super::validate_u64_range;

/// Проверяет лимиты обхода брошенной папки: число файлов и глубину вложенности.
///
/// Ошибка содержит полный путь поля (`playlist.dropped_folder_max_*`), порядок
/// проверок: сначала файлы, затем глубина.
pub(super) fn validate_dropped_folder_limits(config: &AppConfig) -> ConfigResult<()> {
    validate_u64_range(
        "playlist.dropped_folder_max_files",
        u64::from(config.playlist.dropped_folder_max_files),
        u64::from(MIN_DROPPED_FOLDER_MAX_FILES),
        u64::from(MAX_DROPPED_FOLDER_MAX_FILES),
    )?;
    validate_u64_range(
        "playlist.dropped_folder_max_depth",
        u64::from(config.playlist.dropped_folder_max_depth),
        u64::from(MIN_DROPPED_FOLDER_MAX_DEPTH),
        u64::from(MAX_DROPPED_FOLDER_MAX_DEPTH),
    )
}
