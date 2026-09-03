//! Typed surface present settings и детерминированный выбор present mode.

use anyhow::{Context, Result};

use crate::SurfaceAlphaPreference;

/// Предпочтительный present mode swapchain-а в нейтральной форме.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ShellPresentMode {
    /// Авто: предпочесть FIFO (VSync), иначе первый доступный режим backend-а.
    Auto,
    /// FIFO: классический VSync, present блокирует до кадрового интервала.
    Fifo,
    /// Mailbox: новый кадр вытесняет очередь.
    Mailbox,
    /// Immediate: без синхронизации, поэтому возможен tearing.
    Immediate,
}

/// Нейтральные surface present настройки, прокидываемые в shell из композиции.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct SurfacePresentSettings {
    /// Предпочтительный present mode.
    pub present_mode: ShellPresentMode,
    /// Желаемая глубина swapchain latency; ноль нормализуется к единице.
    pub max_frame_latency: u32,
    /// Намерение вызывающего слоя относительно прозрачной desktop-композиции.
    pub alpha_preference: SurfaceAlphaPreference,
}

impl Default for SurfacePresentSettings {
    fn default() -> Self {
        Self {
            present_mode: ShellPresentMode::Auto,
            max_frame_latency: 2,
            alpha_preference: SurfaceAlphaPreference::Opaque,
        }
    }
}

/// Выбирает present mode с явным FIFO fallback и typed ошибкой пустых capabilities.
pub(crate) fn choose_present_mode(
    present_modes: &[wgpu::PresentMode],
    preference: ShellPresentMode,
) -> Result<wgpu::PresentMode> {
    let auto_choice = || -> Result<wgpu::PresentMode> {
        if present_modes.contains(&wgpu::PresentMode::Fifo) {
            return Ok(wgpu::PresentMode::Fifo);
        }
        present_modes
            .first()
            .copied()
            .context("Surface capabilities не вернул ни одного present mode")
    };
    let requested = match preference {
        ShellPresentMode::Auto => return auto_choice(),
        ShellPresentMode::Fifo => wgpu::PresentMode::Fifo,
        ShellPresentMode::Mailbox => wgpu::PresentMode::Mailbox,
        ShellPresentMode::Immediate => wgpu::PresentMode::Immediate,
    };
    if present_modes.contains(&requested) {
        return Ok(requested);
    }
    tracing::warn!(requested = ?requested, available = ?present_modes, "Запрошенный present mode не поддержан surface-ом; откат к FIFO/первому доступному");
    auto_choice()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn present_mode_auto_prefers_fifo_when_available() {
        assert_eq!(
            choose_present_mode(
                &[wgpu::PresentMode::Immediate, wgpu::PresentMode::Fifo],
                ShellPresentMode::Auto
            )
            .expect("present mode selected"),
            wgpu::PresentMode::Fifo
        );
    }

    #[test]
    fn present_mode_uses_requested_when_supported() {
        assert_eq!(
            choose_present_mode(
                &[wgpu::PresentMode::Fifo, wgpu::PresentMode::Mailbox],
                ShellPresentMode::Mailbox
            )
            .expect("present mode selected"),
            wgpu::PresentMode::Mailbox
        );
    }

    #[test]
    fn present_mode_falls_back_to_fifo_when_requested_unsupported() {
        assert_eq!(
            choose_present_mode(
                &[wgpu::PresentMode::Fifo, wgpu::PresentMode::Immediate],
                ShellPresentMode::Mailbox
            )
            .expect("present mode selected"),
            wgpu::PresentMode::Fifo
        );
    }

    #[test]
    fn empty_present_mode_list_is_reported_as_error() {
        let error = choose_present_mode(&[], ShellPresentMode::Auto)
            .expect_err("empty present mode list rejected");
        assert!(
            error
                .to_string()
                .contains("Surface capabilities не вернул ни одного present mode")
        );
    }
}
