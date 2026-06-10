#[derive(Clone, settings_derive::SettingsSchema)]
#[settings(require_all_fields)]
struct MissingMetadata {
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

    missing_metadata: bool,
}

fn main() {}
