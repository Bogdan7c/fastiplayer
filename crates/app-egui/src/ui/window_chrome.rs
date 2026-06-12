//! Кастомный window chrome поверх видео.
//!
//! Модуль остаётся визуальным boundary: он знает только egui geometry/style,
//! возвращает намерения пользователя и не вызывает winit lifecycle напрямую.

use egui::{
    Align2, Color32, CursorIcon, FontId, Rect, Sense, Stroke, StrokeKind, Ui, Vec2, pos2, vec2,
};

use crate::ui::skin::ControlsStyle;

/// Ширина каждой системной кнопки titlebar в логических UI points.
const TITLEBAR_BUTTON_WIDTH_POINTS: f32 = 46.0;

/// Толщина невидимых resize-зон вдоль прямых краёв окна.
const RESIZE_EDGE_POINTS: f32 = 6.0;

/// Размер квадратной corner-зоны, где resize идёт сразу по двум осям.
const RESIZE_CORNER_POINTS: f32 = 12.0;

/// Минимальный зазор между текстом title и соседними интерактивными зонами.
const TITLE_TEXT_GAP_POINTS: f32 = 12.0;

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

/// Прямоугольники titlebar, вычисленные без side effects для тестируемости.
#[derive(Debug, Clone, Copy, PartialEq)]
struct WindowChromeLayout {
    /// Rect строго центрированного title text.
    title_rect: Rect,
    /// Кнопка minimize.
    minimize_button_rect: Rect,
    /// Кнопка maximize/restore.
    maximize_button_rect: Rect,
    /// Кнопка close.
    close_button_rect: Rect,
    /// Пустая зона слева от title.
    left_drag_rect: Option<Rect>,
    /// Пустая зона справа от title до кнопок.
    right_drag_rect: Option<Rect>,
}

/// Рисует titlebar и resize hit-zones, возвращая только действия пользователя.
#[must_use]
pub(crate) fn show(ui: &mut Ui, input: WindowChromeInput<'_>) -> Vec<WindowChromeAction> {
    let window_rect = ui.max_rect();
    let mut actions = resize_actions(ui, window_rect, input.height_points, input.is_maximized);

    egui::Panel::top(titlebar_panel_id())
        .exact_size(input.height_points)
        .frame(egui::Frame::NONE.fill(input.style.fill))
        .show_inside(ui, |ui| {
            let chrome_rect = ui.max_rect();
            let layout = WindowChromeLayout::new(chrome_rect);
            paint_title(ui, input.title, input.style, layout.title_rect);
            collect_drag_actions(ui, layout, &mut actions);
            collect_button_action(
                ui,
                layout.minimize_button_rect,
                WindowChromeButtonKind::Minimize,
                input,
                &mut actions,
            );
            collect_button_action(
                ui,
                layout.maximize_button_rect,
                WindowChromeButtonKind::MaximizeRestore,
                input,
                &mut actions,
            );
            collect_button_action(
                ui,
                layout.close_button_rect,
                WindowChromeButtonKind::Close,
                input,
                &mut actions,
            );
        });

    actions
}

impl WindowChromeLayout {
    /// Вычисляет layout так, чтобы title оставался в центре всего окна, а не центра свободного места.
    #[must_use]
    fn new(chrome_rect: Rect) -> Self {
        let button_size = vec2(TITLEBAR_BUTTON_WIDTH_POINTS, chrome_rect.height());
        let close_button_rect = Rect::from_min_size(
            pos2(
                chrome_rect.right() - TITLEBAR_BUTTON_WIDTH_POINTS,
                chrome_rect.top(),
            ),
            button_size,
        );
        let maximize_button_rect =
            close_button_rect.translate(vec2(-TITLEBAR_BUTTON_WIDTH_POINTS, 0.0));
        let minimize_button_rect =
            maximize_button_rect.translate(vec2(-TITLEBAR_BUTTON_WIDTH_POINTS, 0.0));
        let reserved_side_width =
            (TITLEBAR_BUTTON_WIDTH_POINTS * 3.0 + TITLE_TEXT_GAP_POINTS).min(chrome_rect.width());
        let title_width = (chrome_rect.width() - reserved_side_width * 2.0).max(0.0);
        let title_rect = Rect::from_center_size(
            chrome_rect.center(),
            vec2(title_width, chrome_rect.height()),
        );
        let left_drag_rect = optional_rect(chrome_rect.left(), title_rect.left(), chrome_rect);
        let right_drag_rect =
            optional_rect(title_rect.right(), minimize_button_rect.left(), chrome_rect);

        Self {
            title_rect,
            minimize_button_rect,
            maximize_button_rect,
            close_button_rect,
            left_drag_rect,
            right_drag_rect,
        }
    }
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

/// Рисует текст title внутри clipping rect-а.
fn paint_title(ui: &Ui, title: &str, style: WindowChromeStyle, title_rect: Rect) {
    let painter = ui.painter().with_clip_rect(title_rect);
    painter.text(
        title_rect.center(),
        Align2::CENTER_CENTER,
        title,
        FontId::proportional(15.0),
        style.title_color,
    );
}

/// Собирает drag/double-click actions из пустых частей titlebar.
fn collect_drag_actions(
    ui: &mut Ui,
    layout: WindowChromeLayout,
    actions: &mut Vec<WindowChromeAction>,
) {
    for (index, drag_rect) in [layout.left_drag_rect, layout.right_drag_rect]
        .into_iter()
        .flatten()
        .enumerate()
    {
        let response = ui
            .interact(
                drag_rect,
                ui.id().with(("window_chrome_drag", index)),
                Sense::click_and_drag(),
            )
            .on_hover_and_drag_cursor(CursorIcon::Grab);

        if response.double_clicked() {
            actions.push(WindowChromeAction::ToggleMaximize);
        } else if primary_pressed_on(ui, &response) {
            actions.push(WindowChromeAction::StartDrag);
        }
    }
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

/// Рисует фон hover-а и stroke-icon кнопки.
fn paint_button(
    ui: &Ui,
    button_rect: Rect,
    button_kind: WindowChromeButtonKind,
    input: WindowChromeInput<'_>,
    hovered: bool,
) {
    let painter = ui.painter();
    if hovered {
        let fill = if button_kind == WindowChromeButtonKind::Close {
            input.style.close_hover_fill
        } else {
            input.style.button_hover_fill
        };
        painter.rect_filled(button_rect, 0.0, fill);
    }

    match button_kind {
        WindowChromeButtonKind::Minimize => paint_minimize_icon(painter, button_rect, input.style),
        WindowChromeButtonKind::MaximizeRestore if input.is_maximized => {
            paint_restore_icon(painter, button_rect, input.style);
        }
        WindowChromeButtonKind::MaximizeRestore => {
            paint_maximize_icon(painter, button_rect, input.style);
        }
        WindowChromeButtonKind::Close => paint_close_icon(painter, button_rect, input.style),
    }
}

/// Рисует minimize icon.
fn paint_minimize_icon(painter: &egui::Painter, button_rect: Rect, style: WindowChromeStyle) {
    let center = button_rect.center();
    painter.line_segment(
        [
            pos2(center.x - 6.0, center.y + 5.0),
            pos2(center.x + 6.0, center.y + 5.0),
        ],
        style.icon_stroke,
    );
}

/// Рисует maximize icon.
fn paint_maximize_icon(painter: &egui::Painter, button_rect: Rect, style: WindowChromeStyle) {
    let icon_rect = Rect::from_center_size(button_rect.center(), Vec2::new(12.0, 10.0));
    painter.rect_stroke(icon_rect, 0.0, style.icon_stroke, StrokeKind::Inside);
}

/// Рисует restore icon для maximized window.
fn paint_restore_icon(painter: &egui::Painter, button_rect: Rect, style: WindowChromeStyle) {
    let center = button_rect.center();
    let back_rect =
        Rect::from_center_size(pos2(center.x + 2.0, center.y - 2.0), Vec2::new(10.0, 8.0));
    let front_rect =
        Rect::from_center_size(pos2(center.x - 2.0, center.y + 2.0), Vec2::new(10.0, 8.0));

    painter.rect_stroke(back_rect, 0.0, style.icon_stroke, StrokeKind::Inside);
    painter.rect_filled(
        Rect::from_min_max(
            pos2(front_rect.left() - 1.0, front_rect.top() - 1.0),
            pos2(front_rect.right() + 1.0, front_rect.bottom() + 1.0),
        ),
        0.0,
        style.fill,
    );
    painter.rect_stroke(front_rect, 0.0, style.icon_stroke, StrokeKind::Inside);
}

/// Рисует close icon.
fn paint_close_icon(painter: &egui::Painter, button_rect: Rect, style: WindowChromeStyle) {
    let center = button_rect.center();
    painter.line_segment(
        [
            pos2(center.x - 5.0, center.y - 5.0),
            pos2(center.x + 5.0, center.y + 5.0),
        ],
        style.icon_stroke,
    );
    painter.line_segment(
        [
            pos2(center.x + 5.0, center.y - 5.0),
            pos2(center.x - 5.0, center.y + 5.0),
        ],
        style.icon_stroke,
    );
}

/// Собирает resize actions из невидимых зон вокруг окна.
fn resize_actions(
    ui: &mut Ui,
    window_rect: Rect,
    titlebar_height_points: f32,
    is_maximized: bool,
) -> Vec<WindowChromeAction> {
    if is_maximized || pointer_is_over_titlebar_buttons(ui, window_rect, titlebar_height_points) {
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

/// Не даёт невидимым resize-зонам перехватывать hover/click системных кнопок справа.
fn pointer_is_over_titlebar_buttons(
    ui: &Ui,
    window_rect: Rect,
    titlebar_height_points: f32,
) -> bool {
    let Some(pointer_position) = ui.input(|input| input.pointer.hover_pos()) else {
        return false;
    };

    titlebar_button_block_rect(window_rect, titlebar_height_points).contains(pointer_position)
}

/// Возвращает общий hit-rect для minimize/maximize/close button block.
fn titlebar_button_block_rect(window_rect: Rect, titlebar_height_points: f32) -> Rect {
    let buttons_left =
        (window_rect.right() - TITLEBAR_BUTTON_WIDTH_POINTS * 3.0).max(window_rect.left());
    let buttons_bottom = (window_rect.top() + titlebar_height_points).min(window_rect.bottom());

    Rect::from_min_max(
        pos2(buttons_left, window_rect.top()),
        pos2(window_rect.right(), buttons_bottom),
    )
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
    use egui::Pos2;

    fn test_style() -> WindowChromeStyle {
        WindowChromeStyle {
            fill: Color32::from_rgba_unmultiplied(0, 0, 0, 145),
            title_color: Color32::WHITE,
            icon_stroke: Stroke::new(1.0, Color32::WHITE),
            button_hover_fill: Color32::from_gray(32),
            close_hover_fill: Color32::from_rgb(196, 43, 43),
        }
    }

    #[test]
    fn title_rect_stays_centered_when_buttons_are_on_the_right() {
        let chrome_rect = Rect::from_min_size(Pos2::ZERO, vec2(1000.0, 40.0));
        let layout = WindowChromeLayout::new(chrome_rect);

        assert_eq!(layout.title_rect.center().x, chrome_rect.center().x);
        assert!(layout.title_rect.right() < layout.minimize_button_rect.left());
        assert_eq!(
            layout.close_button_rect.width(),
            TITLEBAR_BUTTON_WIDTH_POINTS
        );
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
