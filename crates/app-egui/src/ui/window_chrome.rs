//! Кастомный window chrome поверх видео.
//!
//! Модуль остаётся визуальным boundary: он знает только egui geometry/style,
//! возвращает намерения пользователя и не вызывает winit lifecycle напрямую.

use egui::{Color32, CursorIcon, Rect, Sense, Stroke, Ui, pos2};
use ui_artwork_egui::{ArtworkPainter, ButtonVisualState, WindowControlGlyph, WindowControlStyle};

use crate::state::SidebarSection;
use crate::ui::skin::ControlsStyle;
use crate::ui::titlebar_icon_area::{self, TitlebarIconAreaAction, TitlebarIconAreaStyle};

mod geometry;

pub(crate) use geometry::WindowChromeEdgeAlignment;
use geometry::{WindowChromeLayout, titlebar_button_block_rect, titlebar_rect_for_window};

/// Толщина невидимых resize-зон вдоль прямых краёв окна.
const RESIZE_EDGE_POINTS: f32 = 6.0;

/// Размер квадратной corner-зоны, где resize идёт сразу по двум осям.
const RESIZE_CORNER_POINTS: f32 = 12.0;

/// Входные данные визуального titlebar.
#[derive(Debug, Clone, Copy)]
pub(crate) struct WindowChromeInput<'title> {
    /// Текст в центре titlebar.
    pub(crate) title: &'title str,

    /// Высота titlebar в egui points.
    pub(crate) height_points: f32,

    /// Текущее maximize-состояние окна, чтобы выбрать icon restore/maximize.
    pub(crate) is_maximized: bool,

    /// Цвета и stroke-и, полученные от текущего UI skin-а.
    pub(crate) style: WindowChromeStyle,

    /// Общие с нижними крайними кнопками горизонтальные оси.
    pub(crate) edge_alignment: WindowChromeEdgeAlignment,

    /// Секция, для которой titlebar показывает постоянную active-заливку.
    pub(crate) active_sidebar_section: Option<SidebarSection>,
}

/// Визуальный стиль кастомного titlebar.
#[derive(Debug, Clone, Copy, PartialEq)]
pub(crate) struct WindowChromeStyle {
    /// Полупрозрачная заливка overlay-панели.
    pub(crate) fill: Color32,

    /// Цвет title text.
    pub(crate) title_color: Color32,

    /// Stroke светлых system icons.
    pub(crate) icon_stroke: Stroke,

    /// Hover-заливка обычных кнопок.
    pub(crate) button_hover_fill: Color32,

    /// Hover-заливка кнопки close.
    pub(crate) close_hover_fill: Color32,
}

impl WindowChromeStyle {
    /// Строит chrome style из текущего player skin-а, не раскрывая skin в модуль окна.
    #[must_use]
    pub(crate) fn from_controls_style(controls_style: ControlsStyle) -> Self {
        Self {
            fill: controls_style.top_panel_fill,
            title_color: controls_style.text_color,
            icon_stroke: Stroke::new(1.6, controls_style.text_color),
            button_hover_fill: Color32::from_rgba_unmultiplied(255, 255, 255, 28),
            close_hover_fill: Color32::from_rgb(196, 43, 43),
        }
    }
}

/// Намерение, которое shell применит через winit boundary после egui frame-а.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum WindowChromeAction {
    /// Свернуть окно.
    Minimize,

    /// Переключить maximize/restore.
    ToggleMaximize,

    /// Закрыть окно через общий cleanup path shell-а.
    Close,

    /// Начать системный drag окна.
    StartDrag,

    /// Начать системный resize в заданном направлении.
    BeginResize(WindowChromeResizeDirection),
}

/// Действия, собранные всем titlebar boundary за один egui frame.
#[derive(Debug, Clone, PartialEq)]
pub(crate) struct WindowChromeOutput {
    /// Window/lifecycle actions, которые shell применяет через winit.
    pub(crate) window_actions: Vec<WindowChromeAction>,

    /// Actions левой icon-area, которые app state мапит в runtime-specific intents.
    pub(crate) titlebar_icon_actions: Vec<TitlebarIconAreaAction>,
}

/// Направление resize без зависимости визуального слоя от `winit::window::ResizeDirection`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub(crate) enum WindowChromeResizeDirection {
    /// Тянуть верхний край.
    North,
    /// Тянуть верхний правый угол.
    NorthEast,
    /// Тянуть правый край.
    East,
    /// Тянуть нижний правый угол.
    SouthEast,
    /// Тянуть нижний край.
    South,
    /// Тянуть нижний левый угол.
    SouthWest,
    /// Тянуть левый край.
    West,
    /// Тянуть верхний левый угол.
    NorthWest,
}

impl WindowChromeResizeDirection {
    /// Возвращает egui cursor, который соответствует системному направлению resize.
    #[must_use]
    fn cursor_icon(self) -> CursorIcon {
        match self {
            Self::East | Self::West => CursorIcon::ResizeHorizontal,
            Self::North | Self::South => CursorIcon::ResizeVertical,
            Self::NorthEast | Self::SouthWest => CursorIcon::ResizeNeSw,
            Self::NorthWest | Self::SouthEast => CursorIcon::ResizeNwSe,
        }
    }
}

/// Тип кнопки titlebar.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
enum WindowChromeButtonKind {
    /// Minimize.
    Minimize,
    /// Maximize или restore.
    MaximizeRestore,
    /// Close.
    Close,
}

impl WindowChromeButtonKind {
    /// Возвращает action, который соответствует кнопке.
    #[must_use]
    fn action(self) -> WindowChromeAction {
        match self {
            Self::Minimize => WindowChromeAction::Minimize,
            Self::MaximizeRestore => WindowChromeAction::ToggleMaximize,
            Self::Close => WindowChromeAction::Close,
        }
    }
}

/// Рисует titlebar и resize hit-zones, возвращая только действия пользователя.
#[must_use]
pub(crate) fn show(ui: &mut Ui, input: WindowChromeInput<'_>) -> WindowChromeOutput {
    let window_rect = ui.max_rect();
    let mut output = WindowChromeOutput {
        window_actions: resize_actions(
            ui,
            window_rect,
            input.height_points,
            input.is_maximized,
            input.edge_alignment,
        ),
        titlebar_icon_actions: Vec::new(),
    };

    egui::Panel::top(titlebar_panel_id())
        .exact_size(input.height_points)
        .frame(egui::Frame::NONE.fill(input.style.fill))
        .show_inside(ui, |ui| {
            let chrome_rect = ui.max_rect();
            let first_button_center_x = input.edge_alignment.left_axis_x(chrome_rect);
            let icon_output = titlebar_icon_area::show(
                ui,
                chrome_rect,
                first_button_center_x,
                TitlebarIconAreaStyle {
                    icon_stroke: input.style.icon_stroke,
                    button_hover_fill: input.style.button_hover_fill,
                },
                input.active_sidebar_section,
            );
            let layout = WindowChromeLayout::new(
                chrome_rect,
                icon_output.reserved_rect,
                input.edge_alignment,
            );
            output.titlebar_icon_actions.extend(icon_output.actions);
            paint_title(ui, input.title, input.style, layout.title_rect);
            collect_drag_actions(ui, layout, input.is_maximized, &mut output.window_actions);
            collect_button_action(
                ui,
                layout.minimize_button_rect,
                WindowChromeButtonKind::Minimize,
                input,
                &mut output.window_actions,
            );
            collect_button_action(
                ui,
                layout.maximize_button_rect,
                WindowChromeButtonKind::MaximizeRestore,
                input,
                &mut output.window_actions,
            );
            collect_button_action(
                ui,
                layout.close_button_rect,
                WindowChromeButtonKind::Close,
                input,
                &mut output.window_actions,
            );
        });

    output
}

fn paint_title(ui: &Ui, title: &str, style: WindowChromeStyle, title_rect: Rect) {
    ArtworkPainter::new(ui.painter()).window_title(title_rect, title, style.title_color);
}

/// Собирает drag/double-click actions из всей свободной части titlebar.
fn collect_drag_actions(
    ui: &mut Ui,
    layout: WindowChromeLayout,
    is_maximized: bool,
    actions: &mut Vec<WindowChromeAction>,
) {
    let Some(drag_rect) = layout.drag_rect else {
        return;
    };
    let Some(active_drag_rect) = drag_interaction_rect(drag_rect, is_maximized) else {
        return;
    };

    let response = ui
        .interact(
            active_drag_rect,
            ui.id().with("window_chrome_drag"),
            Sense::click_and_drag(),
        )
        .on_hover_and_drag_cursor(CursorIcon::Grab);

    if response.double_clicked() {
        actions.push(WindowChromeAction::ToggleMaximize);
    } else if primary_pressed_on(ui, &response) {
        actions.push(WindowChromeAction::StartDrag);
    }
}

/// Сужает drag-zone так, чтобы resize border оставался владельцем краёв окна.
fn drag_interaction_rect(drag_rect: Rect, is_maximized: bool) -> Option<Rect> {
    if is_maximized {
        return Some(drag_rect);
    }

    let drag_left = drag_rect.left() + RESIZE_CORNER_POINTS;
    let drag_top = drag_rect.top() + RESIZE_EDGE_POINTS;

    (drag_rect.right() > drag_left && drag_rect.bottom() > drag_top)
        .then(|| Rect::from_min_max(pos2(drag_left, drag_top), drag_rect.max))
}

/// Регистрирует кнопку и добавляет action при primary click.
fn collect_button_action(
    ui: &mut Ui,
    button_rect: Rect,
    button_kind: WindowChromeButtonKind,
    input: WindowChromeInput<'_>,
    actions: &mut Vec<WindowChromeAction>,
) {
    let response = ui.interact(
        button_rect,
        ui.id().with(("window_chrome_button", button_kind)),
        Sense::click(),
    );
    paint_button(ui, button_rect, button_kind, input, response.hovered());

    if response.clicked() {
        actions.push(button_kind.action());
    }
}

fn paint_button(
    ui: &Ui,
    button_rect: Rect,
    button_kind: WindowChromeButtonKind,
    input: WindowChromeInput<'_>,
    hovered: bool,
) {
    let glyph = match button_kind {
        WindowChromeButtonKind::Minimize => WindowControlGlyph::Minimize,
        WindowChromeButtonKind::MaximizeRestore if input.is_maximized => {
            WindowControlGlyph::Restore
        }
        WindowChromeButtonKind::MaximizeRestore => WindowControlGlyph::Maximize,
        WindowChromeButtonKind::Close => WindowControlGlyph::Close,
    };
    let hover_fill = if button_kind == WindowChromeButtonKind::Close {
        input.style.close_hover_fill
    } else {
        input.style.button_hover_fill
    };
    ArtworkPainter::new(ui.painter()).window_control(
        button_rect,
        glyph,
        if hovered {
            ButtonVisualState::Hovered
        } else {
            ButtonVisualState::Idle
        },
        WindowControlStyle {
            fill: input.style.fill,
            stroke: input.style.icon_stroke,
            hover_fill,
        },
    );
}

/// Собирает resize actions из невидимых зон вокруг окна.
fn resize_actions(
    ui: &mut Ui,
    window_rect: Rect,
    titlebar_height_points: f32,
    is_maximized: bool,
    edge_alignment: WindowChromeEdgeAlignment,
) -> Vec<WindowChromeAction> {
    if is_maximized
        || pointer_is_over_titlebar_interactive_block(
            ui,
            window_rect,
            titlebar_height_points,
            edge_alignment,
        )
    {
        return Vec::new();
    }

    resize_zone_rects(window_rect)
        .into_iter()
        .filter_map(|(direction, rect)| {
            let response = ui
                .interact(
                    rect,
                    ui.id().with(("window_chrome_resize", direction)),
                    Sense::click_and_drag(),
                )
                .on_hover_and_drag_cursor(direction.cursor_icon());

            primary_pressed_on(ui, &response).then_some(WindowChromeAction::BeginResize(direction))
        })
        .collect()
}

/// Не даёт невидимым resize-зонам перехватывать hover/click интерактивных блоков titlebar.
fn pointer_is_over_titlebar_interactive_block(
    ui: &Ui,
    window_rect: Rect,
    titlebar_height_points: f32,
    edge_alignment: WindowChromeEdgeAlignment,
) -> bool {
    let Some(pointer_position) = ui.input(|input| input.pointer.hover_pos()) else {
        return false;
    };

    pointer_position_is_over_titlebar_interactive_block(
        window_rect,
        titlebar_height_points,
        edge_alignment,
        pointer_position,
    )
}

/// Pure hit-test для интерактивных titlebar блоков, чтобы resize guard был тестируемым.
fn pointer_position_is_over_titlebar_interactive_block(
    window_rect: Rect,
    titlebar_height_points: f32,
    edge_alignment: WindowChromeEdgeAlignment,
    pointer_position: egui::Pos2,
) -> bool {
    titlebar_interactive_block_rects(window_rect, titlebar_height_points, edge_alignment)
        .into_iter()
        .any(|block_rect| block_rect.contains(pointer_position))
}

/// Возвращает left icon-area и правый блок системных кнопок как guarded hit-rects.
fn titlebar_interactive_block_rects(
    window_rect: Rect,
    titlebar_height_points: f32,
    edge_alignment: WindowChromeEdgeAlignment,
) -> [Rect; 2] {
    let titlebar_rect = titlebar_rect_for_window(window_rect, titlebar_height_points);
    let first_button_center_x = edge_alignment.left_axis_x(titlebar_rect);

    [
        titlebar_icon_area::button_group_rect(titlebar_rect, first_button_center_x),
        titlebar_button_block_rect(titlebar_rect, edge_alignment),
    ]
}

/// Возвращает `true`, когда primary mouse был нажат на этом widget в текущем frame-е.
fn primary_pressed_on(ui: &Ui, response: &egui::Response) -> bool {
    response.hovered()
        && ui.input(|input| input.pointer.button_pressed(egui::PointerButton::Primary))
}

/// Строит resize hit-зоны от наиболее специфичных углов к прямым краям.
fn resize_zone_rects(window_rect: Rect) -> [(WindowChromeResizeDirection, Rect); 8] {
    let left = window_rect.left();
    let right = window_rect.right();
    let top = window_rect.top();
    let bottom = window_rect.bottom();
    let edge = RESIZE_EDGE_POINTS;
    let corner = RESIZE_CORNER_POINTS;

    [
        (
            WindowChromeResizeDirection::NorthWest,
            Rect::from_min_max(pos2(left, top), pos2(left + corner, top + corner)),
        ),
        (
            WindowChromeResizeDirection::NorthEast,
            Rect::from_min_max(pos2(right - corner, top), pos2(right, top + corner)),
        ),
        (
            WindowChromeResizeDirection::SouthEast,
            Rect::from_min_max(pos2(right - corner, bottom - corner), pos2(right, bottom)),
        ),
        (
            WindowChromeResizeDirection::SouthWest,
            Rect::from_min_max(pos2(left, bottom - corner), pos2(left + corner, bottom)),
        ),
        (
            WindowChromeResizeDirection::North,
            Rect::from_min_max(pos2(left + corner, top), pos2(right - corner, top + edge)),
        ),
        (
            WindowChromeResizeDirection::East,
            Rect::from_min_max(
                pos2(right - edge, top + corner),
                pos2(right, bottom - corner),
            ),
        ),
        (
            WindowChromeResizeDirection::South,
            Rect::from_min_max(
                pos2(left + corner, bottom - edge),
                pos2(right - corner, bottom),
            ),
        ),
        (
            WindowChromeResizeDirection::West,
            Rect::from_min_max(pos2(left, top + corner), pos2(left + edge, bottom - corner)),
        ),
    ]
}

/// Pure hit-test для тестов: выбирает resize direction по координате pointer-а.
#[cfg(test)]
#[must_use]
fn resize_direction_at(
    window_rect: Rect,
    pointer_position: egui::Pos2,
) -> Option<WindowChromeResizeDirection> {
    resize_zone_rects(window_rect)
        .into_iter()
        .find_map(|(direction, rect)| rect.contains(pointer_position).then_some(direction))
}

/// Stable id панели, чтобы egui не переносил state между разными overlay-слоями.
fn titlebar_panel_id() -> &'static str {
    "window_chrome_titlebar"
}

#[cfg(test)]
mod tests {
    use super::*;
    use egui::{Pos2, vec2};

    use crate::ui::skin::{MinimalSkin, PlayerSkin};

    fn test_style() -> WindowChromeStyle {
        WindowChromeStyle {
            fill: Color32::from_rgba_unmultiplied(0, 0, 0, 145),
            title_color: Color32::WHITE,
            icon_stroke: Stroke::new(1.0, Color32::WHITE),
            button_hover_fill: Color32::from_gray(32),
            close_hover_fill: Color32::from_rgb(196, 43, 43),
        }
    }

    fn test_edge_alignment() -> WindowChromeEdgeAlignment {
        WindowChromeEdgeAlignment::from_controls_style(MinimalSkin.controls_style())
    }

    fn test_layout(chrome_rect: Rect) -> WindowChromeLayout {
        let edge_alignment = test_edge_alignment();
        let first_button_center_x = edge_alignment.left_axis_x(chrome_rect);

        WindowChromeLayout::new(
            chrome_rect,
            titlebar_icon_area::reserved_rect(chrome_rect, first_button_center_x),
            edge_alignment,
        )
    }

    #[test]
    fn drag_rect_covers_titlebar_without_stealing_buttons_or_resize_edges() {
        let chrome_rect = Rect::from_min_size(Pos2::ZERO, vec2(1000.0, 40.0));
        let layout = test_layout(chrome_rect);
        let drag_rect = layout
            .drag_rect
            .expect("wide titlebar should have drag rect before buttons");
        let active_drag_rect = drag_interaction_rect(drag_rect, false)
            .expect("normal titlebar should keep a drag rect after resize insets");
        let maximized_drag_rect = drag_interaction_rect(drag_rect, true)
            .expect("maximized titlebar should keep the full drag rect");

        assert_eq!(drag_rect.left(), layout.titlebar_icon_reserved_rect.right());
        assert_eq!(drag_rect.right(), layout.minimize_button_rect.left());
        assert!(drag_rect.contains(layout.title_rect.center()));
        assert!(!drag_rect.contains(layout.titlebar_icon_reserved_rect.center()));
        assert!(!drag_rect.contains(layout.minimize_button_rect.center()));
        assert!(!active_drag_rect.contains(pos2(20.0, 2.0)));
        assert!(!active_drag_rect.contains(pos2(2.0, 20.0)));
        assert!(active_drag_rect.contains(pos2(chrome_rect.center().x, 20.0)));
        assert_eq!(maximized_drag_rect, drag_rect);
    }

    #[test]
    fn resize_guard_treats_left_icon_area_as_interactive_titlebar_block() {
        let window_rect = Rect::from_min_size(Pos2::ZERO, vec2(800.0, 600.0));
        let titlebar_height_points = 40.0;
        let edge_alignment = test_edge_alignment();
        let titlebar_rect = titlebar_rect_for_window(window_rect, titlebar_height_points);
        let first_button_center_x = edge_alignment.left_axis_x(titlebar_rect);
        let left_icon_group =
            titlebar_icon_area::button_group_rect(titlebar_rect, first_button_center_x);
        let first_icon_rect =
            titlebar_icon_area::button_rect(titlebar_rect, first_button_center_x, 0);
        let last_icon_rect =
            titlebar_icon_area::button_rect(titlebar_rect, first_button_center_x, 3);
        let right_button_block = titlebar_button_block_rect(titlebar_rect, edge_alignment);

        assert!(pointer_position_is_over_titlebar_interactive_block(
            window_rect,
            titlebar_height_points,
            edge_alignment,
            first_icon_rect.center()
        ));
        assert!(pointer_position_is_over_titlebar_interactive_block(
            window_rect,
            titlebar_height_points,
            edge_alignment,
            last_icon_rect.center()
        ));
        assert!(pointer_position_is_over_titlebar_interactive_block(
            window_rect,
            titlebar_height_points,
            edge_alignment,
            right_button_block.center()
        ));
        assert!(!pointer_position_is_over_titlebar_interactive_block(
            window_rect,
            titlebar_height_points,
            edge_alignment,
            pos2(window_rect.center().x, titlebar_height_points * 0.5)
        ));
        assert!(!left_icon_group.contains(window_rect.left_top()));
        assert!(!pointer_position_is_over_titlebar_interactive_block(
            window_rect,
            titlebar_height_points,
            edge_alignment,
            window_rect.left_top()
        ));
    }

    #[test]
    fn resize_direction_hit_test_prefers_corners_over_edges() {
        let window_rect = Rect::from_min_size(Pos2::ZERO, vec2(800.0, 600.0));

        assert_eq!(
            resize_direction_at(window_rect, pos2(2.0, 2.0)),
            Some(WindowChromeResizeDirection::NorthWest)
        );
        assert_eq!(
            resize_direction_at(window_rect, pos2(798.0, 2.0)),
            Some(WindowChromeResizeDirection::NorthEast)
        );
        assert_eq!(
            resize_direction_at(window_rect, pos2(400.0, 598.0)),
            Some(WindowChromeResizeDirection::South)
        );
        assert_eq!(resize_direction_at(window_rect, pos2(400.0, 300.0)), None);
    }

    #[test]
    fn buttons_map_to_window_chrome_actions() {
        assert_eq!(
            WindowChromeButtonKind::Minimize.action(),
            WindowChromeAction::Minimize
        );
        assert_eq!(
            WindowChromeButtonKind::MaximizeRestore.action(),
            WindowChromeAction::ToggleMaximize
        );
        assert_eq!(
            WindowChromeButtonKind::Close.action(),
            WindowChromeAction::Close
        );

        let style = test_style();
        assert_eq!(style.close_hover_fill, Color32::from_rgb(196, 43, 43));
    }
}
