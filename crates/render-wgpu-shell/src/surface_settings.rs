//! Typed surface present settings, детерминированный выбор present mode и alpha mode.

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
    let Some(requested) = requested_wgpu_present_mode(preference) else {
        return auto_choice();
    };
    if present_modes.contains(&requested) {
        return Ok(requested);
    }
    tracing::warn!(requested = ?requested, available = ?present_modes, "Requested present mode not supported by surface; rollback to FIFO/first available");
    auto_choice()
}

/// Конкретный режим wgpu, который просит пользователь; `None` у `Auto` (решает surface).
///
/// Единственный перевод нейтрального режима в wgpu: им же перенастройка показа на месте
/// (сессия 02a) определяет, пришлось ли откатиться на запасной режим.
pub(crate) const fn requested_wgpu_present_mode(
    preference: ShellPresentMode,
) -> Option<wgpu::PresentMode> {
    match preference {
        ShellPresentMode::Auto => None,
        ShellPresentMode::Fifo => Some(wgpu::PresentMode::Fifo),
        ShellPresentMode::Mailbox => Some(wgpu::PresentMode::Mailbox),
        ShellPresentMode::Immediate => Some(wgpu::PresentMode::Immediate),
    }
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

/// Выбирает непрозрачный composite alpha mode окна.
///
/// Окно Fastiplayer всегда непрозрачное (скругление углов удалено, решение 57 gles2-renderer),
/// поэтому прозрачные режимы (`PreMultiplied`/`PostMultiplied`) не выбираются никогда, даже если
/// surface их предлагает: на Intel Ironlake альфа-поверхность не идёт на плоскость дисплея и
/// композитор копирует каждый кадр. Порядок: `Opaque`, затем `Inherit`, затем `Auto`.
pub(crate) fn choose_opaque_alpha_mode(
    alpha_modes: &[wgpu::CompositeAlphaMode],
) -> Result<wgpu::CompositeAlphaMode> {
    if alpha_modes.is_empty() {
        anyhow::bail!("Surface capabilities have not returned any alpha mode.");
    }
    [
        wgpu::CompositeAlphaMode::Opaque,
        wgpu::CompositeAlphaMode::Inherit,
        wgpu::CompositeAlphaMode::Auto,
    ]
    .into_iter()
    .find(|choice| alpha_modes.contains(choice))
    .context("Surface does not support any opaque alpha mode.")
}

#[cfg(test)]
mod alpha_mode_tests {
    use super::choose_opaque_alpha_mode;
    use wgpu::CompositeAlphaMode as Mode;

    /// Прозрачные режимы игнорируются, даже когда они первые в списке surface.
    #[test]
    fn transparent_modes_are_never_selected_when_opaque_is_available() {
        let selected =
            choose_opaque_alpha_mode(&[Mode::PreMultiplied, Mode::PostMultiplied, Mode::Opaque])
                .expect("opaque mode selected");
        assert_eq!(selected, Mode::Opaque);
    }

    /// Без `Opaque` берётся `Inherit`, затем `Auto` — в этом порядке.
    #[test]
    fn opaque_falls_back_to_inherit_then_auto() {
        assert_eq!(
            choose_opaque_alpha_mode(&[Mode::Auto, Mode::PreMultiplied, Mode::Inherit])
                .expect("inherit selected"),
            Mode::Inherit
        );
        assert_eq!(
            choose_opaque_alpha_mode(&[Mode::PostMultiplied, Mode::Auto]).expect("auto selected"),
            Mode::Auto
        );
    }

    /// Только прозрачные режимы — ошибка, а не молчаливый выбор прозрачного.
    #[test]
    fn transparent_only_surface_is_reported_as_error() {
        let error = choose_opaque_alpha_mode(&[Mode::PreMultiplied, Mode::PostMultiplied])
            .expect_err("transparent-only list rejected");
        assert!(error.to_string().contains("any opaque alpha mode"));
    }

    /// Пустой список capabilities — явная ошибка.
    #[test]
    fn empty_alpha_mode_list_is_reported_as_error() {
        let error = choose_opaque_alpha_mode(&[]).expect_err("empty alpha mode list rejected");
        assert!(
            error
                .to_string()
                .contains("Surface capabilities have not returned any alpha mode.")
        );
    }
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
