//! Предупреждение «Hardware без VA-API» (UX edge cases 2, B6): от capability report-а
//! до строки, которую реально рисует окно настроек.

use capability_core::{
    BackendCapabilities, BackendDriverInfo, BackendProbeStatus, SupportedVideoOutput,
    SystemCapabilities,
};
use codec_core::{
    BitDepth, ChromaSubsampling, DecodeBackendId, SupportedVideoDecodeFormat, VideoCodec,
    VideoProfile, Vp9Profile,
};
use render_core::RenderCapabilities;
use video_frame_contract::{DmaBufImageLayout, VideoFrameContract};

use super::*;
use crate::settings_runtime::HardwareDecodeAvailability;
use crate::settings_runtime::hardware_decode_hint::HARDWARE_UNAVAILABLE_WARNING;

const BACKEND_FIELD_ID: &str = "video.preferred_backend";

fn vaapi_output() -> SupportedVideoOutput {
    SupportedVideoOutput {
        backend: DecodeBackendId::vaapi(),
        decode_format: SupportedVideoDecodeFormat {
            codec: VideoCodec::Vp9,
            profile: VideoProfile::Vp9(Vp9Profile::Profile0),
            bit_depth: BitDepth::Eight,
            chroma: ChromaSubsampling::Yuv420,
            max_width: Some(1920),
            max_height: Some(1080),
            max_fps: None,
            hdr_input: false,
        },
        frame_contract: VideoFrameContract::dma_buf_nv12(DmaBufImageLayout::ComposedLayers),
    }
}

/// Отчёт, в котором VA-API исправен и годен для показа.
fn capabilities_with_working_vaapi() -> SystemCapabilities {
    let output = vaapi_output();
    let mut capabilities = SystemCapabilities::empty(1);
    capabilities.video_backends.push(BackendCapabilities {
        backend_id: DecodeBackendId::vaapi(),
        display_name: "Test VA-API".to_owned(),
        status: BackendProbeStatus::Available,
        driver: BackendDriverInfo::default(),
        raw_supported_outputs: vec![output.clone()],
        raw_profiles: Vec::new(),
        raw_entrypoints: Vec::new(),
        raw_rt_formats: Vec::new(),
        quirks: Vec::new(),
        diagnostics: Vec::new(),
    });
    capabilities
        .render_backends
        .push(RenderCapabilities::wgpu_nv12(Some(4096)));
    capabilities.playable_video_outputs.push(output);
    capabilities
}

/// Отчёт, в котором probe VA-API провалился (нет драйвера).
fn capabilities_with_failed_vaapi() -> SystemCapabilities {
    let mut capabilities = SystemCapabilities::empty(1);
    capabilities
        .video_backends
        .push(BackendCapabilities::unavailable(
            DecodeBackendId::vaapi(),
            "VA-API",
            "драйвер не найден",
        ));
    capabilities
}

/// Runtime с открытым окном настроек и заданным режимом в config-е.
fn open_runtime(preference: VideoBackendPreference) -> SettingsRuntime {
    let mut config = custom_config_for_test();
    config.video.preferred_backend = preference;
    let mut runtime = SettingsRuntime::from_loaded_config(loaded_config_for_test(config.clone()))
        .expect("settings runtime должен построиться");
    let mut render_adapter =
        RecordingRenderAdapter::from_config(&config).expect("adapter должен стартовать");
    runtime
        .handle_ui_actions(vec![SettingsUiAction::Open], &mut render_adapter)
        .expect("окно настроек должно открыться");
    runtime.wait_for_options_refresh_for_test();
    runtime
}

fn backend_field(runtime: &mut SettingsRuntime) -> crate::settings_ui::SettingsUiField {
    runtime
        .ui_model()
        .fields
        .iter()
        .find(|field| field.descriptor.id == SettingId::from(BACKEND_FIELD_ID))
        .cloned()
        .expect("video.preferred_backend должен быть в visual model")
}

/// Тексты, которые окно настроек реально нарисует для поля.
fn painted_texts_of(field: &crate::settings_ui::SettingsUiField) -> Vec<String> {
    let context = egui::Context::default();
    let mut actions = Vec::new();
    let output = crate::ui::test_frame::run_ui_frame(
        &context,
        crate::ui::test_frame::input_at(0.0, Vec::new()),
        |ui| {
            crate::settings_ui::field_widget::show(ui, field, &mut actions);
        },
    );
    output
        .shapes
        .iter()
        .filter_map(|clipped| match &clipped.shape {
            egui::Shape::Text(text) => Some(text.galley.text().to_owned()),
            _ => None,
        })
        .collect()
}

#[test]
fn availability_is_derived_from_capability_report() {
    assert_eq!(
        HardwareDecodeAvailability::from_system_capabilities(&capabilities_with_working_vaapi()),
        HardwareDecodeAvailability::Available
    );
    assert_eq!(
        HardwareDecodeAvailability::from_system_capabilities(&capabilities_with_failed_vaapi()),
        HardwareDecodeAvailability::Unavailable
    );
    // VA-API «исправен», но renderer не принимает ни один его формат: видео не откроется.
    let mut no_playable = capabilities_with_working_vaapi();
    no_playable.playable_video_outputs.clear();
    assert_eq!(
        HardwareDecodeAvailability::from_system_capabilities(&no_playable),
        HardwareDecodeAvailability::Unavailable
    );
    // VA-API в отчёте не упомянут: утверждать нечего.
    assert_eq!(
        HardwareDecodeAvailability::from_system_capabilities(&SystemCapabilities::empty(1)),
        HardwareDecodeAvailability::Unknown
    );
}

#[test]
fn hardware_selected_and_vaapi_unavailable_shows_warning_in_window() {
    let mut runtime = open_runtime(VideoBackendPreference::Hardware);
    runtime.set_hardware_decode_availability(HardwareDecodeAvailability::from_system_capabilities(
        &capabilities_with_failed_vaapi(),
    ));

    let field = backend_field(&mut runtime);

    assert_eq!(field.status.as_deref(), Some(HARDWARE_UNAVAILABLE_WARNING));
    assert!(
        painted_texts_of(&field)
            .iter()
            .any(|text| text == HARDWARE_UNAVAILABLE_WARNING),
        "строка предупреждения должна быть нарисована в окне"
    );
}

#[test]
fn hardware_selected_and_vaapi_available_has_no_warning() {
    let mut runtime = open_runtime(VideoBackendPreference::Hardware);
    runtime.set_hardware_decode_availability(HardwareDecodeAvailability::from_system_capabilities(
        &capabilities_with_working_vaapi(),
    ));

    let field = backend_field(&mut runtime);

    assert_eq!(field.status, None);
    assert!(
        !painted_texts_of(&field)
            .iter()
            .any(|text| text == HARDWARE_UNAVAILABLE_WARNING)
    );
}

#[test]
fn auto_selected_and_vaapi_unavailable_has_no_warning() {
    let mut runtime = open_runtime(VideoBackendPreference::Auto);
    runtime.set_hardware_decode_availability(HardwareDecodeAvailability::Unavailable);

    assert_eq!(backend_field(&mut runtime).status, None);
}

#[test]
fn probe_not_ready_yet_has_no_warning() {
    // Availability не выставлялась: это состояние окна до первого capability report-а.
    let mut runtime = open_runtime(VideoBackendPreference::Hardware);

    assert_eq!(backend_field(&mut runtime).status, None);
}

#[test]
fn warning_follows_the_draft_choice_and_late_probe_result() {
    let mut runtime = open_runtime(VideoBackendPreference::Auto);
    let mut render_adapter = RecordingRenderAdapter::from_config(&custom_config_for_test())
        .expect("adapter должен стартовать");
    // Результат probe пришёл уже при открытом окне: модель обязана пересобраться.
    assert_eq!(backend_field(&mut runtime).status, None);
    runtime.set_hardware_decode_availability(HardwareDecodeAvailability::Unavailable);
    assert_eq!(backend_field(&mut runtime).status, None, "Auto: нет строки");

    runtime
        .handle_ui_actions(
            vec![SettingsUiAction::SetValue {
                setting_id: SettingId::from(BACKEND_FIELD_ID),
                value: SettingValue::Select(SettingOptionId::from("hardware")),
            }],
            &mut render_adapter,
        )
        .expect("смена режима в черновике должна пройти");
    assert_eq!(
        backend_field(&mut runtime).status.as_deref(),
        Some(HARDWARE_UNAVAILABLE_WARNING),
        "выбрали Hardware в черновике: строка появилась без Apply"
    );

    runtime
        .handle_ui_actions(
            vec![SettingsUiAction::SetValue {
                setting_id: SettingId::from(BACKEND_FIELD_ID),
                value: SettingValue::Select(SettingOptionId::from("auto")),
            }],
            &mut render_adapter,
        )
        .expect("возврат на Авто должен пройти");
    assert_eq!(backend_field(&mut runtime).status, None);
}
