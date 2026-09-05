//! Mapping пользовательского render config в renderer contracts.
//!
//! Модуль не владеет GPU state и не принимает lifecycle-решения. Его граница -
//! чистое преобразование валидированного `AppConfig` в типы `render-core`.

use anyhow::{Result, bail};
use fastiplayer_config::{AppConfig, HdrToSdrOperatorConfig, VulkanPresentMode};
use render_core::{
    ColorAdjustment, ColorPipelineSettings, HdrOutputMode, HdrToSdrSettings,
    HdrToneMappingOperator, SwapchainTransferMode,
};
use render_wgpu_shell::{ShellPresentMode, SurfaceAlphaPreference, SurfacePresentSettings};
use tracing::warn;

/// Преобразует пользовательский `[render.vulkan]` config в нейтральные shell
/// surface present настройки (present mode + желаемая swapchain latency).
pub(crate) fn surface_present_settings_from_config(
    app_config: &AppConfig,
) -> SurfacePresentSettings {
    let present_mode = match app_config.render.vulkan.present_mode {
        VulkanPresentMode::Auto => ShellPresentMode::Auto,
        VulkanPresentMode::Fifo => ShellPresentMode::Fifo,
        VulkanPresentMode::Mailbox => ShellPresentMode::Mailbox,
        VulkanPresentMode::Immediate => ShellPresentMode::Immediate,
    };

    SurfacePresentSettings {
        present_mode,
        max_frame_latency: app_config.render.vulkan.max_frame_latency,
        alpha_preference: SurfaceAlphaPreference::TransparentPreferred,
    }
}

/// Логирует legacy tone mapping placeholder, который Phase 10 не превращает в UI preset.
pub(crate) fn warn_legacy_tone_mapping_config(app_config: &AppConfig) {
    let tone_mapping_is_disabled =
        app_config.render.tone_mapping == fastiplayer_config::ToneMappingMode::Disabled;

    if tone_mapping_is_disabled {
        return;
    }

    warn!(
        tone_mapping = ?app_config.render.tone_mapping,
        "Legacy tone_mapping config не применяется как alternative HDR control в Phase 10"
    );
}

/// Собирает HDR-to-SDR renderer settings из валидированного пользовательского config.
pub(crate) fn hdr_to_sdr_settings_from_config(app_config: &AppConfig) -> HdrToSdrSettings {
    let hdr_to_sdr = &app_config.render.hdr_to_sdr;

    HdrToSdrSettings {
        enabled: hdr_to_sdr.enabled,
        operator: hdr_to_sdr_operator_from_config(hdr_to_sdr.operator),
        output_mode: HdrOutputMode::SdrBt709Only,
        sdr_reference_white_nits: hdr_to_sdr.sdr_reference_white_nits,
        hdr_reference_peak_nits: hdr_to_sdr.hdr_reference_peak_nits,
    }
}

/// Мапит TOML operator в renderer contract без добавления alternative controls.
const fn hdr_to_sdr_operator_from_config(
    operator: HdrToSdrOperatorConfig,
) -> HdrToneMappingOperator {
    match operator {
        HdrToSdrOperatorConfig::Bt2446C => HdrToneMappingOperator::Bt2446C,
    }
}

/// Собирает renderer color settings из валидированного пользовательского config.
pub(crate) fn color_pipeline_settings_from_config(
    app_config: &AppConfig,
) -> Result<ColorPipelineSettings> {
    let color_adjustment = &app_config.render.color_adjustment;

    Ok(ColorPipelineSettings {
        adjustment: ColorAdjustment {
            brightness: color_adjustment.brightness,
            contrast: color_adjustment.contrast,
            saturation: color_adjustment.saturation,
            exposure: color_adjustment.exposure,
            rgb_gain: rgb_triplet_from_config(
                "render.color_adjustment.rgb_gain",
                &color_adjustment.rgb_gain,
            )?,
            rgb_offset: rgb_triplet_from_config(
                "render.color_adjustment.rgb_offset",
                &color_adjustment.rgb_offset,
            )?,
        },
        tone_mapping: render_core::ToneMappingMode::Off,
        swapchain_transfer: SwapchainTransferMode::PreserveCurrentUnorm,
    })
}

/// Конвертирует validated RGB list из config в fixed-size renderer contract.
fn rgb_triplet_from_config(field: &'static str, values: &[f32]) -> Result<[f32; 3]> {
    if values.len() != 3 {
        bail!(
            "{field} должен содержать ровно 3 значения, получено {}",
            values.len()
        );
    }

    for (channel_index, channel_value) in values.iter().copied().enumerate() {
        if !channel_value.is_finite() {
            bail!("{field}[{channel_index}] должен быть конечным числом, получено {channel_value}");
        }
    }

    Ok([values[0], values[1], values[2]])
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Проверяет, что identity config доезжает до renderer без изменения SDR картинки.
    #[test]
    fn default_config_maps_to_identity_color_pipeline_settings() {
        let settings =
            color_pipeline_settings_from_config(&AppConfig::default()).expect("settings mapped");

        assert_eq!(settings, ColorPipelineSettings::identity());
    }

    /// Фиксирует прежний user-facing текст ошибки для неверной длины RGB triplet.
    #[test]
    fn invalid_rgb_triplet_length_keeps_error_text() {
        let mut app_config = AppConfig::default();
        app_config.render.color_adjustment.rgb_gain = vec![1.0, 1.0];

        let error = color_pipeline_settings_from_config(&app_config)
            .expect_err("invalid rgb gain should be rejected");

        assert_eq!(
            error.to_string(),
            "render.color_adjustment.rgb_gain должен содержать ровно 3 значения, получено 2"
        );
    }

    /// Проверяет, что `[render.hdr_to_sdr]` доезжает до renderer contract.
    #[test]
    fn default_config_maps_to_phase10_hdr_to_sdr_settings() {
        let settings = hdr_to_sdr_settings_from_config(&AppConfig::default());

        assert_eq!(settings, HdrToSdrSettings::default());
    }

    /// Первичное создание renderer-а всегда запрашивает прозрачную surface-композицию.
    #[test]
    fn initial_surface_settings_prefer_transparency() {
        let settings = surface_present_settings_from_config(&AppConfig::default());

        assert_eq!(
            settings.alpha_preference,
            SurfaceAlphaPreference::TransparentPreferred
        );
    }
}
