//! Выбор EGL-конфига (чистая функция, без драйвера).
//!
//! Правила:
//! 1. Предпочитаем ровно 8 бит на R/G/B (10-битные конфиги ломают premultiplied-смешивание egui
//!    в гамма-пространстве и не нужны).
//! 2. X11: конфиг ОБЯЗАН совпадать с visual окна (`EGL_NATIVE_VISUAL_ID`), иначе
//!    `eglCreateWindowSurface` откажет или прозрачность сломается. Прозрачность на X11 решает
//!    visual окна: winit с `with_transparent(true)` создаёт 32-битный ARGB visual.
//! 3. Прозрачность: при `TransparentPreferred` — конфиг с альфой ≥ 8, иначе честный фолбэк
//!    «непрозрачно, углы квадратные». При `Opaque` — предпочитаем конфиг без альфы.
//!
//! Порядок кандидатов внутри одной категории — порядок `eglChooseConfig` (EGL 1.5, 3.4.1.2:
//! лучшие первыми).

use super::driver::ConfigDescription;
use crate::presentation::{OpaqueWindowReason, WindowAlphaMode, WindowAlphaRequest};

/// Что учитывать при выборе.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) struct ConfigSelectionRequest {
    /// Желаемая прозрачность.
    pub(crate) alpha: WindowAlphaRequest,
    /// Visual окна X11 (`None` — Wayland, offscreen или X11 без известного visual).
    pub(crate) required_native_visual: Option<i64>,
    /// Цель — offscreen-стенд: прозрачности показа нет.
    pub(crate) offscreen: bool,
}

/// Выбранный конфиг.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) struct SelectedConfig {
    /// Конфиг и его атрибуты.
    pub(crate) description: ConfigDescription,
    /// Что вышло с прозрачностью.
    pub(crate) alpha: WindowAlphaMode,
}

/// Почему конфиг не выбран.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum ConfigSelectionError {
    /// Драйвер не дал ни одного конфига с GLES2 и RGB 8 бит.
    NoGles2Config,
    /// Ни один конфиг не совпадает с visual окна X11.
    NoConfigForWindowVisual { visual_id: i64 },
}

const ALPHA_BITS_FOR_TRANSPARENCY: i32 = 8;

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
            alpha: WindowAlphaMode::Opaque {
                reason: OpaqueWindowReason::OffscreenTarget,
                framebuffer_has_alpha: description.alpha_bits > 0,
            },
        });
    }

    let with_alpha = preferred_depth
        .iter()
        .copied()
        .find(|config| config.alpha_bits >= ALPHA_BITS_FOR_TRANSPARENCY);
    let without_alpha = preferred_depth
        .iter()
        .copied()
        .find(|config| config.alpha_bits == 0);

    match request.alpha {
        WindowAlphaRequest::TransparentPreferred => match with_alpha {
            Some(description) => Ok(SelectedConfig {
                description,
                alpha: WindowAlphaMode::PremultipliedTransparent,
            }),
            None => opaque(
                without_alpha,
                &preferred_depth,
                OpaqueWindowReason::NoAlphaConfig,
            ),
        },
        WindowAlphaRequest::Opaque => opaque(
            without_alpha,
            &preferred_depth,
            OpaqueWindowReason::Requested,
        ),
    }
}

fn opaque(
    without_alpha: Option<ConfigDescription>,
    candidates: &[ConfigDescription],
    reason: OpaqueWindowReason,
) -> Result<SelectedConfig, ConfigSelectionError> {
    let description = match without_alpha {
        Some(description) => description,
        None => first_or_error(candidates)?,
    };
    Ok(SelectedConfig {
        description,
        alpha: WindowAlphaMode::Opaque {
            reason,
            framebuffer_has_alpha: description.alpha_bits > 0,
        },
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

    fn window_request(alpha: WindowAlphaRequest, visual: Option<i64>) -> ConfigSelectionRequest {
        ConfigSelectionRequest {
            alpha,
            required_native_visual: visual,
            offscreen: false,
        }
    }

    #[test]
    fn transparent_request_picks_alpha_config() {
        let candidates = [config(1, 8, 0, 0), config(2, 8, 8, 0)];
        let selected = select_config(
            &candidates,
            window_request(WindowAlphaRequest::TransparentPreferred, None),
        );
        assert_eq!(
            selected.map(|selected| (selected.description.id, selected.alpha)),
            Ok((ConfigId(2), WindowAlphaMode::PremultipliedTransparent))
        );
    }

    #[test]
    fn transparent_request_without_alpha_config_falls_back_to_square_corners() {
        let candidates = [config(1, 8, 0, 0)];
        let selected = select_config(
            &candidates,
            window_request(WindowAlphaRequest::TransparentPreferred, None),
        );
        assert_eq!(
            selected.map(|selected| selected.alpha),
            Ok(WindowAlphaMode::Opaque {
                reason: OpaqueWindowReason::NoAlphaConfig,
                framebuffer_has_alpha: false
            })
        );
    }

    #[test]
    fn opaque_request_prefers_config_without_alpha_and_reports_forced_alpha() {
        let both = [config(1, 8, 8, 0), config(2, 8, 0, 0)];
        let selected = select_config(&both, window_request(WindowAlphaRequest::Opaque, None));
        assert_eq!(
            selected.map(|selected| selected.description.id),
            Ok(ConfigId(2))
        );

        // Только конфиги с альфой: художник обязан писать альфу = 1.
        let alpha_only = [config(3, 8, 8, 0)];
        let selected = select_config(
            &alpha_only,
            window_request(WindowAlphaRequest::Opaque, None),
        );
        assert_eq!(
            selected.map(|selected| selected.alpha),
            Ok(WindowAlphaMode::Opaque {
                reason: OpaqueWindowReason::Requested,
                framebuffer_has_alpha: true
            })
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
        let selected = select_config(
            &candidates,
            window_request(WindowAlphaRequest::TransparentPreferred, Some(0x21)),
        );
        assert_eq!(
            selected.map(|selected| selected.description.id),
            Ok(ConfigId(2))
        );

        // Непрозрачный visual окна: альфы нет, хотя у других visual она есть.
        let selected = select_config(
            &candidates,
            window_request(WindowAlphaRequest::TransparentPreferred, Some(0x22)),
        );
        assert_eq!(
            selected.map(|selected| (selected.description.id, selected.alpha)),
            Ok((
                ConfigId(3),
                WindowAlphaMode::Opaque {
                    reason: OpaqueWindowReason::NoAlphaConfig,
                    framebuffer_has_alpha: false
                }
            ))
        );

        assert_eq!(
            select_config(
                &candidates,
                window_request(WindowAlphaRequest::Opaque, Some(0x99))
            ),
            Err(ConfigSelectionError::NoConfigForWindowVisual { visual_id: 0x99 })
        );
    }

    #[test]
    fn exact_8bit_configs_win_over_deeper_ones() {
        let candidates = [config(1, 10, 2, 0), config(2, 8, 8, 0)];
        let selected = select_config(
            &candidates,
            window_request(WindowAlphaRequest::TransparentPreferred, None),
        );
        assert_eq!(
            selected.map(|selected| selected.description.id),
            Ok(ConfigId(2))
        );
    }

    #[test]
    fn empty_candidate_list_means_no_gles2() {
        assert_eq!(
            select_config(&[], window_request(WindowAlphaRequest::Opaque, None)),
            Err(ConfigSelectionError::NoGles2Config)
        );
    }

    #[test]
    fn offscreen_target_reports_offscreen_reason() {
        let candidates = [config(1, 8, 8, 0)];
        let selected = select_config(
            &candidates,
            ConfigSelectionRequest {
                alpha: WindowAlphaRequest::TransparentPreferred,
                required_native_visual: None,
                offscreen: true,
            },
        );
        assert_eq!(
            selected.map(|selected| selected.alpha),
            Ok(WindowAlphaMode::Opaque {
                reason: OpaqueWindowReason::OffscreenTarget,
                framebuffer_has_alpha: true
            })
        );
    }
}
