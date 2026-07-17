use egui::epaint::{Mesh, PathShape};
use egui::{Align2, Color32, FontId, Painter, Pos2, Rect, Shape, Stroke, pos2};

use crate::ButtonVisualState;

/// Геометрия анимированной кнопки сброса скорости.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct PlaybackRateButtonGeometry {
    /// Полный rect кнопки в её текущей анимированной позиции.
    pub button_rect: Rect,
    /// Видимая часть rect; всё слева от неё считается спрятанным под Play/Pause.
    pub visible_clip_rect: Rect,
    /// Радиус дуги Play/Pause, которую повторяет вогнутая левая грань.
    pub concave_radius: f32,
}

/// Визуальные параметры кнопки сброса скорости.
#[derive(Debug, Clone, PartialEq)]
pub struct PlaybackRateButtonStyle {
    /// Светлый контур прозрачной кнопки.
    pub outline: Stroke,
    /// Полупрозрачная заливка при наведении.
    pub hover_fill: Color32,
    /// Цвет подписи текущей скорости.
    pub text_color: Color32,
    /// Шрифт подписи текущей скорости.
    pub font_id: FontId,
}

/// Количество сегментов дуги: этого достаточно для 32-point кнопки без заметных изломов.
const CONCAVE_ARC_SEGMENTS: usize = 16;

/// Рисует анимированную кнопку скорости, не зная ничего о playback-состоянии и действиях.
pub(crate) fn paint(
    painter: &Painter,
    geometry: PlaybackRateButtonGeometry,
    label: Option<&str>,
    state: ButtonVisualState,
    style: PlaybackRateButtonStyle,
) {
    // Нулевой clip означает полностью спрятанную кнопку и не должен создавать paint shapes.
    if geometry.visible_clip_rect.width() <= 0.0
        || geometry.visible_clip_rect.height() <= 0.0
        || geometry.button_rect.width() <= 0.0
        || geometry.button_rect.height() <= 0.0
    {
        return;
    }

    // Отдельный painter обрезает moving geometry на фиксированной границе рядом с Play/Pause.
    let clipped_painter = painter.with_clip_rect(geometry.visible_clip_rect);
    // Общий набор точек гарантирует, что hover-fill и outline используют одну форму.
    let concave_rows = concave_rows(geometry.button_rect, geometry.concave_radius);

    // В покое кнопка остаётся прозрачной; заливка появляется только при hover.
    if state == ButtonVisualState::Hovered {
        // Concave polygon нельзя передать в `PathShape` с fill, поэтому собираем triangle strip.
        clipped_painter.add(Shape::mesh(hover_mesh(
            &concave_rows,
            geometry.button_rect.right(),
            style.hover_fill,
        )));
    }

    // Outline замыкает вогнутую грань, нижнюю, правую и верхнюю стороны кнопки.
    clipped_painter.add(Shape::Path(PathShape::closed_line(
        outline_points(&concave_rows, geometry.button_rect.right()),
        style.outline,
    )));

    // `None` используется при подтверждённом 1x: пустой контур может закрыться без надписи 1x.
    if let Some(label) = label {
        // Текст центрируется в реально пригодной области между глубиной выемки и правой гранью.
        let deepest_left = concave_left_x(
            geometry.button_rect,
            geometry.concave_radius,
            geometry.button_rect.center().y,
        );
        // Центр usable-области не даёт длинному `0.25x` визуально залезать в выемку.
        let label_center_x = (deepest_left + geometry.button_rect.right()) * 0.5;
        // Возвращаемый rect текста не нужен: hit-testing остаётся ответственностью app-egui.
        let _ = clipped_painter.text(
            pos2(label_center_x, geometry.button_rect.center().y),
            Align2::CENTER_CENTER,
            label,
            style.font_id,
            style.text_color,
        );
    }
}

/// Возвращает точки вогнутой грани сверху вниз.
fn concave_rows(rect: Rect, radius: f32) -> Vec<Pos2> {
    // Ровно `segments + 1` точек включают обе горизонтальные границы.
    (0..=CONCAVE_ARC_SEGMENTS)
        .map(|segment_index| {
            // Нормализованный прогресс делает sampling независимым от высоты кнопки.
            let progress = segment_index as f32 / CONCAVE_ARC_SEGMENTS as f32;
            // Линейное распределение Y достаточно, потому что X вычисляется по окружности.
            let y = rect.top() + rect.height() * progress;
            // Каждая строка хранит фактическую точку вогнутой грани.
            pos2(concave_left_x(rect, radius, y), y)
        })
        .collect()
}

/// Вычисляет X дуги, которая повторяет круглую Play/Pause-кнопку.
fn concave_left_x(rect: Rect, radius: f32, y: f32) -> f32 {
    // Радиус не может быть меньше половины высоты, иначе окружность не покрывает грань.
    let safe_radius = radius.max(rect.height() * 0.5);
    // Вертикальное расстояние берётся относительно общего центра кнопок.
    let vertical_distance = (y - rect.center().y).abs().min(safe_radius);
    // X правой половины окружности на текущей высоте.
    let circle_x = (safe_radius.mul_add(safe_radius, -vertical_distance * vertical_distance))
        .max(0.0)
        .sqrt();
    // На верхней и нижней границе дуга должна начинаться ровно от `rect.left()`.
    let edge_vertical_distance = (rect.height() * 0.5).min(safe_radius);
    // Базовый X окружности вычитается, чтобы перевести абсолютную дугу в локальную выемку.
    let edge_circle_x = (safe_radius.mul_add(
        safe_radius,
        -edge_vertical_distance * edge_vertical_distance,
    ))
    .max(0.0)
    .sqrt();
    // На экстремально узком rect выемка ограничивается правой гранью.
    (rect.left() + circle_x - edge_circle_x).min(rect.right())
}

/// Строит triangle strip для hover-заливки concave silhouette.
fn hover_mesh(concave_rows: &[Pos2], right_x: f32, fill: Color32) -> Mesh {
    // Mesh остаётся untextured и содержит две вершины на каждую sampled строку.
    let mut mesh = Mesh::default();
    // Резервирование исключает лишние reallocations при каждом UI-кадре.
    mesh.reserve_vertices(concave_rows.len() * 2);
    // Между соседними строками всегда две треугольные половины quad-а.
    mesh.reserve_triangles(concave_rows.len().saturating_sub(1) * 2);

    // Сначала добавляем пары left/right, чтобы индексы образовывали простой triangle strip.
    for left_point in concave_rows {
        // Левая вершина следует дуге.
        mesh.colored_vertex(*left_point, fill);
        // Правая вершина сохраняет вертикальную прямоугольную грань.
        mesh.colored_vertex(pos2(right_x, left_point.y), fill);
    }

    // Каждая пара соседних строк превращается в quad из двух треугольников.
    for row_index in 0..concave_rows.len().saturating_sub(1) {
        // В mesh у каждой строки ровно две вершины.
        let top_left = (row_index * 2) as u32;
        // Остальные индексы выводятся из фиксированного порядка left/right.
        let top_right = top_left + 1;
        // Следующая строка начинается через две вершины.
        let bottom_left = top_left + 2;
        // Правая нижняя вершина завершает quad.
        let bottom_right = top_left + 3;
        // Первый треугольник покрывает левую половину quad-а.
        mesh.add_triangle(top_left, top_right, bottom_left);
        // Второй треугольник покрывает правую половину без зазора.
        mesh.add_triangle(top_right, bottom_right, bottom_left);
    }

    // Готовый mesh передаётся Painter одним shape.
    mesh
}

/// Замыкает sampled вогнутую грань через правые углы кнопки.
fn outline_points(concave_rows: &[Pos2], right_x: f32) -> Vec<Pos2> {
    // Две дополнительные точки образуют нижнюю и верхнюю правые вершины.
    let mut points = Vec::with_capacity(concave_rows.len() + 2);
    // Вогнутая сторона идёт сверху вниз.
    points.extend_from_slice(concave_rows);
    // Последняя sampled строка гарантированно соответствует низу rect.
    if let Some(bottom_left) = concave_rows.last() {
        // Нижняя сторона идёт от дуги к правой грани.
        points.push(pos2(right_x, bottom_left.y));
    }
    // Первая sampled строка гарантированно соответствует верху rect.
    if let Some(top_left) = concave_rows.first() {
        // Правая сторона поднимается к верхнему углу, после чего PathShape замкнёт верх.
        points.push(pos2(right_x, top_left.y));
    }
    // Возвращаем один стабильный path для outline.
    points
}
