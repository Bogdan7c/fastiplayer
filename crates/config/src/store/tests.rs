use std::path::Path;

use super::migrations::{REMOVED_FRAME_SERVER_HOVER_KEYS, REMOVED_HARDWARE_DECODE_ONLY_KEY};
use super::*;
use crate::{
    CURRENT_SCHEMA_VERSION, FrameServerConfig, FrameServerLiveScrubDecodeModeConfig,
    HdrToSdrOperatorConfig, LEGACY_SCHEMA_VERSION_2, LEGACY_SCHEMA_VERSION_3,
    MAX_PREFERRED_VIDEO_HEIGHT, PausedCommitBehavior, PreferredVideoHeight, ToneMappingMode,
    VideoBackendPreference, WebMediaHdrSelection, validation,
};

mod field_validation;
mod frame_server;
mod persistence_sections;
mod render_validation;
/// Проверяет, что atomic save не оставил временных файлов рядом с config.
fn assert_no_save_temp_files(config_directory: &Path) {
    let leftover_temp_file_count = fs::read_dir(config_directory)
        .expect("config directory readable")
        .filter_map(Result::ok)
        .filter(|entry| {
            entry
                .file_name()
                .to_string_lossy()
                .starts_with(".config.toml.")
        })
        .count();

    assert_eq!(leftover_temp_file_count, 0);
}

mod schema_migration;
mod ui_and_legacy;
