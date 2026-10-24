//! Typed surface present settings и детерминированный выбор present mode.

use anyhow::{Context, Result};

use crate::ShellPresentMode;

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
            .context("Surface capabilities have not returned any present mode")
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
    tracing::warn!(requested = ?requested, available = ?present_modes, "Requested present mode not supported by surface; rollback to FIFO/first available");
    auto_choice()
}

/// Выбирает формат swapchain для SDR-видео.
pub(crate) fn choose_surface_format(
    formats: &[wgpu::TextureFormat],
) -> Result<wgpu::TextureFormat> {
    // Для текущего NV12 renderer предпочитаем обычный 8-bit формат.
    const PREFERRED_FORMATS: &[wgpu::TextureFormat] = &[
        wgpu::TextureFormat::Bgra8Unorm,
        wgpu::TextureFormat::Rgba8Unorm,
        wgpu::TextureFormat::Bgra8UnormSrgb,
        wgpu::TextureFormat::Rgba8UnormSrgb,
    ];

    // Сначала ищем явно поддерживаемый 8-bit формат.
    for preferred_format in PREFERRED_FORMATS {
        if formats.contains(preferred_format) {
            return Ok(*preferred_format);
        }
    }

    // Если 8-bit форматов нет, используем первый формат из capabilities.
    formats
        .first()
        .copied()
        .context("Surface capabilities have not returned any texture format.")
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
                .contains("Surface capabilities have not returned any present mode")
        );
    }
}

#[cfg(test)]
mod surface_format_tests {
    use super::choose_surface_format;

    /// Проверяет, что SDR path предпочитает Unorm, чтобы не включить implicit sRGB transfer.
    #[test]
    fn surface_format_prefers_current_unorm_path_before_srgb() {
        let formats = [
            wgpu::TextureFormat::Bgra8UnormSrgb,
            wgpu::TextureFormat::Rgba8Unorm,
            wgpu::TextureFormat::Bgra8Unorm,
        ];

        let selected_format = choose_surface_format(&formats).expect("surface format selected");

        assert_eq!(selected_format, wgpu::TextureFormat::Bgra8Unorm);
    }

    /// Проверяет явную ошибку вместо panic при некорректных surface capabilities.
    #[test]
    fn empty_surface_format_list_is_reported_as_error() {
        let error = choose_surface_format(&[]).expect_err("empty format list rejected");

        assert!(
            error
                .to_string()
                .contains("Surface capabilities have not returned any texture format.")
        );
    }
}
