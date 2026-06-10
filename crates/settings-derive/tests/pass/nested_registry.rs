#[derive(Clone, settings_derive::SettingsSchema)]
#[settings(require_all_fields)]
struct AppConfig {
    #[setting(
        id = "schema_version",
        path = "schema_version",
        section = "system",
        group = "schema",
        surface = "main",
        label_id = "settings.schema_version.label",
        label_ru = "Версия схемы",
        editor = "read_only",
        apply = "system.apply",
        read_only,
        default = "no_reset"
    )]
    schema_version: u32,

    #[setting(nested)]
    render: RenderConfig,
}

#[derive(Clone, settings_derive::SettingsSchema)]
#[settings(require_all_fields)]
struct RenderConfig {
    #[setting(
        id = "render.brightness",
        path = "render.brightness",
        section = "render",
        group = "color",
        surface = "main",
        label_id = "settings.render.brightness.label",
        label_ru = "Яркость",
        editor = "float",
        min = -1.0,
        max = 1.0,
        step = 0.05,
        apply = "render.preview"
    )]
    brightness: f32,
}

fn main() {
    let registry = <AppConfig as settings_core::SettingsSchema>::settings_registry()
        .expect("nested registry should compile");
    assert!(registry
        .descriptor(&settings_core::SettingId::from("render.brightness"))
        .is_some());
}
