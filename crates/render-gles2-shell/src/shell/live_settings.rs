//! Живые настройки цвета GLES2-рендера: preview / commit / rollback без пересборки программ.
//!
//! Семантика та же, что у wgpu (`WgpuVideoRenderer::apply_live_settings_snapshot`), чтобы
//! приложение не знало, какой рендер под ним: тот же снимок, те же отказы, те же исходы.
//! Настройки — данные рисования: [`VideoDrawPlan`](crate::VideoDrawPlan) читает их на каждый
//! кадр и превращает в uniform-ы цвета, ни одна программа видеослоя не пересобирается.
//!
//! Что рендер не умеет (отказ `Unsupported` с перечнем полей, состояние не меняется):
//! параметры пользовательских шейдеров, тон-маппинг кроме `Off`, перенос в swapchain кроме
//! `PreserveCurrentUnorm`, HDR→SDR кроме BT.2446-C в SDR BT.709.
//!
//! Стартовые настройки (`set_*`) НЕ проходят проверку «поддерживается ли поле»: это прежнее
//! стартовое поведение фабрики каталога (`commit` отклонял бы неподдержанные поля и менял
//! старт).

use render_core::{
    ColorPipelineSettings, HdrToSdrSettings, RenderLiveApplyPhase, RenderLiveApplyReport,
    RenderLiveSettingId, RenderLiveSettings, RenderLiveSettingsAdapter, RenderLiveSettingsError,
    RenderLiveSettingsUpdate, SwapchainTransferMode, ToneMappingMode,
};

/// Действующие живые настройки рендера и их применение.
#[derive(Debug, Default)]
pub(super) struct LiveSettingsState {
    settings: RenderLiveSettings,
}

impl LiveSettingsState {
    /// Действующий снимок (его читает построение плана видео на каждый кадр).
    pub(super) const fn settings(&self) -> &RenderLiveSettings {
        &self.settings
    }

    /// Стартовые настройки цвета (без проверки «поддерживается ли поле»).
    pub(super) fn set_color_pipeline(&mut self, settings: ColorPipelineSettings) {
        self.settings.color_pipeline = settings;
    }

    /// Стартовые настройки HDR→SDR (без проверки «поддерживается ли поле»).
    pub(super) fn set_hdr_to_sdr(&mut self, settings: HdrToSdrSettings) {
        self.settings.hdr_to_sdr = settings;
    }

    /// Применяет снимок: «ничего не изменилось», «применено» или отказ без изменения состояния.
    fn apply_snapshot(
        &mut self,
        phase: RenderLiveApplyPhase,
        settings: &RenderLiveSettings,
    ) -> Result<RenderLiveApplyReport, RenderLiveSettingsError> {
        let changed_fields = settings.changed_fields_from(&self.settings);
        if changed_fields.is_empty() {
            return Ok(RenderLiveApplyReport::no_op(phase));
        }
        let unsupported_fields = unsupported_fields(settings, &changed_fields);
        if !unsupported_fields.is_empty() {
            return Err(RenderLiveSettingsError::unsupported(
                phase,
                unsupported_fields,
                "GLES2 renderer supports color adjustment and BT.2446-C HDR-to-SDR into SDR \
                 BT.709; custom shader parameters and other color pipeline modes are not \
                 implemented",
            ));
        }
        self.settings = settings.clone();
        Ok(RenderLiveApplyReport::applied(phase, changed_fields))
    }
}

impl RenderLiveSettingsAdapter for LiveSettingsState {
    fn preview_live_settings(
        &mut self,
        update: &RenderLiveSettingsUpdate,
    ) -> Result<RenderLiveApplyReport, RenderLiveSettingsError> {
        self.apply_snapshot(RenderLiveApplyPhase::Preview, &update.settings)
    }

    fn commit_live_settings(
        &mut self,
        settings: &RenderLiveSettings,
    ) -> Result<RenderLiveApplyReport, RenderLiveSettingsError> {
        self.apply_snapshot(RenderLiveApplyPhase::Commit, settings)
    }

    fn rollback_live_settings(
        &mut self,
        baseline: &RenderLiveSettings,
    ) -> Result<RenderLiveApplyReport, RenderLiveSettingsError> {
        self.apply_snapshot(RenderLiveApplyPhase::Rollback, baseline)
    }
}

/// Среди изменённых полей — те, которые рендер ещё не умеет применять.
fn unsupported_fields(
    settings: &RenderLiveSettings,
    changed_fields: &[RenderLiveSettingId],
) -> Vec<RenderLiveSettingId> {
    let mut unsupported = Vec::new();
    for changed_field in changed_fields {
        match changed_field {
            RenderLiveSettingId::ShaderParameter(_) => unsupported.push(changed_field.clone()),
            RenderLiveSettingId::ColorPipelineToneMapping
                if settings.color_pipeline.tone_mapping != ToneMappingMode::Off =>
            {
                unsupported.push(changed_field.clone());
            }
            RenderLiveSettingId::ColorPipelineSwapchainTransfer
                if settings.color_pipeline.swapchain_transfer
                    != SwapchainTransferMode::PreserveCurrentUnorm =>
            {
                unsupported.push(changed_field.clone());
            }
            RenderLiveSettingId::HdrToSdrEnabled
            | RenderLiveSettingId::HdrToSdrOperator
            | RenderLiveSettingId::HdrToSdrOutputMode
            | RenderLiveSettingId::HdrToSdrSdrReferenceWhiteNits
            | RenderLiveSettingId::HdrToSdrHdrReferencePeakNits
                if !settings.hdr_to_sdr.is_phase10_bt2446_c_sdr_bt709() =>
            {
                unsupported.push(changed_field.clone());
            }
            _ => {}
        }
    }
    unsupported.sort();
    unsupported.dedup();
    unsupported
}

#[cfg(test)]
mod tests;
