//! Выбор desktop surface alpha mode отдельно от общего renderer lifecycle.

use std::sync::atomic::{AtomicBool, Ordering};

use anyhow::{Context, Result};

use crate::window_corner_mask::SurfaceAlphaEncoding;

/// Process-wide защита от повторения fallback warning при controlled recreation.
static TRANSPARENCY_FALLBACK_WARNED: AtomicBool = AtomicBool::new(false);

/// Политика выбора alpha mode для desktop surface.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SurfaceAlphaPreference {
    /// Выбирать только непрозрачную композицию для обычных прямоугольных окон.
    Opaque,
    /// Предпочесть подтверждённую прозрачность, но безопасно продолжить с opaque fallback.
    TransparentPreferred,
}

/// Выбирает фактический alpha mode и сообщает, можно ли безопасно исполнять corner mask.
pub(crate) fn choose_alpha_mode(
    alpha_modes: &[wgpu::CompositeAlphaMode],
    preference: SurfaceAlphaPreference,
) -> Result<(wgpu::CompositeAlphaMode, Option<SurfaceAlphaEncoding>)> {
    if alpha_modes.is_empty() {
        anyhow::bail!("Surface capabilities не вернул ни одного alpha mode");
    }
    let first_supported = |choices: &[wgpu::CompositeAlphaMode]| {
        choices
            .iter()
            .copied()
            .find(|choice| alpha_modes.contains(choice))
    };
    match preference {
        SurfaceAlphaPreference::Opaque => {
            let mode = first_supported(&[
                wgpu::CompositeAlphaMode::Opaque,
                wgpu::CompositeAlphaMode::Inherit,
                wgpu::CompositeAlphaMode::Auto,
            ])
            .context("Surface не поддерживает ни одного непрозрачного alpha mode")?;
            Ok((mode, None))
        }
        SurfaceAlphaPreference::TransparentPreferred => {
            if alpha_modes.contains(&wgpu::CompositeAlphaMode::PreMultiplied) {
                return Ok((
                    wgpu::CompositeAlphaMode::PreMultiplied,
                    Some(SurfaceAlphaEncoding::Premultiplied),
                ));
            }
            if alpha_modes.contains(&wgpu::CompositeAlphaMode::PostMultiplied) {
                return Ok((
                    wgpu::CompositeAlphaMode::PostMultiplied,
                    Some(SurfaceAlphaEncoding::Postmultiplied),
                ));
            }
            let fallback = first_supported(&[
                wgpu::CompositeAlphaMode::Opaque,
                wgpu::CompositeAlphaMode::Inherit,
            ])
            .context("Surface не поддерживает пригодный alpha fallback")?;
            if !TRANSPARENCY_FALLBACK_WARNED.swap(true, Ordering::Relaxed) {
                tracing::warn!(available = ?alpha_modes, selected = ?fallback, "Surface не поддерживает прозрачную композицию; скругление окна отключено");
            }
            Ok((fallback, None))
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn empty_alpha_mode_list_is_reported_as_error() {
        let error = choose_alpha_mode(&[], SurfaceAlphaPreference::TransparentPreferred)
            .expect_err("empty alpha mode list rejected");
        assert!(
            error
                .to_string()
                .contains("Surface capabilities не вернул ни одного alpha mode")
        );
    }

    #[test]
    fn transparent_alpha_prefers_premultiplied_over_postmultiplied() {
        let selected = choose_alpha_mode(
            &[
                wgpu::CompositeAlphaMode::PostMultiplied,
                wgpu::CompositeAlphaMode::PreMultiplied,
                wgpu::CompositeAlphaMode::Opaque,
            ],
            SurfaceAlphaPreference::TransparentPreferred,
        )
        .expect("transparent alpha selected");
        assert_eq!(selected.0, wgpu::CompositeAlphaMode::PreMultiplied);
        assert_eq!(selected.1, Some(SurfaceAlphaEncoding::Premultiplied));
    }

    #[test]
    fn transparent_alpha_uses_postmultiplied_when_needed() {
        let selected = choose_alpha_mode(
            &[
                wgpu::CompositeAlphaMode::Opaque,
                wgpu::CompositeAlphaMode::PostMultiplied,
            ],
            SurfaceAlphaPreference::TransparentPreferred,
        )
        .expect("postmultiplied alpha selected");
        assert_eq!(selected.0, wgpu::CompositeAlphaMode::PostMultiplied);
        assert_eq!(selected.1, Some(SurfaceAlphaEncoding::Postmultiplied));
    }

    #[test]
    fn transparent_alpha_falls_back_to_square_opaque_surface() {
        let selected = choose_alpha_mode(
            &[
                wgpu::CompositeAlphaMode::Inherit,
                wgpu::CompositeAlphaMode::Opaque,
            ],
            SurfaceAlphaPreference::TransparentPreferred,
        )
        .expect("opaque fallback selected");
        assert_eq!(selected, (wgpu::CompositeAlphaMode::Opaque, None));
    }

    #[test]
    fn transparent_alpha_uses_inherit_when_opaque_is_unavailable() {
        let selected = choose_alpha_mode(
            &[wgpu::CompositeAlphaMode::Inherit],
            SurfaceAlphaPreference::TransparentPreferred,
        )
        .expect("inherit fallback selected");
        assert_eq!(selected, (wgpu::CompositeAlphaMode::Inherit, None));
    }

    #[test]
    fn opaque_alpha_policy_never_selects_transparent_mode() {
        let selected = choose_alpha_mode(
            &[
                wgpu::CompositeAlphaMode::PreMultiplied,
                wgpu::CompositeAlphaMode::Opaque,
            ],
            SurfaceAlphaPreference::Opaque,
        )
        .expect("opaque alpha selected");
        assert_eq!(selected, (wgpu::CompositeAlphaMode::Opaque, None));
    }
}
