use std::path::Path;

use super::migrations::{REMOVED_FRAME_SERVER_HOVER_KEYS, REMOVED_HARDWARE_DECODE_ONLY_KEY};
use super::*;
use crate::{
    CURRENT_SCHEMA_VERSION, FrameServerConfig, FrameServerLiveScrubDecodeModeConfig,
    LEGACY_SCHEMA_VERSION_2, LEGACY_SCHEMA_VERSION_3, MAX_PREFERRED_VIDEO_HEIGHT,
    PreferredVideoHeight, VideoBackendPreference, WebMediaHdrSelection, validation,
};

mod field_validation;
mod frame_server;
mod persistence_sections;
mod playlist_dropped_items;
mod playlist_error_behavior_migration;
mod recovery;
mod render_profile_migration;
mod render_validation;
mod startup_read_ahead_settings;
mod subtitles_ass_settings;
mod subtitles_blur_settings;
mod subtitles_glyph_cache_settings;
mod subtitles_pgs_settings;
mod subtitles_prerender_settings;
mod subtitles_settings;
mod subtitles_stream_settings;
mod surface_pool_automatic_migration;
mod track_memory_settings;
mod video_backend_dynamic;
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
