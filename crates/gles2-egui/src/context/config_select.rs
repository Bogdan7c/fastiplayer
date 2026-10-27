//! Выбор EGL-конфига (чистая функция, без драйвера).
//!
//! Правила:
//! 1. Предпочитаем ровно 8 бит на R/G/B (10-битные конфиги ломают premultiplied-смешивание egui
//!    в гамма-пространстве и не нужны).
//! 2. X11: конфиг ОБЯЗАН совпадать с visual окна (`EGL_NATIVE_VISUAL_ID`), иначе
//!    `eglCreateWindowSurface` откажет. Visual окна у нас обычный непрозрачный (winit без
//!    `with_transparent`).
//! 3. Окно всегда непрозрачное (скругление углов удалено, решение 57 плана gles2-renderer):
//!    предпочитаем конфиг БЕЗ альфы. Если драйвер даёт только конфиги с альфой, берём лучший из
//!    них, а факт «у буфера есть альфа» ([`FramebufferAlpha::Present`]) сообщаем рисующим: они
//!    обязаны писать альфу = 1.
//!
//! Порядок кандидатов внутри одной категории — порядок `eglChooseConfig` (EGL 1.5, 3.4.1.2:
//! лучшие первыми).

use super::driver::ConfigDescription;
use crate::presentation::FramebufferAlpha;

/// Что учитывать при выборе.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) struct ConfigSelectionRequest {
    /// Visual окна X11 (`None` — Wayland, offscreen или X11 без известного visual).
    pub(crate) required_native_visual: Option<i64>,
    /// Цель — offscreen-стенд: показа окна нет, берём лучший конфиг как есть.
    pub(crate) offscreen: bool,
}

/// Выбранный конфиг.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) struct SelectedConfig {
    /// Конфиг и его атрибуты.
    pub(crate) description: ConfigDescription,
    /// Есть ли у буфера выбранного конфига канал альфы.
    pub(crate) alpha: FramebufferAlpha,
}

/// Почему конфиг не выбран.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum ConfigSelectionError {
    /// Драйвер не дал ни одного конфига с GLES2 и RGB 8 бит.
    NoGles2Config,
    /// Ни один конфиг не совпадает с visual окна X11.
    NoConfigForWindowVisual { visual_id: i64 },
}

/// Выбирает конфиг по правилам модуля.
pub(crate) fn select_config(
    candidates: &[ConfigDescription],
    request: ConfigSelectionRequest,
) -> Result<SelectedConfig, ConfigSelectionError> {
    if candidates.is_empty() {
        return Err(ConfigSelectionError::NoGles2Config);
    }
    let visual_matching: Vec<ConfigDescription> = match request.required_native_visual {
        Some(visual_id) => {
            let matching: Vec<_> = candidates
                .iter()
                .copied()
                .filter(|config| config.native_visual_id == visual_id)
                .collect();
            if matching.is_empty() {
                return Err(ConfigSelectionError::NoConfigForWindowVisual { visual_id });
            }
            matching
        }
        None => candidates.to_vec(),
    };
    let preferred_depth = prefer_exact_8bit(&visual_matching);

    if request.offscreen {
        // Offscreen рисует в свой FBO: альфа буфера по умолчанию не важна, берём лучший.
        let description = first_or_error(&preferred_depth)?;
        return Ok(SelectedConfig {
            description,
            alpha: FramebufferAlpha::from_alpha_bits(description.alpha_bits),
        });
    }

    // Окно непрозрачное: конфиг без альфы, а если его нет — лучший из имеющихся.
    let without_alpha = preferred_depth
        .iter()
        .copied()
        .find(|config| config.alpha_bits == 0);
    let description = match without_alpha {
        Some(description) => description,
        None => first_or_error(&preferred_depth)?,
    };
    Ok(SelectedConfig {
        description,
        alpha: FramebufferAlpha::from_alpha_bits(description.alpha_bits),
    })
}

fn prefer_exact_8bit(candidates: &[ConfigDescription]) -> Vec<ConfigDescription> {
    let exact: Vec<_> = candidates
        .iter()
        .copied()
        .filter(|config| config.red_bits == 8 && config.green_bits == 8 && config.blue_bits == 8)
        .collect();
    if exact.is_empty() {
        candidates.to_vec()
    } else {
        exact
    }
}

fn first_or_error(
    candidates: &[ConfigDescription],
) -> Result<ConfigDescription, ConfigSelectionError> {
    candidates
        .first()
        .copied()
        .ok_or(ConfigSelectionError::NoGles2Config)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::context::driver::ConfigId;

    fn config(id: usize, rgb_bits: i32, alpha_bits: i32, visual: i64) -> ConfigDescription {
        ConfigDescription {
            id: ConfigId(id),
            red_bits: rgb_bits,
            green_bits: rgb_bits,
            blue_bits: rgb_bits,
            alpha_bits,
            native_visual_id: visual,
            min_swap_interval: 0,
            max_swap_interval: 1,
        }
    }

    fn window_request(visual: Option<i64>) -> ConfigSelectionRequest {
        ConfigSelectionRequest {
            required_native_visual: visual,
            offscreen: false,
        }
    }

    #[test]
    fn window_prefers_config_without_alpha_even_when_alpha_config_is_listed_first() {
        let candidates = [config(1, 8, 8, 0), config(2, 8, 0, 0)];
        let selected = select_config(&candidates, window_request(None));
        assert_eq!(
            selected.map(|selected| (selected.description.id, selected.alpha)),
            Ok((ConfigId(2), FramebufferAlpha::Absent))
        );
    }

    #[test]
    fn window_with_only_alpha_configs_reports_framebuffer_alpha() {
        // Только конфиги с альфой: берём лучший, художник обязан писать альфу = 1.
        let alpha_only = [config(3, 8, 8, 0)];
        let selected = select_config(&alpha_only, window_request(None));
        assert_eq!(
            selected.map(|selected| (selected.description.id, selected.alpha)),
            Ok((ConfigId(3), FramebufferAlpha::Present))
        );
    }

    #[test]
    fn x11_config_must_match_window_visual() {
        // Как на T410 под X11: ARGB visual 0x21 у части конфигов.
        let candidates = [
            config(1, 8, 8, 0x20),
            config(2, 8, 8, 0x21),
            config(3, 8, 0, 0x22),
        ];
        // Окно непрозрачное, но visual задан системой: берём конфиг этого visual, даже с альфой.
        let selected = select_config(&candidates, window_request(Some(0x21)));
        assert_eq!(
            selected.map(|selected| (selected.description.id, selected.alpha)),
            Ok((ConfigId(2), FramebufferAlpha::Present))
        );

        // Непрозрачный visual окна: альфы нет.
        let selected = select_config(&candidates, window_request(Some(0x22)));
        assert_eq!(
            selected.map(|selected| (selected.description.id, selected.alpha)),
            Ok((ConfigId(3), FramebufferAlpha::Absent))
        );

        assert_eq!(
            select_config(&candidates, window_request(Some(0x99))),
            Err(ConfigSelectionError::NoConfigForWindowVisual { visual_id: 0x99 })
        );
    }

    #[test]
    fn exact_8bit_configs_win_over_deeper_ones() {
        let candidates = [config(1, 10, 2, 0), config(2, 8, 8, 0)];
        let selected = select_config(&candidates, window_request(None));
        assert_eq!(
            selected.map(|selected| selected.description.id),
            Ok(ConfigId(2))
        );
    }

    #[test]
    fn empty_candidate_list_means_no_gles2() {
        assert_eq!(
            select_config(&[], window_request(None)),
            Err(ConfigSelectionError::NoGles2Config)
        );
    }

    #[test]
    fn offscreen_target_takes_best_config_and_reports_its_alpha() {
        let candidates = [config(1, 8, 8, 0), config(2, 8, 0, 0)];
        let selected = select_config(
            &candidates,
            ConfigSelectionRequest {
                required_native_visual: None,
                offscreen: true,
            },
        );
        assert_eq!(
            selected.map(|selected| (selected.description.id, selected.alpha)),
            Ok((ConfigId(1), FramebufferAlpha::Present))
        );
    }
}
