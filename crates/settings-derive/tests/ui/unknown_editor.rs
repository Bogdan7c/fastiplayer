#[derive(Clone, settings_derive::SettingsSchema)]
#[settings(require_all_fields)]
struct UnknownEditor {
    #[setting(
        id = "video.enabled",
        path = "video.enabled",
        section = "video",
        group = "decode",
        surface = "main",
        label_id = "settings.video.enabled.label",
        label_ru = "Видео включено",
        editor = "slider",
        apply = "video.apply"
    )]
    enabled: bool,
}

fn main() {}
