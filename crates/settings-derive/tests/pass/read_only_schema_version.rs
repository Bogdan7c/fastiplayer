#[derive(Clone, settings_derive::SettingsSchema)]
#[settings(require_all_fields)]
struct ReadOnlyDocument {
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
}

fn main() {
    let registry = <ReadOnlyDocument as settings_core::SettingsSchema>::settings_registry()
        .expect("read-only schema_version should compile");
    let descriptor = registry
        .descriptor(&settings_core::SettingId::from("schema_version"))
        .expect("schema_version descriptor should exist");
    assert_eq!(descriptor.access, settings_core::SettingAccess::ReadOnly);
}
