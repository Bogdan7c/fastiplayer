//! Минимальный размер главного окна.
//!
//! Окно без системной рамки: titlebar и нижняя панель управления рисует сам egui.
//! Если окно ниже суммы их высот, панели налезают друг на друга и таймлайн с кнопками
//! перестают быть кликабельными. Поэтому минимум выводится из реальных размеров UI,
//! а не выбирается «на глаз»; тест `bottom_controls_fit_into_reserved_height` измеряет
//! настоящую нижнюю панель и ломается, если раскладка выросла, а константы — нет.

use winit::dpi::LogicalSize;

/// Минимальная ширина окна в логических точках.
///
/// Гарантирует полный порядок transport controls в нижней панели
/// (проверяется раскладочными тестами `player_controls` для ширины 400).
pub(super) const MIN_WINDOW_WIDTH_POINTS: f32 = 400.0;

/// Высота titlebar по умолчанию (`ui.window.titlebar_height_px`).
///
/// Настройка допускает до 96, но резервировать под максимум значит запретить
/// компактное окно всем; при нестандартном значении панели лишь частично
/// перекроют видео, но не друг друга, пока окно выше минимума плюс разница.
const DEFAULT_TITLEBAR_HEIGHT_POINTS: f32 = 40.0;

/// Высота нижней панели: поля, подписи времени, таймлайн, строка кнопок
/// (в тесте измерено 117) плюс строка глобального статуса очереди с кнопкой
/// «Отменить ожидание» (с ней измерено 159) и 1 точка запаса. Строка статуса
/// появляется временно, но перекрывать titlebar при её появлении нельзя.
const BOTTOM_PANEL_RESERVED_HEIGHT_POINTS: f32 = 160.0;

/// Минимальная полоса видео между titlebar и нижней панелью, чтобы окно не
/// превращалось в одни панели управления.
const MIN_VIDEO_STRIP_HEIGHT_POINTS: f32 = 32.0;

/// Минимальная высота окна в логических точках.
pub(super) const MIN_WINDOW_HEIGHT_POINTS: f32 = DEFAULT_TITLEBAR_HEIGHT_POINTS
    + BOTTOM_PANEL_RESERVED_HEIGHT_POINTS
    + MIN_VIDEO_STRIP_HEIGHT_POINTS;

/// Минимальный внутренний размер окна для `with_min_inner_size`.
#[must_use]
pub(super) const fn min_inner_size() -> LogicalSize<f32> {
    LogicalSize::new(MIN_WINDOW_WIDTH_POINTS, MIN_WINDOW_HEIGHT_POINTS)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::playlist_runtime::{
        NavigationControlAvailability, PlaylistGlobalTransportStatus, PlaylistTransportUiModel,
    };
    use crate::ui::player_controls::{BottomControlsInput, render_bottom_controls};
    use crate::ui::skin::MinimalSkin;
    use crate::ui::test_frame::run_ui_frame;
    use crate::ui::timeline::TimelineUiState;
    use player_core::PlayerSnapshot;
    use playlist_core::RepeatMode;

    /// Измеряет, сколько вертикального места занимает нижняя панель при ширине окна.
    fn measured_bottom_panel_height(
        width: f32,
        global_status: Option<PlaylistGlobalTransportStatus>,
    ) -> f32 {
        let screen_height = 600.0;
        let snapshot = PlayerSnapshot::empty();
        let mut timeline_state = TimelineUiState::default();
        let transport = PlaylistTransportUiModel {
            playlist_view_revision: 1,
            previous: NavigationControlAvailability::Disabled,
            next: NavigationControlAvailability::Disabled,
            repeat_mode: RepeatMode::StopAtEnd,
            shuffle_enabled: false,
            queue_modes_enabled: true,
            global_status,
        };
        let mut remaining_height = screen_height;
        let input = egui::RawInput {
            screen_rect: Some(egui::Rect::from_min_size(
                egui::Pos2::ZERO,
                egui::vec2(width, screen_height),
            )),
            ..Default::default()
        };
        // Панель egui узнаёт свою высоту после первого прохода содержимого, поэтому
        // измеряем на втором кадре того же контекста.
        let egui_ctx = egui::Context::default();
        // Дорожек нет: CC не показывается, высота панели не зависит от субтитров.
        let subtitle_menu =
            crate::state::SubtitleMenuView::empty(crate::state::SubtitleVisibility::Shown);
        let audio_menu = crate::state::AudioTrackMenuView::default();
        for _frame in 0..2 {
            run_ui_frame(&egui_ctx, input.clone(), |ui| {
                let _actions = render_bottom_controls(
                    ui,
                    BottomControlsInput {
                        player_snapshot: &snapshot,
                        timeline_state: &mut timeline_state,
                        timeline_inline_status: None,
                        skin: &MinimalSkin,
                        is_window_fullscreen: false,
                        live_scrub_enabled: false,
                        reduced_motion: false,
                        playlist_transport: &transport,
                        subtitle_menu: &subtitle_menu,
                        audio_menu: &audio_menu,
                        view_profile: crate::window_view_profile::WindowViewProfile::Standard,
                    },
                );
                remaining_height = ui.available_height();
            });
        }
        screen_height - remaining_height
    }

    #[test]
    fn bottom_controls_fit_into_reserved_height() {
        let plain = measured_bottom_panel_height(MIN_WINDOW_WIDTH_POINTS, None);
        let with_status = measured_bottom_panel_height(
            MIN_WINDOW_WIDTH_POINTS,
            Some(PlaylistGlobalTransportStatus::WaitingAutomatic),
        );
        assert!(plain > 0.0, "panel should take up space");
        assert!(
            with_status <= BOTTOM_PANEL_RESERVED_HEIGHT_POINTS,
            "bottom panel ({with_status}) above the reserved"
        );
    }

    #[test]
    fn minimum_height_is_not_degenerate() {
        let size = min_inner_size();
        assert_eq!(size.width, 400.0);
        assert!(
            size.height >= DEFAULT_TITLEBAR_HEIGHT_POINTS + BOTTOM_PANEL_RESERVED_HEIGHT_POINTS
        );
        assert!(size.height > 1.0);
    }
}
