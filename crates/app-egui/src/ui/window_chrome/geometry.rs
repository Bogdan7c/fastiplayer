//! Чистая геометрия custom titlebar без egui interaction и window actions.

use egui::{Rect, pos2, vec2};

use crate::ui::skin::ControlsStyle;
use crate::ui::titlebar_icon_area::TitlebarIconAreaAlignment;

/// Ширина каждой системной кнопки titlebar в логических UI points.
const TITLEBAR_BUTTON_WIDTH_POINTS: f32 = 46.0;

/// Минимальный зазор между текстом title и соседними интерактивными зонами.
const TITLE_TEXT_GAP_POINTS: f32 = 12.0;

/// Оси крайних titlebar-кнопок относительно краёв всего окна.
///
/// Insets вычисляются из геометрии нижней панели, чтобы первая левая кнопка
/// совпадала с Open, а Close — с Fullscreen при любом поддерживаемом skin-е.
#[derive(Debug, Clone, Copy, PartialEq)]
pub(crate) struct WindowChromeEdgeAlignment {
    /// Расстояние от левого края окна до центра первой titlebar-кнопки.
    left_axis_inset_points: f32,

    /// Шаг общей сетки центров titlebar и playlist toolbar.
    left_axis_center_step_points: f32,

    /// Расстояние от правого края окна до центра Close.
    right_axis_inset_points: f32,
}

impl WindowChromeEdgeAlignment {
    /// Строит общие edge-axis из authoritative геометрии нижней панели.
    #[must_use]
    pub(crate) fn from_controls_style(controls_style: ControlsStyle) -> Self {
        let content_edge_offset = controls_style.bottom_edge_button_center_offset_points();

        Self {
            left_axis_inset_points: controls_style.left_edge_control_first_center_inset_points(),
            left_axis_center_step_points: controls_style.left_edge_control_center_step,
            right_axis_inset_points: controls_style.panel_margin.rightf() + content_edge_offset,
        }
    }

    /// Возвращает абсолютную сетку левых titlebar-кнопок внутри окна.
    #[must_use]
    pub(super) fn left_icon_alignment(self, window_rect: Rect) -> TitlebarIconAreaAlignment {
        TitlebarIconAreaAlignment::new(
            window_rect.left() + self.left_axis_inset_points,
            self.left_axis_center_step_points,
        )
    }

    /// Возвращает абсолютную X-координату правой оси внутри окна.
    #[must_use]
    fn right_axis_x(self, window_rect: Rect) -> f32 {
        window_rect.right() - self.right_axis_inset_points
    }
}

/// Прямоугольники titlebar, вычисленные без side effects для тестируемости.
#[derive(Debug, Clone, Copy, PartialEq)]
pub(super) struct WindowChromeLayout {
    /// Левая icon-area, зарезервированная под переключатели sidebar.
    pub(super) titlebar_icon_reserved_rect: Rect,

    /// Rect строго центрированного title text.
    pub(super) title_rect: Rect,

    /// Кнопка minimize.
    pub(super) minimize_button_rect: Rect,

    /// Кнопка maximize/restore.
    pub(super) maximize_button_rect: Rect,

    /// Кнопка close.
    pub(super) close_button_rect: Rect,

    /// Зона перетаскивания titlebar без блока системных кнопок справа.
    pub(super) drag_rect: Option<Rect>,
}

impl WindowChromeLayout {
    /// Вычисляет layout так, чтобы title оставался в центре всего окна.
    #[must_use]
    pub(super) fn new(
        chrome_rect: Rect,
        titlebar_icon_reserved_rect: Rect,
        edge_alignment: WindowChromeEdgeAlignment,
    ) -> Self {
        let [
            minimize_button_rect,
            maximize_button_rect,
            close_button_rect,
        ] = window_control_button_rects(chrome_rect, edge_alignment);
        let left_reserved_side_width = titlebar_icon_reserved_rect.width() + TITLE_TEXT_GAP_POINTS;
        let right_reserved_side_width =
            chrome_rect.right() - minimize_button_rect.left() + TITLE_TEXT_GAP_POINTS;
        let reserved_side_width = left_reserved_side_width
            .max(right_reserved_side_width)
            .min(chrome_rect.width());
        let title_width = (chrome_rect.width() - reserved_side_width * 2.0).max(0.0);
        let title_rect = Rect::from_center_size(
            chrome_rect.center(),
            vec2(title_width, chrome_rect.height()),
        );
        let drag_rect = optional_rect(
            titlebar_icon_reserved_rect.right(),
            minimize_button_rect.left(),
            chrome_rect,
        );

        Self {
            titlebar_icon_reserved_rect,
            title_rect,
            minimize_button_rect,
            maximize_button_rect,
            close_button_rect,
            drag_rect,
        }
    }
}

/// Возвращает titlebar rect внутри window rect с учётом configured height.
pub(super) fn titlebar_rect_for_window(window_rect: Rect, titlebar_height_points: f32) -> Rect {
    Rect::from_min_max(
        window_rect.min,
        pos2(
            window_rect.right(),
            (window_rect.top() + titlebar_height_points).min(window_rect.bottom()),
        ),
    )
}

/// Возвращает общий hit-rect для Minimize/Maximize/Close.
pub(super) fn titlebar_button_block_rect(
    titlebar_rect: Rect,
    edge_alignment: WindowChromeEdgeAlignment,
) -> Rect {
    let [minimize_button_rect, _, close_button_rect] =
        window_control_button_rects(titlebar_rect, edge_alignment);

    Rect::from_min_max(minimize_button_rect.min, close_button_rect.max)
}

/// Возвращает contiguous rects системной группы в порядке Minimize/Maximize/Close.
fn window_control_button_rects(
    chrome_rect: Rect,
    edge_alignment: WindowChromeEdgeAlignment,
) -> [Rect; 3] {
    let button_size = vec2(TITLEBAR_BUTTON_WIDTH_POINTS, chrome_rect.height());
    let close_button_rect = Rect::from_center_size(
        pos2(
            edge_alignment.right_axis_x(chrome_rect),
            chrome_rect.center().y,
        ),
        button_size,
    );
    let maximize_button_rect =
        close_button_rect.translate(vec2(-TITLEBAR_BUTTON_WIDTH_POINTS, 0.0));
    let minimize_button_rect =
        maximize_button_rect.translate(vec2(-TITLEBAR_BUTTON_WIDTH_POINTS, 0.0));

    [
        minimize_button_rect,
        maximize_button_rect,
        close_button_rect,
    ]
}

/// Возвращает непустой горизонтальный rect внутри titlebar.
fn optional_rect(left: f32, right: f32, chrome_rect: Rect) -> Option<Rect> {
    (right > left).then(|| {
        Rect::from_min_max(
            pos2(left, chrome_rect.top()),
            pos2(right, chrome_rect.bottom()),
        )
    })
}

#[cfg(test)]
mod tests {
    use egui::Pos2;

    use super::*;
    use crate::ui::skin::{MinimalSkin, PlayerSkin};
    use crate::ui::titlebar_icon_area;

    fn test_edge_alignment() -> WindowChromeEdgeAlignment {
        WindowChromeEdgeAlignment::from_controls_style(MinimalSkin.controls_style())
    }

    fn test_layout(chrome_rect: Rect) -> WindowChromeLayout {
        let edge_alignment = test_edge_alignment();
        let icon_alignment = edge_alignment.left_icon_alignment(chrome_rect);

        WindowChromeLayout::new(
            chrome_rect,
            titlebar_icon_area::reserved_rect(chrome_rect, icon_alignment),
            edge_alignment,
        )
    }

    #[test]
    fn title_rect_stays_centered_relative_to_the_whole_window() {
        let chrome_rect = Rect::from_min_size(Pos2::ZERO, vec2(1000.0, 40.0));
        let layout = test_layout(chrome_rect);

        assert_eq!(layout.title_rect.center().x, chrome_rect.center().x);
        assert!(layout.title_rect.right() < layout.minimize_button_rect.left());
        assert!(
            layout
                .drag_rect
                .expect("wide titlebar should have drag rect")
                .contains(layout.title_rect.center())
        );
    }

    #[test]
    fn button_groups_follow_bottom_control_axes_and_keep_spacing() {
        let chrome_rect = Rect::from_min_size(Pos2::ZERO, vec2(1000.0, 40.0));
        let edge_alignment = test_edge_alignment();
        let icon_alignment = edge_alignment.left_icon_alignment(chrome_rect);
        let left_button_rects = [0, 1, 2, 3]
            .map(|index| titlebar_icon_area::button_rect(chrome_rect, icon_alignment, index));
        let layout = test_layout(chrome_rect);

        assert_eq!(
            left_button_rects[0].center().x,
            chrome_rect.left() + edge_alignment.left_axis_inset_points
        );
        assert!(left_button_rects.windows(2).all(|button_pair| {
            button_pair[1].center().x - button_pair[0].center().x
                == edge_alignment.left_axis_center_step_points
        }));
        assert_eq!(
            layout.close_button_rect.center().x,
            edge_alignment.right_axis_x(chrome_rect)
        );
        assert_eq!(
            layout.minimize_button_rect.right(),
            layout.maximize_button_rect.left()
        );
        assert_eq!(
            layout.maximize_button_rect.right(),
            layout.close_button_rect.left()
        );
        assert!(
            [
                layout.minimize_button_rect,
                layout.maximize_button_rect,
                layout.close_button_rect,
            ]
            .into_iter()
            .all(|button_rect| button_rect.width() == TITLEBAR_BUTTON_WIDTH_POINTS)
        );
        assert!(layout.close_button_rect.right() < chrome_rect.right());
    }
}
