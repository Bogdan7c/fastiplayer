use super::*;
use crate::*;
#[test]
fn render_live_settings_default_keeps_renderer_defaults() {
    let settings = RenderLiveSettings::default();

    assert_eq!(settings.color_pipeline, ColorPipelineSettings::default());
    assert_eq!(settings.hdr_to_sdr, HdrToSdrSettings::default());
    assert!(settings.shader_parameters.is_empty());
    assert!(
        settings
            .changed_fields_from(&RenderLiveSettings::default())
            .is_empty()
    );
}

#[test]
fn render_live_settings_update_tracks_changed_fields() {
    let baseline = RenderLiveSettings::default();
    let shader_parameter_id = ShaderParameterId::new("render.shader.test_gain");
    let mut settings = baseline.clone();

    settings.color_pipeline.adjustment.brightness = 0.25;
    settings.color_pipeline.adjustment.rgb_gain = [1.0, 0.9, 0.8];
    settings.hdr_to_sdr.sdr_reference_white_nits = 120.0;
    settings
        .shader_parameters
        .parameters
        .push(ShaderParameter::new(
            shader_parameter_id.clone(),
            ShaderParameterValue::Float(0.5),
        ));

    let update = RenderLiveSettingsUpdate::from_baseline(&baseline, settings);

    assert_eq!(
        update.changed_fields,
        vec![
            RenderLiveSettingId::ColorAdjustmentBrightness,
            RenderLiveSettingId::ColorAdjustmentRgbGain,
            RenderLiveSettingId::HdrToSdrSdrReferenceWhiteNits,
            RenderLiveSettingId::ShaderParameter(shader_parameter_id),
        ]
    );
}

#[test]
fn live_settings_errors_keep_noop_unsupported_absent_and_fatal_distinct() {
    let no_op_report = RenderLiveApplyReport::no_op(RenderLiveApplyPhase::Preview);
    let unsupported_error = RenderLiveSettingsError::unsupported(
        RenderLiveApplyPhase::Preview,
        vec![RenderLiveSettingId::ShaderParameter(
            ShaderParameterId::new("render.shader.unknown"),
        )],
        "shader parameter is not supported by this backend",
    );
    let absent_resource_error = RenderLiveSettingsError::absent_resource(
        RenderLiveApplyPhase::Rollback,
        "renderer is not initialized",
    );
    let fatal_error = RenderLiveSettingsError::fatal(RenderLiveApplyPhase::Commit, "device lost");

    assert_eq!(no_op_report.outcome, RenderLiveApplyOutcome::NoOp);
    assert_eq!(
        unsupported_error.kind(),
        RenderLiveSettingsErrorKind::Unsupported
    );
    assert_eq!(
        absent_resource_error.kind(),
        RenderLiveSettingsErrorKind::AbsentResource
    );
    assert_eq!(fatal_error.kind(), RenderLiveSettingsErrorKind::Fatal);
    assert_ne!(unsupported_error.kind(), fatal_error.kind());
    assert_eq!(
        unsupported_error.setting_ids(),
        &[RenderLiveSettingId::ShaderParameter(
            ShaderParameterId::new("render.shader.unknown",)
        )]
    );
}
