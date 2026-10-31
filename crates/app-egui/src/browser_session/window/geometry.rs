//! Геометрия окна браузерного режима: открыться с запомненным размером и фуллскрином,
//! рассказать `SettingsRuntime`, что окно изменилось (решение 138).
//!
//! Обычное окно эти правила не использует и ничего не меняет: его атрибуты —
//! `main_window_attributes()` (1280×720) при любых значениях `[browser_session.window]`.
//!
//! Модуль — шов между winit и настройками: чистые функции ([`browser_window_attributes`],
//! [`observation_from_window_state`]) проверяются без живого окна, остальное — тонкие
//! обёртки над `Window`.

use fastiplayer_config::BrowserWindowConfig;
use winit::dpi::{LogicalSize, PhysicalSize};
use winit::window::{Window, WindowAttributes};

use crate::app_shell::main_window_attributes;
use crate::settings_runtime::{BrowserWindowObservation, WindowedSizePoints};
use crate::window_fullscreen::{FullscreenEntry, FullscreenWindowPort, enter_fullscreen};

/// Атрибуты окна браузерного режима: как у главного окна, но внутренний размер — из
/// запомненного (логические точки, значения уже в пределах схемы).
pub(super) fn browser_window_attributes(remembered: BrowserWindowConfig) -> WindowAttributes {
    main_window_attributes().with_inner_size(LogicalSize::new(
        f64::from(remembered.inner_width_points),
        f64::from(remembered.inner_height_points),
    ))
}

/// Если окно запомнено в фуллскрине — входит в него тем же входом, что кнопка и
/// `--fullscreen` (`window_fullscreen::enter_fullscreen`). `None` — окно оставлено оконным.
pub(super) fn apply_remembered_fullscreen(
    window: &impl FullscreenWindowPort,
    remembered: BrowserWindowConfig,
) -> Option<FullscreenEntry> {
    remembered.fullscreen.then(|| enter_fullscreen(window))
}

/// Чистое решение: что окно увидело о себе.
///
/// - Фуллскрин → [`BrowserWindowObservation::Fullscreen`] (размер не важен);
/// - обычное окно → его внутренний размер в логических точках (физический / масштаб),
///   прижатый к пределам схемы;
/// - нулевая сторона (свёрнутое окно) или нечисловой масштаб → `None`: запоминать нечего.
pub(super) fn observation_from_window_state(
    inner_size: PhysicalSize<u32>,
    scale_factor: f64,
    fullscreen: bool,
) -> Option<BrowserWindowObservation> {
    if fullscreen {
        return Some(BrowserWindowObservation::Fullscreen);
    }
    if inner_size.width == 0
        || inner_size.height == 0
        || !(scale_factor.is_finite() && scale_factor > 0.0)
    {
        return None;
    }
    let logical: LogicalSize<f64> = inner_size.to_logical(scale_factor);
    // Округление до ближайшей точки; значения неотрицательны и конечны.
    let to_points = |value: f64| value.round().clamp(0.0, f64::from(u32::MAX)) as u32;
    Some(BrowserWindowObservation::Windowed(
        WindowedSizePoints::clamped(to_points(logical.width), to_points(logical.height)),
    ))
}

/// Наблюдение за живым окном (после `Resized`).
pub(super) fn observe_window(window: &Window) -> Option<BrowserWindowObservation> {
    observation_from_window_state(
        window.inner_size(),
        window.scale_factor(),
        window.fullscreen().is_some(),
    )
}

#[cfg(test)]
mod tests {
    use winit::dpi::Size;

    use super::*;
    use crate::window_fullscreen::test_support::{FakeFullscreenWindow, FakeWindowRequest};

    fn remembered(width: u16, height: u16, fullscreen: bool) -> BrowserWindowConfig {
        BrowserWindowConfig {
            inner_width_points: width,
            inner_height_points: height,
            fullscreen,
        }
    }

    /// Окно браузерного режима открывается с запомненным размером в логических точках.
    #[test]
    fn browser_window_opens_at_the_remembered_logical_size() {
        let attributes = browser_window_attributes(remembered(900, 500, false));

        assert_eq!(
            attributes.inner_size,
            Some(Size::Logical(LogicalSize::new(900.0, 500.0)))
        );
    }

    /// Обычное окно: 1280×720 физических, что бы ни было записано для браузерного окна.
    #[test]
    fn regular_window_attributes_ignore_the_remembered_browser_window() {
        // Атрибуты браузерного окна с другим размером уже построены — обычные не меняются.
        let _browser = browser_window_attributes(remembered(900, 500, true));

        let regular = main_window_attributes();

        assert_eq!(
            regular.inner_size,
            Some(Size::Physical(PhysicalSize::new(1280, 720)))
        );
    }

    /// Запомненный фуллскрин → вход тем же путём, что кнопка; без него — ничего.
    #[test]
    fn remembered_fullscreen_enters_through_the_shared_entry() {
        let window = FakeFullscreenWindow::default();

        let entry = apply_remembered_fullscreen(&window, remembered(900, 500, true));

        assert_eq!(entry, Some(FullscreenEntry::Requested));
        assert_eq!(
            window.requests(),
            vec![FakeWindowRequest::BorderlessFullscreen]
        );

        let untouched = FakeFullscreenWindow::default();
        assert_eq!(
            apply_remembered_fullscreen(&untouched, remembered(900, 500, false)),
            None
        );
        assert!(untouched.requests().is_empty());
    }

    /// Масштаб 2: физические 1800×1000 — это 900×500 логических точек.
    #[test]
    fn windowed_size_is_reported_in_logical_points() {
        let observation = observation_from_window_state(PhysicalSize::new(1800, 1000), 2.0, false);

        assert_eq!(
            observation,
            Some(BrowserWindowObservation::Windowed(
                WindowedSizePoints::clamped(900, 500)
            ))
        );
    }

    /// В фуллскрине размер не наблюдается: только факт фуллскрина.
    #[test]
    fn fullscreen_hides_the_size() {
        let observation = observation_from_window_state(PhysicalSize::new(3840, 2160), 1.0, true);

        assert_eq!(observation, Some(BrowserWindowObservation::Fullscreen));
    }

    /// Свёрнутое окно (нулевая сторона) и странный масштаб ничего не запоминают.
    #[test]
    fn degenerate_window_state_is_not_remembered() {
        assert_eq!(
            observation_from_window_state(PhysicalSize::new(0, 500), 1.0, false),
            None
        );
        assert_eq!(
            observation_from_window_state(PhysicalSize::new(900, 500), 0.0, false),
            None
        );
        assert_eq!(
            observation_from_window_state(PhysicalSize::new(900, 500), f64::NAN, false),
            None
        );
    }
}
