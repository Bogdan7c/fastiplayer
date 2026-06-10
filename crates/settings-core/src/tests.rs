use crate::{
    DefaultBehavior, NumericDescriptor, NumericRange, NumericStep, SettingAccess, SettingApplyMode,
    SettingDescriptor, SettingDescriptorText, SettingEditor, SettingId, SettingOption,
    SettingOptionCurrentValue, SettingOptionId, SettingOptions, SettingPlacement, SettingRouteId,
    SettingText, SettingValue, SettingValueError, SettingValueType, SettingsError,
    SettingsRegistry, TextDescriptor, TextFormat, VectorDescriptor,
};

#[derive(Debug, Clone)]
struct TestDocument {
    brightness: f64,
    schema_version: i64,
    title: String,
    rgb: Vec<f64>,
}

impl Default for TestDocument {
    fn default() -> Self {
        Self {
            brightness: 0.5,
            schema_version: 2,
            title: "Исходное имя".to_owned(),
            rgb: vec![1.0, 1.0, 1.0],
        }
    }
}

struct BrightnessAccessor;

impl crate::SettingAccessor<TestDocument> for BrightnessAccessor {
    fn get(&self, document: &TestDocument) -> crate::SettingsResult<SettingValue> {
        Ok(SettingValue::Float(document.brightness))
    }

    fn set(&self, document: &mut TestDocument, value: SettingValue) -> crate::SettingsResult<()> {
        let SettingValue::Float(brightness) = value else {
            return Err(SettingsError::access_failed("brightness expected float"));
        };
        document.brightness = brightness;
        Ok(())
    }

    fn reset(
        &self,
        document: &mut TestDocument,
        default_document: &TestDocument,
    ) -> crate::SettingsResult<()> {
        document.brightness = default_document.brightness;
        Ok(())
    }
}

struct SchemaVersionAccessor;

impl crate::SettingAccessor<TestDocument> for SchemaVersionAccessor {
    fn get(&self, document: &TestDocument) -> crate::SettingsResult<SettingValue> {
        Ok(SettingValue::Integer(document.schema_version))
    }

    fn set(&self, document: &mut TestDocument, value: SettingValue) -> crate::SettingsResult<()> {
        let SettingValue::Integer(schema_version) = value else {
            return Err(SettingsError::access_failed(
                "schema version expected integer",
            ));
        };
        document.schema_version = schema_version;
        Ok(())
    }

    fn reset(
        &self,
        document: &mut TestDocument,
        default_document: &TestDocument,
    ) -> crate::SettingsResult<()> {
        document.schema_version = default_document.schema_version;
        Ok(())
    }
}

struct TitleAccessor;

impl crate::SettingAccessor<TestDocument> for TitleAccessor {
    fn get(&self, document: &TestDocument) -> crate::SettingsResult<SettingValue> {
        Ok(SettingValue::Text(document.title.clone()))
    }

    fn set(&self, document: &mut TestDocument, value: SettingValue) -> crate::SettingsResult<()> {
        let SettingValue::Text(title) = value else {
            return Err(SettingsError::access_failed("title expected text"));
        };
        document.title = title;
        Ok(())
    }

    fn reset(
        &self,
        document: &mut TestDocument,
        default_document: &TestDocument,
    ) -> crate::SettingsResult<()> {
        document.title.clone_from(&default_document.title);
        Ok(())
    }
}

struct RgbAccessor;

impl crate::SettingAccessor<TestDocument> for RgbAccessor {
    fn get(&self, document: &TestDocument) -> crate::SettingsResult<SettingValue> {
        Ok(SettingValue::NumericVector(document.rgb.clone()))
    }

    fn set(&self, document: &mut TestDocument, value: SettingValue) -> crate::SettingsResult<()> {
        let SettingValue::NumericVector(rgb) = value else {
            return Err(SettingsError::access_failed("rgb expected numeric vector"));
        };
        document.rgb = rgb;
        Ok(())
    }

    fn reset(
        &self,
        document: &mut TestDocument,
        default_document: &TestDocument,
    ) -> crate::SettingsResult<()> {
        document.rgb.clone_from(&default_document.rgb);
        Ok(())
    }
}

#[test]
fn registry_rejects_duplicate_ids() {
    let mut registry = SettingsRegistry::<TestDocument>::empty();
    let descriptor = brightness_descriptor();

    registry
        .register(descriptor.clone(), BrightnessAccessor)
        .expect("first descriptor should register");

    let error = registry
        .register(descriptor, BrightnessAccessor)
        .expect_err("duplicate setting id must be rejected");

    assert_eq!(
        error,
        SettingsError::DuplicateSettingId {
            id: SettingId::from("render.brightness"),
        }
    );
}

#[test]
fn descriptor_text_ids_and_fallback_text_are_represented() {
    let descriptor = brightness_descriptor();

    assert_eq!(
        descriptor.text.label.text_id.as_str(),
        "settings.render.brightness.label"
    );
    assert_eq!(descriptor.text.label.fallback_ru, "Яркость");

    let description = descriptor
        .text
        .description
        .as_ref()
        .expect("description fallback must be present");
    assert_eq!(
        description.text_id.as_str(),
        "settings.render.brightness.description"
    );
    assert_eq!(description.fallback_ru, "Регулирует яркость изображения.");

    let help = descriptor
        .text
        .help
        .as_ref()
        .expect("help fallback must be present");
    assert_eq!(help.text_id.as_str(), "settings.render.brightness.help");
    assert_eq!(
        help.fallback_ru,
        "Значение 0.5 соответствует нейтральной яркости."
    );
}

#[test]
fn diff_only_reports_changed_fields() {
    let mut registry = SettingsRegistry::<TestDocument>::empty();
    registry
        .register(brightness_descriptor(), BrightnessAccessor)
        .expect("brightness descriptor should register");
    registry
        .register(title_descriptor(), TitleAccessor)
        .expect("title descriptor should register");
    registry
        .register(rgb_descriptor(), RgbAccessor)
        .expect("rgb descriptor should register");

    let baseline = TestDocument::default();
    let mut current = baseline.clone();
    current.title = "Новое имя".to_owned();

    let diff = registry
        .diff(&baseline, &current)
        .expect("diff should read values through accessors");

    assert_eq!(diff.len(), 1);
    assert_eq!(diff.changes()[0].id, SettingId::from("ui.title"));
    assert_eq!(
        diff.changes()[0].before,
        SettingValue::Text("Исходное имя".to_owned())
    );
    assert_eq!(
        diff.changes()[0].after,
        SettingValue::Text("Новое имя".to_owned())
    );
}

#[test]
fn read_only_setting_rejects_set() {
    let mut registry = SettingsRegistry::<TestDocument>::empty();
    registry
        .register(schema_descriptor(), SchemaVersionAccessor)
        .expect("schema descriptor should register");

    let mut document = TestDocument::default();
    let error = registry
        .set_value(
            &mut document,
            &SettingId::from("system.schema_version"),
            SettingValue::Integer(3),
        )
        .expect_err("read-only setting must reject set");

    assert_eq!(
        error,
        SettingsError::ReadOnlySetting {
            id: SettingId::from("system.schema_version"),
        }
    );
    assert_eq!(document.schema_version, 2);
}

#[test]
fn vector_length_validation_rejects_wrong_length() {
    let mut registry = SettingsRegistry::<TestDocument>::empty();
    registry
        .register(rgb_descriptor(), RgbAccessor)
        .expect("rgb descriptor should register");

    let mut document = TestDocument::default();
    let error = registry
        .set_value(
            &mut document,
            &SettingId::from("render.rgb"),
            SettingValue::NumericVector(vec![1.0, 0.5]),
        )
        .expect_err("wrong vector length must be rejected");

    assert_eq!(
        error,
        SettingsError::InvalidValue {
            id: SettingId::from("render.rgb"),
            reason: SettingValueError::InvalidVectorLength {
                expected: 3,
                actual: 2,
            },
        }
    );
    assert_eq!(document.rgb, vec![1.0, 1.0, 1.0]);
}

#[test]
fn dynamic_unavailable_current_value_is_explicit() {
    let options = SettingOptions::ready(
        "audio.output_device",
        vec![SettingOption::new(
            "default",
            SettingText::new("settings.audio.output.default", "Системное устройство"),
        )],
        SettingOptionCurrentValue::UnavailableCurrent {
            id: SettingOptionId::from("usb-dac-42"),
            label: SettingText::new(
                "settings.audio.output.unavailable_current",
                "USB DAC 42 (сейчас недоступно)",
            ),
        },
    );

    assert!(options.current.is_unavailable_current());
    let SettingOptionCurrentValue::UnavailableCurrent { id, label } = options.current else {
        panic!("current value should be explicitly unavailable");
    };
    assert_eq!(id.as_str(), "usb-dac-42");
    assert_eq!(label.fallback_ru, "USB DAC 42 (сейчас недоступно)");
}

fn brightness_descriptor() -> SettingDescriptor {
    SettingDescriptor {
        id: SettingId::from("render.brightness"),
        path: "render.brightness".into(),
        text: SettingDescriptorText::new(SettingText::new(
            "settings.render.brightness.label",
            "Яркость",
        ))
        .with_description(SettingText::new(
            "settings.render.brightness.description",
            "Регулирует яркость изображения.",
        ))
        .with_help(SettingText::new(
            "settings.render.brightness.help",
            "Значение 0.5 соответствует нейтральной яркости.",
        )),
        placement: SettingPlacement::new("render", "color", "main-settings-window"),
        value_type: SettingValueType::Float,
        editor: SettingEditor::Numeric(NumericDescriptor::new(
            NumericRange::Float { min: 0.0, max: 1.0 },
            NumericStep::Float(0.01),
            None,
        )),
        access: SettingAccess::ReadWrite,
        default_behavior: DefaultBehavior::FromDefaultDocument,
        route: SettingRouteId::from("render"),
        apply_mode: SettingApplyMode::ImmediatePreview,
    }
}

fn schema_descriptor() -> SettingDescriptor {
    SettingDescriptor {
        id: SettingId::from("system.schema_version"),
        path: "schema_version".into(),
        text: SettingDescriptorText::new(SettingText::new(
            "settings.system.schema_version.label",
            "Версия схемы",
        )),
        placement: SettingPlacement::new("system", "metadata", "main-settings-window"),
        value_type: SettingValueType::Integer,
        editor: SettingEditor::ReadOnly,
        access: SettingAccess::ReadOnly,
        default_behavior: DefaultBehavior::NoReset,
        route: SettingRouteId::from("system"),
        apply_mode: SettingApplyMode::CommittedApply,
    }
}

fn title_descriptor() -> SettingDescriptor {
    SettingDescriptor {
        id: SettingId::from("ui.title"),
        path: "ui.title".into(),
        text: SettingDescriptorText::new(SettingText::new(
            "settings.ui.title.label",
            "Название окна",
        )),
        placement: SettingPlacement::new("ui", "window", "main-settings-window"),
        value_type: SettingValueType::Text,
        editor: SettingEditor::Text(TextDescriptor::new(TextFormat::SingleLine)),
        access: SettingAccess::ReadWrite,
        default_behavior: DefaultBehavior::FromDefaultDocument,
        route: SettingRouteId::from("ui"),
        apply_mode: SettingApplyMode::CommittedApply,
    }
}

fn rgb_descriptor() -> SettingDescriptor {
    SettingDescriptor {
        id: SettingId::from("render.rgb"),
        path: "render.rgb".into(),
        text: SettingDescriptorText::new(SettingText::new("settings.render.rgb.label", "RGB")),
        placement: SettingPlacement::new("render", "color", "main-settings-window"),
        value_type: SettingValueType::NumericVector,
        editor: SettingEditor::Vector(VectorDescriptor::new(
            NumericDescriptor::new(
                NumericRange::Float { min: 0.0, max: 1.0 },
                NumericStep::Float(0.01),
                None,
            ),
            vec![
                SettingText::new("settings.render.rgb.red", "Красный"),
                SettingText::new("settings.render.rgb.green", "Зелёный"),
                SettingText::new("settings.render.rgb.blue", "Синий"),
            ],
            3,
        )),
        access: SettingAccess::ReadWrite,
        default_behavior: DefaultBehavior::FromDefaultDocument,
        route: SettingRouteId::from("render"),
        apply_mode: SettingApplyMode::ImmediatePreview,
    }
}
