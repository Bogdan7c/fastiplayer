use settings_core::{
    SettingAccess, SettingApplyMode, SettingId, SettingValue, SettingsError, SettingsSchema,
};

#[derive(Debug, Clone, PartialEq, settings_derive::SettingsSchema)]
#[settings(require_all_fields)]
struct Document {
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
    render: RenderDocument,
}

impl Default for Document {
    fn default() -> Self {
        Self {
            schema_version: 2,
            render: RenderDocument::default(),
        }
    }
}

#[derive(Debug, Clone, PartialEq, settings_derive::SettingsSchema)]
#[settings(require_all_fields)]
struct RenderDocument {
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

impl Default for RenderDocument {
    fn default() -> Self {
        Self { brightness: 0.0 }
    }
}

#[test]
fn read_only_schema_version_is_registered_but_not_writable() {
    let registry = Document::settings_registry().expect("registry should be generated");
    let descriptor = registry
        .descriptor(&SettingId::from("schema_version"))
        .expect("schema_version descriptor should exist");

    assert_eq!(descriptor.access, SettingAccess::ReadOnly);
    assert_eq!(descriptor.apply_mode, SettingApplyMode::CommittedApply);

    let mut document = Document::default();
    let error = registry
        .set_value(
            &mut document,
            &SettingId::from("schema_version"),
            SettingValue::Integer(3),
        )
        .expect_err("read-only schema_version must reject writes");

    assert_eq!(
        error,
        SettingsError::ReadOnlySetting {
            id: SettingId::from("schema_version"),
        }
    );
}

#[test]
fn nested_registry_uses_parent_accessors_and_default_document_reset() {
    let registry = Document::settings_registry().expect("registry should be generated");
    let brightness_id = SettingId::from("render.brightness");
    let descriptor = registry
        .descriptor(&brightness_id)
        .expect("nested brightness descriptor should exist");

    assert_eq!(descriptor.apply_mode, SettingApplyMode::ImmediatePreview);
    assert_eq!(descriptor.route.as_str(), "render");

    let mut document = Document {
        schema_version: 2,
        render: RenderDocument { brightness: 0.75 },
    };
    let default_document = Document {
        schema_version: 2,
        render: RenderDocument { brightness: 0.33 },
    };

    assert_eq!(
        registry
            .get_value(&document, &brightness_id)
            .expect("brightness should be readable"),
        SettingValue::Float(0.75)
    );

    registry
        .set_value(&mut document, &brightness_id, SettingValue::Float(-0.25))
        .expect("brightness should be writable through nested accessor");
    assert_eq!(document.render.brightness, -0.25);

    registry
        .reset_value(&mut document, &default_document, &brightness_id)
        .expect("reset should copy from provided default document");
    assert_eq!(document.render.brightness, 0.33);
}
