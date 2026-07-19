//! Нейтральный векторный glyph подтверждённо играющего элемента.

use egui::{Color32, Painter, Pos2, Rect, Shape, vec2};

/// Доля доступной высоты, занятая треугольником.
const HEIGHT_FACTOR: f32 = 0.58;
/// Ширина треугольника относительно его высоты.
const WIDTH_FACTOR: f32 = 0.72;

/// Рисует центрированный `Play`-треугольник без знания playlist-состояния.
pub(crate) fn paint(painter: &Painter, cell_rect: Rect, color: Color32) {
    // Пустая или нечисловая ячейка не должна создавать некорректную mesh-геометрию.
    let Some(points) = glyph_points(cell_rect) else {
        return;
    };
    // Filled shape сохраняет чёткий силуэт при дробном HiDPI scale.
    painter.add(Shape::convex_polygon(
        points.to_vec(),
        color,
        egui::Stroke::NONE,
    ));
}

/// Возвращает стабильные вершины для characterization-тестов и paint path.
pub(crate) fn glyph_points(cell_rect: Rect) -> Option<[Pos2; 3]> {
    // Некорректный rect нельзя безопасно нормализовать относительно центра.
    if !cell_rect.is_finite() || !cell_rect.is_positive() {
        return None;
    }
    // Меньшая сторона не даёт широкой badge-ячейке растянуть glyph.
    let available_side = cell_rect.width().min(cell_rect.height());
    // Высота остаётся заметной, но не касается границ ячейки.
    let glyph_height = available_side * HEIGHT_FACTOR;
    // Ширина сохраняет привычную оптическую форму Play.
    let glyph_width = glyph_height * WIDTH_FACTOR;
    // Небольшой сдвиг вправо визуально центрирует треугольник по массе.
    let glyph_center = cell_rect.center() + vec2(glyph_width * 0.06, 0.0);
    // Левая вертикальная грань задаёт симметричные верхнюю и нижнюю вершины.
    let left_x = glyph_center.x - glyph_width * 0.5;
    // Острие располагается справа на той же центральной оси.
    let right_x = glyph_center.x + glyph_width * 0.5;
    // Половина высоты используется обеими вершинами без накопления rounding.
    let half_height = glyph_height * 0.5;
    // Порядок по часовой стрелке формирует валидный convex polygon.
    Some([
        Pos2::new(left_x, glyph_center.y - half_height),
        Pos2::new(right_x, glyph_center.y),
        Pos2::new(left_x, glyph_center.y + half_height),
    ])
}
