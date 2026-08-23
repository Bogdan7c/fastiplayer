use crate::{
    AppConfig, CURRENT_SCHEMA_VERSION, LEGACY_SCHEMA_VERSION_2, LEGACY_SCHEMA_VERSION_3,
    LEGACY_SCHEMA_VERSION_4, LEGACY_SCHEMA_VERSION_5, LEGACY_SCHEMA_VERSION_6,
    LEGACY_SCHEMA_VERSION_7,
};

pub(super) const REMOVED_HARDWARE_DECODE_ONLY_KEY: &str = "hardware_decode_only";
/// Legacy placeholder никогда не управлял чтением cookies самим `yt-dlp`.
pub(super) const REMOVED_PREFER_ACCOUNT_SESSION_KEY: &str = "prefer_account_session";
pub(super) const REMOVED_FRAME_SERVER_HOVER_KEYS: &[&str] = &[
    "hover_preview_enabled",
    "hover_pool_frames",
    "hover_thread_count",
    "hover_prepare_window_slots",
    "software_hover_prepare_window_slots",
    "recent_superseded_prepare_slots",
    "software_recent_superseded_prepare_slots",
    "hover_leave_grace_ms",
    "network_hover_prepare_throttle_ms",
];

/// Нормализует только известные legacy-поля до strict Serde-разбора.
pub(super) fn normalize_document(toml_document: &mut toml::Value) {
    let toml::Value::Table(root_table) = toml_document else {
        return;
    };
    if schema_at_most(root_table, LEGACY_SCHEMA_VERSION_3) {
        remove_removed_hardware_decode_only(root_table);
    }
    if schema_at_most(root_table, LEGACY_SCHEMA_VERSION_4) {
        remove_removed_frame_server_hover_keys(root_table);
    }
    if schema_at_most(root_table, LEGACY_SCHEMA_VERSION_5) {
        migrate_legacy_youtube_section(root_table);
    }
}

/// Поднимает поддерживаемые v2-v7 структуры до текущей in-memory версии.
pub(super) fn upgrade_config(config: &mut AppConfig) {
    if matches!(
        config.schema_version,
        LEGACY_SCHEMA_VERSION_2
            | LEGACY_SCHEMA_VERSION_3
            | LEGACY_SCHEMA_VERSION_4
            | LEGACY_SCHEMA_VERSION_5
            | LEGACY_SCHEMA_VERSION_6
            | LEGACY_SCHEMA_VERSION_7
    ) {
        config.schema_version = CURRENT_SCHEMA_VERSION;
    }
}

/// Переименовывает legacy `[youtube]` в `[yt_dlp]` до strict Serde-разбора.
///
/// Если документ уже содержит обе секции, миграция ничего не объединяет:
/// старый ключ остаётся на месте и strict parser возвращает понятную ошибку,
/// вместо молчаливого выбора одного из конфликтующих значений.
fn migrate_legacy_youtube_section(root_table: &mut toml::Table) {
    if root_table.contains_key("yt_dlp") {
        return;
    }
    let Some(mut legacy_section) = root_table.remove("youtube") else {
        return;
    };
    if let toml::Value::Table(section_table) = &mut legacy_section {
        section_table.remove(REMOVED_PREFER_ACCOUNT_SESSION_KEY);
    }
    root_table.insert("yt_dlp".to_string(), legacy_section);
}

fn schema_at_most(root_table: &toml::Table, maximum: u32) -> bool {
    matches!(root_table.get("schema_version"), Some(toml::Value::Integer(version)) if *version >= 0 && *version <= i64::from(maximum))
}

fn remove_removed_hardware_decode_only(root_table: &mut toml::Table) {
    if let Some(toml::Value::Table(video_table)) = root_table.get_mut("video") {
        video_table.remove(REMOVED_HARDWARE_DECODE_ONLY_KEY);
    }
}

fn remove_removed_frame_server_hover_keys(root_table: &mut toml::Table) {
    let Some(toml::Value::Table(frame_server_table)) = root_table.get_mut("frame_server") else {
        return;
    };
    for removed_key in REMOVED_FRAME_SERVER_HOVER_KEYS {
        frame_server_table.remove(*removed_key);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Legacy normalization удаляет только перечисленные v3-поля и сохраняет strict unknowns.
    #[test]
    fn v3_document_removes_hardware_flag_but_keeps_unknown_key() {
        let mut document: toml::Value = toml::from_str(
            "schema_version = 3\n[video]\nhardware_decode_only = true\nunknown = 1\n",
        )
        .expect("legacy fixture");

        normalize_document(&mut document);

        let video = document
            .get("video")
            .and_then(toml::Value::as_table)
            .expect("video table");
        assert!(!video.contains_key("hardware_decode_only"));
        assert!(video.contains_key("unknown"));
    }

    /// Current schema не получает послаблений для удалённых legacy-ключей.
    #[test]
    fn v7_document_keeps_removed_keys_for_strict_parser_rejection() {
        let mut document: toml::Value =
            toml::from_str("schema_version = 7\n[frame_server]\nhover_preview_enabled = true\n")
                .expect("current fixture");

        normalize_document(&mut document);

        let frame_server = document
            .get("frame_server")
            .and_then(toml::Value::as_table)
            .expect("frame server table");
        assert!(frame_server.contains_key("hover_preview_enabled"));
    }

    /// Legacy секция мигрируется exact, а неработавший placeholder удаляется.
    #[test]
    fn v5_document_renames_youtube_section_and_removes_placeholder() {
        let mut document: toml::Value = toml::from_str(
            "schema_version = 5\n[youtube]\nenabled = false\nprefer_account_session = true\nresolve_timeout_ms = 1234\n",
        )
        .expect("legacy yt-dlp fixture");

        normalize_document(&mut document);

        assert!(document.get("youtube").is_none());
        let yt_dlp = document
            .get("yt_dlp")
            .and_then(toml::Value::as_table)
            .expect("migrated yt_dlp table");
        assert_eq!(
            yt_dlp.get("enabled").and_then(toml::Value::as_bool),
            Some(false)
        );
        assert_eq!(
            yt_dlp
                .get("resolve_timeout_ms")
                .and_then(toml::Value::as_integer),
            Some(1234)
        );
        assert!(!yt_dlp.contains_key(REMOVED_PREFER_ACCOUNT_SESSION_KEY));
    }
}
