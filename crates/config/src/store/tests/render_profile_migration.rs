//! Миграция v23 → v24: `render.profile = "vulkan"` становится `"auto"`, `[render.opengles]` удаляется.
//!
//! Решение владельца (gles2-renderer, сессия 09, №36): default-документ v23 писал `vulkan`
//! явно, отличить сознательный выбор нельзя. Остальные поля и повторная загрузка не меняются.

use super::*;
use crate::RenderProfile;

/// v23-документ, как его писал приложение: явный `vulkan`, таблица `opengles` и
/// нестандартные соседние значения, которые миграция трогать не вправе.
const SCHEMA_V23_WITH_VULKAN_AND_OPENGLES: &str = r#"
schema_version = 23

[render]
profile = "vulkan"
tone_mapping = "auto"

[render.vulkan]
present_mode = "mailbox"
max_frame_latency = 3

[render.opengles]
enabled = true
simple_ui = false
"#;

/// Полный file boundary: `vulkan` -> `auto`, `[render.opengles]` убран, всё остальное
/// сохранено, сохранённый v24 не содержит следов opengles и идемпотентен при перезагрузке.
#[test]
fn schema_v23_vulkan_migrates_to_auto_and_drops_opengles_table() {
    let temp_dir = tempfile::tempdir().expect("temp dir created");
    let config_path = temp_dir.path().join("config.toml");
    fs::write(&config_path, SCHEMA_V23_WITH_VULKAN_AND_OPENGLES).expect("v23 fixture written");

    let migrated = load_from_path(&config_path).expect("schema v23 config migrates");
    assert_eq!(migrated.config.schema_version, CURRENT_SCHEMA_VERSION);
    assert_eq!(migrated.config.schema_version, 28);
    assert_eq!(migrated.config.render.profile, RenderProfile::Auto);

    // Всё остальное, что настроил пользователь, уцелело: сравниваем с эталоном, где
    // отличается ровно профиль.
    let mut expected = AppConfig::default();
    expected.render.tone_mapping = crate::ToneMappingMode::Auto;
    expected.render.vulkan.present_mode = crate::VulkanPresentMode::Mailbox;
    expected.render.vulkan.max_frame_latency = 3;
    assert_eq!(migrated.config, expected);

    save_validated_atomic_at(&config_path, &migrated.config).expect("migrated v24 config saves");
    let saved_text = fs::read_to_string(&config_path).expect("saved v24 TOML readable");
    assert!(saved_text.contains("schema_version = 28"));
    assert!(saved_text.contains("profile = \"auto\""));
    assert!(!saved_text.contains("opengles"));
    assert!(!saved_text.contains("profile = \"vulkan\""));

    let reloaded = load_from_path(&config_path).expect("saved v24 config reloads");
    assert_eq!(reloaded.config, migrated.config);
    let resaved_text = reloaded.config.to_pretty_toml().expect("re-serialize");
    assert_eq!(resaved_text, saved_text, "повторное сохранение стабильно");
}

/// Другие значения профиля миграция не трогает; отсутствие `[render]` даёт default `auto`.
#[test]
fn schema_v23_non_vulkan_profiles_are_kept() {
    let temp_dir = tempfile::tempdir().expect("temp dir created");
    for (file_name, legacy_text, expected_profile) in [
        (
            "auto.toml",
            "schema_version = 23\n\n[render]\nprofile = \"auto\"\n",
            RenderProfile::Auto,
        ),
        (
            "opengles.toml",
            "schema_version = 23\n\n[render]\nprofile = \"opengles\"\n\n[render.opengles]\nenabled = true\nsimple_ui = true\n",
            RenderProfile::OpenGles,
        ),
        (
            "no-render.toml",
            "schema_version = 23\n",
            RenderProfile::Auto,
        ),
    ] {
        let config_path = temp_dir.path().join(file_name);
        fs::write(&config_path, legacy_text).expect("v23 fixture written");

        let loaded = load_from_path(&config_path).expect("schema v23 config loads");

        assert_eq!(
            loaded.config.render.profile, expected_profile,
            "{file_name}"
        );
    }
}

/// Явный `vulkan`, сохранённый уже в v24 (сознательный выбор), миграция не трогает.
#[test]
fn schema_v24_vulkan_is_kept_as_explicit_user_choice() {
    let temp_dir = tempfile::tempdir().expect("temp dir created");
    let config_path = temp_dir.path().join("config.toml");
    fs::write(
        &config_path,
        "schema_version = 28\n\n[render]\nprofile = \"vulkan\"\n",
    )
    .expect("v24 fixture written");

    let loaded = load_from_path(&config_path).expect("schema v24 config loads");

    assert_eq!(loaded.config.render.profile, RenderProfile::Vulkan);
}

/// В схеме v24 таблицы `[render.opengles]` больше нет: strict parser её отклоняет.
#[test]
fn schema_v24_rejects_removed_opengles_table() {
    let temp_dir = tempfile::tempdir().expect("temp dir created");
    let config_path = temp_dir.path().join("config.toml");
    fs::write(
        &config_path,
        "schema_version = 28\n\n[render.opengles]\nenabled = false\nsimple_ui = true\n",
    )
    .expect("v24 fixture written");

    assert!(load_from_path(&config_path).is_err());
}

/// Новый конфиг по умолчанию использует `auto` и пишет его в файл.
#[test]
fn default_config_uses_auto_render_profile() {
    assert_eq!(AppConfig::default().render.profile, RenderProfile::Auto);
    let default_text = AppConfig::default()
        .to_pretty_toml()
        .expect("serialize defaults");
    assert!(default_text.contains("profile = \"auto\""));
}
