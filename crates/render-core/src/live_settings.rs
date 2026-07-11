use std::{borrow::Cow, error::Error, fmt};

use serde::{Deserialize, Serialize};

use crate::{ColorPipelineSettings, HdrToSdrSettings, ShaderParameterId, ShaderParameterSet};

/// Field-level id renderer live setting-а.
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum RenderLiveSettingId {
    /// `render.color_adjustment.brightness`.
    ColorAdjustmentBrightness,

    /// `render.color_adjustment.contrast`.
    ColorAdjustmentContrast,

    /// `render.color_adjustment.saturation`.
    ColorAdjustmentSaturation,

    /// `render.color_adjustment.exposure`.
    ColorAdjustmentExposure,

    /// `render.color_adjustment.rgb_gain`.
    ColorAdjustmentRgbGain,

    /// `render.color_adjustment.rgb_offset`.
    ColorAdjustmentRgbOffset,

    /// Renderer-neutral tone mapping mode.
    ColorPipelineToneMapping,

    /// Renderer-neutral swapchain transfer mode.
    ColorPipelineSwapchainTransfer,

    /// `render.hdr_to_sdr.enabled`.
    HdrToSdrEnabled,

    /// `render.hdr_to_sdr.operator`.
    HdrToSdrOperator,

    /// `render.hdr_to_sdr.output_mode`.
    HdrToSdrOutputMode,

    /// `render.hdr_to_sdr.sdr_reference_white_nits`.
    HdrToSdrSdrReferenceWhiteNits,

    /// `render.hdr_to_sdr.hdr_reference_peak_nits`.
    HdrToSdrHdrReferencePeakNits,

    /// Future shader parameter, определённый typed descriptor-ом.
    ShaderParameter(ShaderParameterId),
}

impl RenderLiveSettingId {
    /// Возвращает stable id для reports/status.
    #[must_use]
    pub fn stable_id(&self) -> Cow<'_, str> {
        match self {
            Self::ColorAdjustmentBrightness => Cow::Borrowed("render.color_adjustment.brightness"),
            Self::ColorAdjustmentContrast => Cow::Borrowed("render.color_adjustment.contrast"),
            Self::ColorAdjustmentSaturation => Cow::Borrowed("render.color_adjustment.saturation"),
            Self::ColorAdjustmentExposure => Cow::Borrowed("render.color_adjustment.exposure"),
            Self::ColorAdjustmentRgbGain => Cow::Borrowed("render.color_adjustment.rgb_gain"),
            Self::ColorAdjustmentRgbOffset => Cow::Borrowed("render.color_adjustment.rgb_offset"),
            Self::ColorPipelineToneMapping => Cow::Borrowed("render.color_pipeline.tone_mapping"),
            Self::ColorPipelineSwapchainTransfer => {
                Cow::Borrowed("render.color_pipeline.swapchain_transfer")
            }
            Self::HdrToSdrEnabled => Cow::Borrowed("render.hdr_to_sdr.enabled"),
            Self::HdrToSdrOperator => Cow::Borrowed("render.hdr_to_sdr.operator"),
            Self::HdrToSdrOutputMode => Cow::Borrowed("render.hdr_to_sdr.output_mode"),
            Self::HdrToSdrSdrReferenceWhiteNits => {
                Cow::Borrowed("render.hdr_to_sdr.sdr_reference_white_nits")
            }
            Self::HdrToSdrHdrReferencePeakNits => {
                Cow::Borrowed("render.hdr_to_sdr.hdr_reference_peak_nits")
            }
            Self::ShaderParameter(id) => Cow::Borrowed(id.as_str()),
        }
    }
}

/// Backend-neutral live settings snapshot, который можно применять без decode/session rebuild.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub struct RenderLiveSettings {
    /// Общие color pipeline settings.
    pub color_pipeline: ColorPipelineSettings,

    /// HDR-to-SDR settings для поддержанного HDR presentation path.
    pub hdr_to_sdr: HdrToSdrSettings,

    /// Future shader parameters с typed values.
    pub shader_parameters: ShaderParameterSet,
}

impl RenderLiveSettings {
    /// Возвращает field-level diff относительно baseline.
    #[must_use]
    pub fn changed_fields_from(&self, baseline: &Self) -> Vec<RenderLiveSettingId> {
        let mut changed_fields = Vec::new();

        push_color_pipeline_changed_fields(
            &mut changed_fields,
            &self.color_pipeline,
            &baseline.color_pipeline,
        );
        push_hdr_to_sdr_changed_fields(&mut changed_fields, &self.hdr_to_sdr, &baseline.hdr_to_sdr);

        changed_fields.extend(
            self.shader_parameters
                .changed_parameter_ids_from(&baseline.shader_parameters)
                .into_iter()
                .map(RenderLiveSettingId::ShaderParameter),
        );

        changed_fields
    }
}

impl Default for RenderLiveSettings {
    /// Default live settings совпадают с текущим renderer default contract.
    fn default() -> Self {
        Self {
            color_pipeline: ColorPipelineSettings::default(),
            hdr_to_sdr: HdrToSdrSettings::default(),
            shader_parameters: ShaderParameterSet::default(),
        }
    }
}

/// Изменение live settings, отправляемое конкретному renderer adapter-у.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub struct RenderLiveSettingsUpdate {
    /// Новый полный snapshot, который должен стать active runtime state.
    pub settings: RenderLiveSettings,

    /// Field-level ids, изменённые в этом update-е.
    pub changed_fields: Vec<RenderLiveSettingId>,
}

impl RenderLiveSettingsUpdate {
    /// Создаёт update из готового settings snapshot и explicit diff.
    #[must_use]
    pub fn new(settings: RenderLiveSettings, changed_fields: Vec<RenderLiveSettingId>) -> Self {
        Self {
            settings,
            changed_fields,
        }
    }

    /// Создаёт update, вычисляя field-level diff относительно baseline.
    #[must_use]
    pub fn from_baseline(baseline: &RenderLiveSettings, settings: RenderLiveSettings) -> Self {
        let changed_fields = settings.changed_fields_from(baseline);

        Self {
            settings,
            changed_fields,
        }
    }
}

/// Фаза применения live settings.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum RenderLiveApplyPhase {
    /// Preview update во время draft transaction.
    Preview,

    /// Commit после Apply/OK.
    Commit,

    /// Rollback к baseline после Cancel/window close.
    Rollback,
}

impl RenderLiveApplyPhase {
    /// Возвращает stable id фазы для diagnostics.
    #[must_use]
    pub const fn stable_id(self) -> &'static str {
        match self {
            Self::Preview => "preview",
            Self::Commit => "commit",
            Self::Rollback => "rollback",
        }
    }
}

/// Успешный outcome применения live settings.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum RenderLiveApplyOutcome {
    /// Snapshot уже активен; adapter ничего не менял.
    NoOp,

    /// Adapter применил один или несколько field-level changes.
    Applied,
}

/// Успешный report live settings adapter-а.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub struct RenderLiveApplyReport {
    /// Фаза применения, чтобы status layer не угадывал контекст.
    pub phase: RenderLiveApplyPhase,

    /// Итог успешной операции.
    pub outcome: RenderLiveApplyOutcome,

    /// Field-level ids, реально требовавшие изменения.
    pub changed_fields: Vec<RenderLiveSettingId>,
}

impl RenderLiveApplyReport {
    /// Создаёт no-op report.
    #[must_use]
    pub fn no_op(phase: RenderLiveApplyPhase) -> Self {
        Self {
            phase,
            outcome: RenderLiveApplyOutcome::NoOp,
            changed_fields: Vec::new(),
        }
    }

    /// Создаёт applied report.
    #[must_use]
    pub fn applied(phase: RenderLiveApplyPhase, changed_fields: Vec<RenderLiveSettingId>) -> Self {
        Self {
            phase,
            outcome: RenderLiveApplyOutcome::Applied,
            changed_fields,
        }
    }
}

/// Категория ошибки live settings adapter-а.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum RenderLiveSettingsErrorKind {
    /// Adapter жив, но конкретные fields/values не поддерживаются.
    Unsupported,

    /// Нужный runtime resource отсутствует прямо сейчас.
    AbsentResource,

    /// Backend сообщил ошибку, после которой normal retry небезопасен.
    Fatal,
}

/// Ошибка применения live settings.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum RenderLiveSettingsError {
    /// Adapter жив, но конкретные fields/values не поддерживаются.
    Unsupported {
        /// Фаза, в которой возникла ошибка.
        phase: RenderLiveApplyPhase,

        /// Affected field-level ids.
        setting_ids: Vec<RenderLiveSettingId>,

        /// Человекочитаемое объяснение для report/status.
        reason: String,
    },

    /// Runtime resource отсутствует: например, renderer ещё не создан.
    AbsentResource {
        /// Фаза, в которой возникла ошибка.
        phase: RenderLiveApplyPhase,

        /// Человекочитаемое объяснение для report/status.
        reason: String,
    },

    /// Fatal backend error.
    Fatal {
        /// Фаза, в которой возникла ошибка.
        phase: RenderLiveApplyPhase,

        /// Человекочитаемое объяснение для report/status.
        reason: String,
    },
}

impl RenderLiveSettingsError {
    /// Создаёт unsupported error с явными affected fields.
    #[must_use]
    pub fn unsupported(
        phase: RenderLiveApplyPhase,
        setting_ids: Vec<RenderLiveSettingId>,
        reason: impl Into<String>,
    ) -> Self {
        Self::Unsupported {
            phase,
            setting_ids,
            reason: reason.into(),
        }
    }

    /// Создаёт absent-resource error.
    #[must_use]
    pub fn absent_resource(phase: RenderLiveApplyPhase, reason: impl Into<String>) -> Self {
        Self::AbsentResource {
            phase,
            reason: reason.into(),
        }
    }

    /// Создаёт fatal error.
    #[must_use]
    pub fn fatal(phase: RenderLiveApplyPhase, reason: impl Into<String>) -> Self {
        Self::Fatal {
            phase,
            reason: reason.into(),
        }
    }

    /// Возвращает фазу ошибки.
    #[must_use]
    pub const fn phase(&self) -> RenderLiveApplyPhase {
        match self {
            Self::Unsupported { phase, .. }
            | Self::AbsentResource { phase, .. }
            | Self::Fatal { phase, .. } => *phase,
        }
    }

    /// Возвращает категорию ошибки без строкового parsing.
    #[must_use]
    pub const fn kind(&self) -> RenderLiveSettingsErrorKind {
        match self {
            Self::Unsupported { .. } => RenderLiveSettingsErrorKind::Unsupported,
            Self::AbsentResource { .. } => RenderLiveSettingsErrorKind::AbsentResource,
            Self::Fatal { .. } => RenderLiveSettingsErrorKind::Fatal,
        }
    }

    /// Возвращает affected fields для unsupported error-а.
    #[must_use]
    pub fn setting_ids(&self) -> &[RenderLiveSettingId] {
        match self {
            Self::Unsupported { setting_ids, .. } => setting_ids,
            Self::AbsentResource { .. } | Self::Fatal { .. } => &[],
        }
    }
}

impl fmt::Display for RenderLiveSettingsError {
    /// Пишет короткий user-facing текст без backend-specific типов.
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Unsupported {
                phase,
                setting_ids,
                reason,
            } => write!(
                formatter,
                "render live settings {} unsupported for [{}]: {}",
                phase.stable_id(),
                setting_ids
                    .iter()
                    .map(RenderLiveSettingId::stable_id)
                    .collect::<Vec<_>>()
                    .join(", "),
                reason
            ),
            Self::AbsentResource { phase, reason } => write!(
                formatter,
                "render live settings {} absent resource: {}",
                phase.stable_id(),
                reason
            ),
            Self::Fatal { phase, reason } => write!(
                formatter,
                "render live settings {} fatal error: {}",
                phase.stable_id(),
                reason
            ),
        }
    }
}

impl Error for RenderLiveSettingsError {}

/// Renderer-neutral live settings adapter boundary.
pub trait RenderLiveSettingsAdapter {
    /// Применяет preview update без TOML write и без pipeline/session rebuild.
    fn preview_live_settings(
        &mut self,
        update: &RenderLiveSettingsUpdate,
    ) -> Result<RenderLiveApplyReport, RenderLiveSettingsError>;

    /// Фиксирует уже валидированный settings snapshot как committed runtime state.
    fn commit_live_settings(
        &mut self,
        settings: &RenderLiveSettings,
    ) -> Result<RenderLiveApplyReport, RenderLiveSettingsError>;

    /// Откатывает renderer к baseline, захваченному preview transaction-ом.
    fn rollback_live_settings(
        &mut self,
        baseline: &RenderLiveSettings,
    ) -> Result<RenderLiveApplyReport, RenderLiveSettingsError>;
}

/// Добавляет field-level diff для color pipeline части live settings.
fn push_color_pipeline_changed_fields(
    changed_fields: &mut Vec<RenderLiveSettingId>,
    settings: &ColorPipelineSettings,
    baseline: &ColorPipelineSettings,
) {
    if settings.adjustment.brightness != baseline.adjustment.brightness {
        changed_fields.push(RenderLiveSettingId::ColorAdjustmentBrightness);
    }

    if settings.adjustment.contrast != baseline.adjustment.contrast {
        changed_fields.push(RenderLiveSettingId::ColorAdjustmentContrast);
    }

    if settings.adjustment.saturation != baseline.adjustment.saturation {
        changed_fields.push(RenderLiveSettingId::ColorAdjustmentSaturation);
    }

    if settings.adjustment.exposure != baseline.adjustment.exposure {
        changed_fields.push(RenderLiveSettingId::ColorAdjustmentExposure);
    }

    if settings.adjustment.rgb_gain != baseline.adjustment.rgb_gain {
        changed_fields.push(RenderLiveSettingId::ColorAdjustmentRgbGain);
    }

    if settings.adjustment.rgb_offset != baseline.adjustment.rgb_offset {
        changed_fields.push(RenderLiveSettingId::ColorAdjustmentRgbOffset);
    }

    if settings.tone_mapping != baseline.tone_mapping {
        changed_fields.push(RenderLiveSettingId::ColorPipelineToneMapping);
    }

    if settings.swapchain_transfer != baseline.swapchain_transfer {
        changed_fields.push(RenderLiveSettingId::ColorPipelineSwapchainTransfer);
    }
}

/// Добавляет field-level diff для HDR-to-SDR части live settings.
fn push_hdr_to_sdr_changed_fields(
    changed_fields: &mut Vec<RenderLiveSettingId>,
    settings: &HdrToSdrSettings,
    baseline: &HdrToSdrSettings,
) {
    if settings.enabled != baseline.enabled {
        changed_fields.push(RenderLiveSettingId::HdrToSdrEnabled);
    }

    if settings.operator != baseline.operator {
        changed_fields.push(RenderLiveSettingId::HdrToSdrOperator);
    }

    if settings.output_mode != baseline.output_mode {
        changed_fields.push(RenderLiveSettingId::HdrToSdrOutputMode);
    }

    if settings.sdr_reference_white_nits != baseline.sdr_reference_white_nits {
        changed_fields.push(RenderLiveSettingId::HdrToSdrSdrReferenceWhiteNits);
    }

    if settings.hdr_reference_peak_nits != baseline.hdr_reference_peak_nits {
        changed_fields.push(RenderLiveSettingId::HdrToSdrHdrReferencePeakNits);
    }
}
#[cfg(test)]
#[path = "tests/live_settings.rs"]
mod tests;
