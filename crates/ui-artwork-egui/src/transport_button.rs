//! Ручная отрисовка кнопок перехода к предыдущему и следующему элементу.

use egui::{Color32, Painter, Rect, Shape, Stroke, pos2};

use crate::ButtonVisualState;

/// Направление transport glyph без зависимости от playlist или player domain.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TransportGlyph {
    /// Перейти к предыдущему элементу.
    Previous,

    /// Перейти к следующему элементу.
    Next,
}

/// Визуальные параметры компактной transport-кнопки.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct TransportButtonStyle {
    /// Полный размер glyph внутри переданного hit-area.
    pub icon_extent: f32,

    /// Ширина вертикального ограничителя glyph.
    pub bar_width: f32,

    /// Цвет ограничителя и треугольника.
    pub color: Color32,

    /// Цвет круглой подложки, видимой только при hover.
    pub hover_fill: Color32,
}

/// Рисует transport glyph относительно центра переданного прямоугольника.
pub(crate) fn paint(
    painter: &Painter,
    rect: Rect,
    glyph: TransportGlyph,
    state: ButtonVisualState,
    style: TransportButtonStyle,
) {
    // Центр hit-area остаётся общей точкой зеркальной геометрии Previous и Next.
    let center = rect.center();
    // Hover-подложка занимает доступный квадрат, но не выходит за узкую сторону rect.
    let hover_radius = rect.width().min(rect.height()) * 0.5;
    // Idle-состояние не получает постоянную рамку или фон.
    if state == ButtonVisualState::Hovered {
        // Полупрозрачный круг повторяет hover-язык остальных hand-drawn controls.
        painter.circle_filled(center, hover_radius, style.hover_fill);
    }

    // Половина extent задаёт общую горизонтальную и вертикальную границу glyph.
    let half_extent = style.icon_extent * 0.5;
    // Знак направления позволяет получить Next точным зеркалом Previous.
    let direction = match glyph {
        // Отрицательный знак направляет треугольник и ограничитель влево.
        TransportGlyph::Previous => -1.0,
        // Положительный знак направляет треугольник и ограничитель вправо.
        TransportGlyph::Next => 1.0,
    };
    // Ограничитель располагается у внешнего края glyph.
    let bar_center_x = center.x + direction * (half_extent - style.bar_width * 0.5);
    // Половина ширины нужна для четырёх точных углов прямоугольного ограничителя.
    let bar_half_width = style.bar_width * 0.5;
    // Ограничитель почти заполняет высоту glyph, сохраняя небольшой оптический отступ.
    let bar_half_height = half_extent * 0.84;
    // Прямоугольник рисуется polygon-ом, чтобы его форма не зависела от corner-radius API.
    painter.add(Shape::convex_polygon(
        vec![
            // Левый верхний угол ограничителя.
            pos2(bar_center_x - bar_half_width, center.y - bar_half_height),
            // Правый верхний угол ограничителя.
            pos2(bar_center_x + bar_half_width, center.y - bar_half_height),
            // Правый нижний угол ограничителя.
            pos2(bar_center_x + bar_half_width, center.y + bar_half_height),
            // Левый нижний угол ограничителя.
            pos2(bar_center_x - bar_half_width, center.y + bar_half_height),
        ],
        style.color,
        Stroke::NONE,
    ));

    // Остриё треугольника оставляет один bar-width между собой и ограничителем.
    let triangle_tip_x = center.x + direction * (half_extent - style.bar_width * 1.5);
    // Основание занимает противоположную сторону glyph и создаёт читаемый transport-силуэт.
    let triangle_base_x = center.x - direction * (half_extent - style.bar_width);
    // Треугольник немного ниже ограничителя, как в предоставленном концепте.
    let triangle_half_height = half_extent * 0.78;
    // Порядок точек одинаков для обоих направлений, потому что меняются только X-координаты.
    painter.add(Shape::convex_polygon(
        vec![
            // Острие показывает направление перехода.
            pos2(triangle_tip_x, center.y),
            // Верхняя точка вертикального основания.
            pos2(triangle_base_x, center.y - triangle_half_height),
            // Нижняя точка вертикального основания.
            pos2(triangle_base_x, center.y + triangle_half_height),
        ],
        style.color,
        Stroke::NONE,
    ));
}
