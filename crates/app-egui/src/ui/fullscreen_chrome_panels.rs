//! Заголовок окна и нижняя панель управления, которые умеют уезжать за край экрана.
//!
//! Тонкие обёртки: панели рисуют их владельцы (`window_chrome`, `player_controls`) без
//! изменений, а сдвиг делает [`super::edge_slide`]. Позицию анимации задаёт владелец
//! автоскрытия [`crate::fullscreen_chrome`]; при `hidden_fraction == 0` раскладка окна
//! та же, что без обёрток.

use egui::Ui;

use super::edge_slide::{self, EdgeSlide, EdgeSlideOutput, ScreenEdge};
use super::player_controls::{self, BottomControlsInput, ControlAction};
use super::skin::PlayerSkin;
use super::window_chrome::{self, WindowChromeInput, WindowChromeOutput};

/// Рисует titlebar, сдвинутый вверх на `hidden_fraction` своей высоты.
pub(crate) fn show_titlebar(
    ui: &mut Ui,
    hidden_fraction: f32,
    input: WindowChromeInput<'_>,
) -> EdgeSlideOutput<WindowChromeOutput> {
    edge_slide::show(
        ui,
        EdgeSlide {
            id: egui::Id::new("fullscreen_chrome_titlebar"),
            edge: ScreenEdge::Top,
            hidden_fraction,
        },
        |ui| window_chrome::show(ui, input),
    )
}

/// Рисует нижнюю панель управления, сдвинутую вниз на `hidden_fraction` своей высоты.
pub(crate) fn show_bottom_controls<S: PlayerSkin>(
    ui: &mut Ui,
    hidden_fraction: f32,
    input: BottomControlsInput<'_, S>,
) -> EdgeSlideOutput<Vec<ControlAction>> {
    edge_slide::show(
        ui,
        EdgeSlide {
            id: egui::Id::new("fullscreen_chrome_bottom_controls"),
            edge: ScreenEdge::Bottom,
            hidden_fraction,
        },
        |ui| player_controls::render_bottom_controls(ui, input),
    )
}
