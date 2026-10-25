//! Живые настройки: preview / commit / rollback — те же исходы и отказы, что у wgpu, а отказ не
//! меняет состояние.

use render_core::{
    RenderLiveApplyOutcome, RenderLiveSettingsErrorKind, ShaderParameter, ShaderParameterId,
    ShaderParameterValue,
};

use super::*;

fn brighter(base: &RenderLiveSettings) -> RenderLiveSettings {
    let mut settings = base.clone();
    settings.color_pipeline.adjustment.brightness += 0.25;
    settings
}

/// Применение нового снимка меняет состояние и называет изменённые поля; повтор — «ничего».
#[test]
fn commit_applies_a_change_and_repeating_it_is_a_no_op() {
    let mut state = LiveSettingsState::default();
    let target = brighter(state.settings());

    let report = state.commit_live_settings(&target).unwrap();

    assert_eq!(report.phase, RenderLiveApplyPhase::Commit);
    assert_eq!(report.outcome, RenderLiveApplyOutcome::Applied);
    assert_eq!(
        report.changed_fields,
        vec![RenderLiveSettingId::ColorAdjustmentBrightness]
    );
    assert_eq!(state.settings(), &target, "the snapshot is in effect");

    let repeated = state.commit_live_settings(&target).unwrap();
    assert_eq!(repeated.outcome, RenderLiveApplyOutcome::NoOp);
    assert!(repeated.changed_fields.is_empty());
}

/// Preview применяет сразу, rollback возвращает исходный снимок: оба в своей фазе.
#[test]
fn preview_then_rollback_restores_the_baseline() {
    let mut state = LiveSettingsState::default();
    let baseline = state.settings().clone();
    let preview = brighter(&baseline);

    let previewed = state
        .preview_live_settings(&RenderLiveSettingsUpdate::from_baseline(
            &baseline,
            preview.clone(),
        ))
        .unwrap();
    assert_eq!(previewed.phase, RenderLiveApplyPhase::Preview);
    assert_eq!(state.settings(), &preview);

    let rolled_back = state.rollback_live_settings(&baseline).unwrap();
    assert_eq!(rolled_back.phase, RenderLiveApplyPhase::Rollback);
    assert_eq!(rolled_back.outcome, RenderLiveApplyOutcome::Applied);
    assert_eq!(state.settings(), &baseline);
}

/// Неподдержанное поле — `Unsupported` с перечнем полей, и ничего не применяется (даже
/// поддержанные поля того же снимка).
#[test]
fn unsupported_fields_are_refused_without_touching_the_state() {
    let mut state = LiveSettingsState::default();
    let before = state.settings().clone();
    let mut target = brighter(&before);
    target.color_pipeline.tone_mapping = ToneMappingMode::Reinhard;
    target.color_pipeline.swapchain_transfer = SwapchainTransferMode::SrgbRenderTarget;

    let error = state.commit_live_settings(&target).unwrap_err();

    assert_eq!(error.phase(), RenderLiveApplyPhase::Commit);
    let mut refused = error.setting_ids().to_vec();
    refused.sort();
    assert_eq!(
        refused,
        vec![
            RenderLiveSettingId::ColorPipelineToneMapping,
            RenderLiveSettingId::ColorPipelineSwapchainTransfer
        ],
        "only the unsupported fields are named, not the supported brightness"
    );
    assert_eq!(
        state.settings(),
        &before,
        "a refused snapshot changes nothing"
    );
}

/// Параметры пользовательских шейдеров рендер не умеет.
#[test]
fn shader_parameters_are_unsupported() {
    let mut state = LiveSettingsState::default();
    let before = state.settings().clone();
    let mut target = before.clone();
    target
        .shader_parameters
        .parameters
        .push(ShaderParameter::new(
            ShaderParameterId::new("grain"),
            ShaderParameterValue::Float(1.0),
        ));

    let error = state.commit_live_settings(&target).unwrap_err();

    assert_eq!(error.kind(), RenderLiveSettingsErrorKind::Unsupported);
    assert_eq!(
        error.setting_ids(),
        &[RenderLiveSettingId::ShaderParameter(
            ShaderParameterId::new("grain")
        )]
    );
    assert_eq!(state.settings(), &before);
}

/// HDR→SDR: принимается только рабочая связка BT.2446-C → SDR BT.709 с разумными уровнями;
/// выключение и уровни, ломающие формулу, — отказ по каждому изменённому полю HDR и без
/// изменения состояния.
#[test]
fn only_bt2446c_into_sdr_bt709_is_accepted_for_hdr_to_sdr() {
    let mut state = LiveSettingsState::default();
    let mut accepted = state.settings().clone();
    accepted.hdr_to_sdr.sdr_reference_white_nits = 203.0;
    let report = state.commit_live_settings(&accepted).unwrap();
    assert_eq!(
        report.changed_fields,
        vec![RenderLiveSettingId::HdrToSdrSdrReferenceWhiteNits]
    );

    let before = state.settings().clone();
    let mut refused = before.clone();
    refused.hdr_to_sdr.enabled = false;
    // Пик ниже опорного белого ломает формулу BT.2446-C.
    refused.hdr_to_sdr.hdr_reference_peak_nits = 50.0;
    let error = state.commit_live_settings(&refused).unwrap_err();
    assert_eq!(
        error.setting_ids(),
        &[
            RenderLiveSettingId::HdrToSdrEnabled,
            RenderLiveSettingId::HdrToSdrHdrReferencePeakNits
        ]
    );
    assert_eq!(state.settings(), &before);
}

/// Стартовые `set_*` не проходят проверку поддержки (прежнее стартовое поведение каталога) и
/// видны следующему снимку как действующие.
#[test]
fn startup_setters_bypass_the_support_check() {
    let mut state = LiveSettingsState::default();
    let pipeline = ColorPipelineSettings {
        tone_mapping: ToneMappingMode::Reinhard,
        ..ColorPipelineSettings::default()
    };
    state.set_color_pipeline(pipeline);
    assert_eq!(
        state.settings().color_pipeline.tone_mapping,
        ToneMappingMode::Reinhard
    );
    let hdr = HdrToSdrSettings {
        sdr_reference_white_nits: 150.0,
        ..HdrToSdrSettings::default()
    };
    state.set_hdr_to_sdr(hdr);
    assert_eq!(state.settings().hdr_to_sdr, hdr);

    // Возврат к умолчаниям — это уже обычная фиксация, и она поддержана.
    let report = state
        .commit_live_settings(&RenderLiveSettings::default())
        .unwrap();
    assert_eq!(report.outcome, RenderLiveApplyOutcome::Applied);
}
