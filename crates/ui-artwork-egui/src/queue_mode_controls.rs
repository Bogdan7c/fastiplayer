//! Нейтральная векторная геометрия постоянных Shuffle и Repeat controls.

use egui::{Color32, Painter, Pos2, Rect, Shape, Stroke, pos2};

/// Glyph режима очереди без зависимости от playlist domain.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum QueueModeGlyph {
    /// Две пересекающиеся траектории со стрелками.
    Shuffle,

    /// Две закольцованные стрелки повтора очереди.
    Repeat,

    /// Повтор одного элемента с геометрической цифрой `1`.
    RepeatOne,
}

/// Уже разрешённое вызывающей стороной визуальное состояние одного control.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct QueueModePaintState {
    /// Итоговый цвет glyph после hover/active/disabled переходов.
    pub foreground: Color32,

    /// Итоговая круглая поверхность; прозрачный цвет означает отсутствие shape.
    pub surface_fill: Color32,

    /// Видим ли keyboard-focus outline.
    pub focus_visible: bool,

    /// Масштабируется только glyph, поэтому hit-area и layout остаются неизменными.
    pub content_scale: f32,
}

/// Размеры и outline-токены, общие для обеих queue-mode кнопок.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct QueueModeControlStyle {
    /// Полный размер glyph внутри переданного hit-area.
    pub icon_extent: f32,

    /// Толщина всех линий glyph.
    pub glyph_stroke_width: f32,

    /// Keyboard-focus outline, уже окрашенный skin-ом.
    pub focus_outline: Stroke,

    /// Inset outline от границы квадратной hit-area.
    pub focus_inset: f32,
}

/// Рисует поверхность, focus и выбранный glyph внутри стабильной hit-area.
pub(crate) fn paint(
    painter: &Painter,
    rect: Rect,
    glyph: QueueModeGlyph,
    state: QueueModePaintState,
    style: QueueModeControlStyle,
) {
    // Круглая поверхность не зависит от pressed scale и сохраняет постоянный control footprint.
    let surface_radius = rect.width().min(rect.height()) * 0.5;
    // Полностью прозрачное idle-состояние не добавляет пустой shape в paint list.
    if state.surface_fill != Color32::TRANSPARENT {
        painter.circle_filled(rect.center(), surface_radius, state.surface_fill);
    }

    // Focus outline остаётся снаружи glyph и никогда не меняет interaction geometry.
    if state.focus_visible {
        painter.circle_stroke(
            rect.center(),
            (surface_radius - style.focus_inset).max(0.0),
            style.focus_outline,
        );
    }

    // Некорректный внешний scale не может вытолкнуть glyph за собственную hit-area.
    let content_scale = state.content_scale.clamp(0.0, 1.0);
    // Половина extent определяет общий bounding box всех queue-mode вариантов.
    let half_extent = style.icon_extent * content_scale * 0.5;
    // Одинаковый stroke поддерживает единый визуальный вес Shuffle, Repeat и цифры.
    let glyph_stroke = Stroke::new(style.glyph_stroke_width, state.foreground);

    match glyph {
        QueueModeGlyph::Shuffle => paint_shuffle(painter, rect.center(), half_extent, glyph_stroke),
        QueueModeGlyph::Repeat => paint_repeat(painter, rect.center(), half_extent, glyph_stroke),
        QueueModeGlyph::RepeatOne => {
            paint_repeat(painter, rect.center(), half_extent, glyph_stroke);
            paint_repeat_one_digit(painter, rect.center(), half_extent, glyph_stroke);
        }
    }
}

/// Рисует две плавно пересекающиеся линии и две открытые стрелки Shuffle.
fn paint_shuffle(painter: &Painter, center: Pos2, half_extent: f32, stroke: Stroke) {
    // Вертикальный разнос оставляет пересечение читаемым даже на 18-point glyph.
    let lane_offset = half_extent * 0.52;
    // Линии заканчиваются немного раньше стрелок, чтобы открытые наконечники не слипались.
    let arrow_length = half_extent * 0.34;
    // Общие горизонтальные границы удерживают все points внутри icon extent.
    let left_x = center.x - half_extent;
    let right_x = center.x + half_extent;
    // Первая cubic-кривая идёт слева сверху направо вниз.
    let descending = cubic_points(
        pos2(left_x, center.y - lane_offset),
        pos2(center.x - half_extent * 0.25, center.y - lane_offset),
        pos2(center.x + half_extent * 0.20, center.y + lane_offset),
        pos2(right_x - arrow_length * 0.25, center.y + lane_offset),
    );
    // Вторая cubic-кривая зеркалит первую по горизонтальной оси.
    let ascending = cubic_points(
        pos2(left_x, center.y + lane_offset),
        pos2(center.x - half_extent * 0.25, center.y + lane_offset),
        pos2(center.x + half_extent * 0.20, center.y - lane_offset),
        pos2(right_x - arrow_length * 0.25, center.y - lane_offset),
    );
    // Каждая траектория остаётся отдельным PathShape для deterministic artwork tests.
    painter.add(Shape::line(descending, stroke));
    painter.add(Shape::line(ascending, stroke));
    // Открытые наконечники не создают залитых треугольников и сохраняют лёгкий glyph.
    paint_open_right_arrow(
        painter,
        pos2(right_x, center.y + lane_offset),
        arrow_length,
        stroke,
    );
    paint_open_right_arrow(
        painter,
        pos2(right_x, center.y - lane_offset),
        arrow_length,
        stroke,
    );
}

/// Рисует две соединённые визуально дуги Repeat с противоположными стрелками.
fn paint_repeat(painter: &Painter, center: Pos2, half_extent: f32, stroke: Stroke) {
    // Repeat использует немного меньшую вертикальную амплитуду, чем Shuffle.
    let lane_offset = half_extent * 0.48;
    // Наконечник занимает треть icon half-extent и остаётся внутри bounding box.
    let arrow_length = half_extent * 0.34;
    // Верхняя траектория загибается от левой середины к стрелке справа.
    let top_loop = cubic_points(
        pos2(center.x - half_extent, center.y),
        pos2(center.x - half_extent, center.y - lane_offset),
        pos2(center.x - half_extent * 0.55, center.y - lane_offset),
        pos2(center.x + half_extent, center.y - lane_offset),
    );
    // Нижняя траектория симметрично возвращается от правой середины к стрелке слева.
    let bottom_loop = cubic_points(
        pos2(center.x + half_extent, center.y),
        pos2(center.x + half_extent, center.y + lane_offset),
        pos2(center.x + half_extent * 0.55, center.y + lane_offset),
        pos2(center.x - half_extent, center.y + lane_offset),
    );
    // Две открытые линии формируют единый закольцованный силуэт без заливки.
    painter.add(Shape::line(top_loop, stroke));
    painter.add(Shape::line(bottom_loop, stroke));
    // Верхняя стрелка направлена вправо.
    paint_open_right_arrow(
        painter,
        pos2(center.x + half_extent, center.y - lane_offset),
        arrow_length,
        stroke,
    );
    // Нижняя стрелка направлена влево точным зеркалом.
    paint_open_left_arrow(
        painter,
        pos2(center.x - half_extent, center.y + lane_offset),
        arrow_length,
        stroke,
    );
}

/// Добавляет геометрическую цифру `1` в центр Repeat One без font-зависимости.
fn paint_repeat_one_digit(painter: &Painter, center: Pos2, half_extent: f32, stroke: Stroke) {
    // Цифра ниже дуг и уже основного glyph, поэтому не пересекается со стрелками.
    let digit_half_height = half_extent * 0.36;
    // Небольшой верхний штрих отличает `1` от простой вертикальной черты.
    let digit_points = vec![
        pos2(
            center.x - half_extent * 0.16,
            center.y - digit_half_height * 0.55,
        ),
        pos2(center.x, center.y - digit_half_height),
        pos2(center.x, center.y + digit_half_height),
    ];
    // Один open PathShape делает отличие Repeat One стабильным и легко тестируемым.
    painter.add(Shape::line(digit_points, stroke));
}

/// Рисует открытый V-образный наконечник стрелки вправо.
fn paint_open_right_arrow(painter: &Painter, tip: Pos2, length: f32, stroke: Stroke) {
    // Обе ножки сходятся в tip и не требуют отдельной заливки.
    painter.add(Shape::line(
        vec![
            pos2(tip.x - length, tip.y - length),
            tip,
            pos2(tip.x - length, tip.y + length),
        ],
        stroke,
    ));
}

/// Рисует открытый V-образный наконечник стрелки влево.
fn paint_open_left_arrow(painter: &Painter, tip: Pos2, length: f32, stroke: Stroke) {
    // Геометрия является горизонтальным зеркалом правого наконечника.
    painter.add(Shape::line(
        vec![
            pos2(tip.x + length, tip.y - length),
            tip,
            pos2(tip.x + length, tip.y + length),
        ],
        stroke,
    ));
}

/// Сэмплирует cubic Bézier в предсказуемый набор точек для одного PathShape.
fn cubic_points(start: Pos2, control_a: Pos2, control_b: Pos2, end: Pos2) -> Vec<Pos2> {
    // Двенадцать сегментов дают плавную линию на production glyph 18 pt.
    const SEGMENT_COUNT: usize = 12;
    // Обе крайние точки включаются, чтобы stroke точно достигал заданных границ.
    (0..=SEGMENT_COUNT)
        .map(|segment_index| {
            // Нормализованный параметр меняется от нуля до единицы включительно.
            let time = segment_index as f32 / SEGMENT_COUNT as f32;
            // Обратный параметр упрощает стандартную cubic Bézier формулу.
            let inverse_time = 1.0 - time;
            // Коэффициенты вычисляются явно, чтобы geometry не зависела от внешнего tessellator API.
            let start_weight = inverse_time.powi(3);
            let control_a_weight = 3.0 * inverse_time.powi(2) * time;
            let control_b_weight = 3.0 * inverse_time * time.powi(2);
            let end_weight = time.powi(3);
            // Взвешенная сумма возвращает одну детерминированную point кривой.
            pos2(
                start.x * start_weight
                    + control_a.x * control_a_weight
                    + control_b.x * control_b_weight
                    + end.x * end_weight,
                start.y * start_weight
                    + control_a.y * control_a_weight
                    + control_b.y * control_b_weight
                    + end.y * end_weight,
            )
        })
        .collect()
}
