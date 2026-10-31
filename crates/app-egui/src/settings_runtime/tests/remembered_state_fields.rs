//! Окно настроек не показывает «запоминаемое состояние», которое программа пишет сама
//! (поверхность `REMEMBERED_STATE_SURFACE_ID`, решение 137), но показывает обычные
//! настройки браузерного режима и осознанно видимые громкость и ширину сайдбара.

use std::collections::BTreeSet;

use super::*;

/// Id полей, которые модель окна настроек отдаёт отрисовке.
fn visible_setting_ids() -> BTreeSet<String> {
    let mut runtime = SettingsRuntime::from_loaded_config(loaded_config_for_test_at(
        AppConfig::default(),
        temp_config_path("remembered-state-fields"),
    ))
    .expect("settings runtime should build");
    runtime.open_settings().expect("settings should open");
    runtime
        .ui_model()
        .fields
        .iter()
        .map(|field| field.descriptor.id.as_str().to_owned())
        .collect()
}

#[test]
fn remembered_browser_window_keys_are_hidden_but_buffer_keys_are_shown() {
    let visible = visible_setting_ids();

    for hidden in [
        "browser_session.window.inner_width_points",
        "browser_session.window.inner_height_points",
        "browser_session.window.fullscreen",
    ] {
        assert!(!visible.contains(hidden), "{hidden} must not be shown");
    }
    for shown in [
        "browser_session.buffer_ahead_s",
        "browser_session.buffer_behind_s",
        // Видны осознанно (UX11 и сайдбар): их не скрываем.
        "audio.volume",
        "ui.sidebar.width_points",
    ] {
        assert!(visible.contains(shown), "{shown} must be shown");
    }
}
