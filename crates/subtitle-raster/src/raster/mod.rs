// Порт из libass 0.17.5: ass_rasterizer.c, ass_rasterizer.h, c/rasterizer_template.h.
//
// Copyright (C) 2016 Vabishchevich Nikolay <vabnick@gmail.com>
//
// Permission to use, copy, modify, and distribute this software for any
// purpose with or without fee is hereby granted, provided that the above
// copyright notice and this permission notice appear in all copies.
//
// THE SOFTWARE IS PROVIDED "AS IS" AND THE AUTHOR DISCLAIMS ALL WARRANTIES
// WITH REGARD TO THIS SOFTWARE INCLUDING ALL IMPLIED WARRANTIES OF
// MERCHANTABILITY AND FITNESS. IN NO EVENT SHALL THE AUTHOR BE LIABLE FOR
// ANY SPECIAL, DIRECT, INDIRECT, OR CONSEQUENTIAL DAMAGES OR ANY DAMAGES
// WHATSOEVER RESULTING FROM LOSS OF USE, DATA OR PROFITS, WHETHER IN AN
// ACTION OF CONTRACT, NEGLIGENCE OR OTHER TORTIOUS ACTION, ARISING OUT OF
// OR IN CONNECTION WITH THE USE OR PERFORMANCE OF THIS SOFTWARE.
//
// Rust port © Fastiplayer contributors.

//! Растеризатор контуров в маску покрытия 8 бит (libass 0.17.5: `ass_rasterizer.c`).
//!
//! ## Алгоритм (комментарий libass «Tiled Rasterization Algorithm»)
//! 1. Сплайны контура рекурсивно превращаются в ломаную ([`Rasterizer::set_outline`]).
//! 2. Для каждого тайла определяется, какие отрезки в него попадают: список отрезков
//!    рекурсивно делится горизонтальными и вертикальными линиями; одновременно считается
//!    число намотки левого верхнего угла второй половины ([`segment`]).
//! 3. На уровне тайла 16x16 возможны три исхода: нет отрезков — тайл заливается сплошным
//!    значением по намотке; один отрезок — заливка полуплоскостью; общий случай — каждый отрезок
//!    рисуется как трапеция со знаком ([`tile`]).
//!
//! Подрежимы разбиты по файлам: [`segment`] — отрезки и разбиение, [`tile`] — заливка тайла,
//! [`fill`] — обход четвертного дерева и подготовка окна.

mod fill;
#[cfg(test)]
mod internals_tests;
mod segment;
#[cfg(test)]
mod tests;
mod tile;

pub(crate) use fill::TileRect;
pub(crate) use tile::TILE_SIZE;

use crate::outline::{ControlBox, Outline, OutlineError, Point, SegmentKind};
use fill::LineStacks;
use segment::Segment;

/// Почему растеризация не удалась.
#[derive(Debug, Clone, Copy, PartialEq, Eq, thiserror::Error)]
pub(crate) enum RasterError {
    /// Контур несогласован или координаты вне диапазона.
    #[error(transparent)]
    Outline(#[from] OutlineError),
    /// Окно заливки пустое либо его размеры не кратны размеру тайла.
    #[error("fill window must be non-empty and tile-aligned")]
    InvalidWindow,
    /// Буфер короче, чем требует окно заливки (шаг строки * высота).
    #[error("target buffer is smaller than the fill window requires")]
    BufferTooSmall,
    /// Отрезок вышел за пределы тайла: нарушение инварианта разбиения.
    #[error("polyline segment lies outside of its tile")]
    SegmentOutsideTile,
}

/// Какой контур добавляется: основной или дополнительный (libass: параметр `extra`).
///
/// Основной сбрасывает накопленную ломаную и рамку; дополнительный (вторая граница обводки)
/// добавляется к основному, а повторный дополнительный заменяет предыдущий дополнительный.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum OutlineLayer {
    /// Основной контур (`extra = false`).
    Primary,
    /// Дополнительный контур (`extra = true`).
    Additional,
}

/// Окно заливки в пикселях (libass: параметры `ass_rasterizer_fill`).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) struct FillWindow {
    /// Левый край окна в координатах контура, пиксели.
    pub(crate) x0: i32,
    /// Верхний край окна, пиксели.
    pub(crate) y0: i32,
    /// Ширина окна, кратна размеру тайла.
    pub(crate) width: usize,
    /// Высота окна, кратна размеру тайла.
    pub(crate) height: usize,
    /// Шаг строки целевого буфера в байтах (не меньше ширины).
    pub(crate) stride: usize,
}

/// Состояние растеризатора (libass: `RasterizerData`).
#[derive(Debug)]
pub(crate) struct Rasterizer {
    /// Допустимая ошибка аппроксимации сплайнов в 1/64 пикселя (`RASTERIZER_PRECISION = 16`).
    outline_error: i32,
    /// Точная рамка ломаной (после `set_outline`).
    bbox: ControlBox,
    /// Накопленная ломаная; первые `n_first` отрезков — основной контур.
    polyline: Vec<Segment>,
    n_first: usize,
    /// Стеки отрезков уровней четвертного дерева (libass: `linebuf[2]`).
    stacks: LineStacks,
    /// Рабочий буфер разбиения (чтобы не выделять память на каждый уровень).
    scratch: Vec<Segment>,
    /// Временный тайл для объединения двух заливок (libass: `rst->tile`).
    tile: [u8; TILE_SIZE * TILE_SIZE],
    /// Наибольшая сторона битмапа в пикселях, которую разрешено выделять (`None` — без предела).
    /// Предел задаёт владелец конфигурации (`RasterInputLimits::max_canvas_side`): картинка
    /// шире любого холста бесполезна, а 3D-перспектива у плоскости камеры раздувает контур
    /// до сотен мегабайт.
    max_bitmap_side: Option<u32>,
}

impl Rasterizer {
    /// Создаёт растеризатор с допустимой ошибкой сплайнов `outline_error` (1/64 пикселя).
    pub(crate) fn new(outline_error: i32) -> Self {
        Self {
            outline_error,
            bbox: ControlBox::EMPTY,
            polyline: Vec::new(),
            n_first: 0,
            stacks: LineStacks::default(),
            scratch: Vec::new(),
            tile: [0; TILE_SIZE * TILE_SIZE],
            max_bitmap_side: None,
        }
    }

    /// Ограничивает сторону выделяемых битмапов (см. поле `max_bitmap_side`).
    pub(crate) fn with_max_bitmap_side(mut self, side: u32) -> Self {
        self.max_bitmap_side = Some(side);
        self
    }

    /// Предел стороны битмапа, если задан.
    pub(crate) fn max_bitmap_side(&self) -> Option<u32> {
        self.max_bitmap_side
    }

    /// Точная рамка загруженной ломаной в 1/64 пикселя; пуста, если контуров нет.
    pub(crate) fn bounding_box(&self) -> ControlBox {
        self.bbox
    }

    /// Превращает контур в ломаную и обновляет рамку (libass: `ass_rasterizer_set_outline`).
    pub(crate) fn set_outline(
        &mut self,
        path: &Outline,
        layer: OutlineLayer,
    ) -> Result<(), RasterError> {
        if layer == OutlineLayer::Primary {
            self.bbox = ControlBox::EMPTY;
            self.n_first = 0;
        }
        self.polyline.truncate(self.n_first);

        for view in path.segments_view() {
            let view = view?;
            match (view.kind, view.owned) {
                (SegmentKind::Line, [start]) => self.add_line(*start, view.end),
                (SegmentKind::Quadratic, [start, control]) => {
                    self.add_quadratic(&[*start, *control, view.end]);
                }
                (SegmentKind::Cubic, [start, first_control, second_control]) => {
                    self.add_cubic(&[*start, *first_control, *second_control, view.end]);
                }
                _ => return Err(OutlineError::Malformed.into()),
            }
        }

        for line in self.polyline.iter().skip(self.n_first) {
            self.bbox
                .include(line.x_min, line.y_min, line.x_max, line.y_max);
        }
        if layer == OutlineLayer::Primary {
            self.n_first = self.polyline.len();
        }
        Ok(())
    }

    /// Добавляет отрезок ломаной; нулевой длины пропускаются (libass: `add_line`).
    fn add_line(&mut self, start: Point, end: Point) {
        if let Some(segment) = Segment::from_line(start, end) {
            self.polyline.push(segment);
        }
    }

    /// Квадратичный сплайн -> ломаная рекурсивным делением (libass: `add_quadratic`).
    fn add_quadratic(&mut self, points: &[Point; 3]) {
        let [start, control, end] = *points;
        let split = SplitTest::new(start, end, self.outline_error);
        if !split.needs_subdivision(start, control) {
            self.add_line(start, end);
            return;
        }
        let mut mid1 = Point::new(start.x + control.x, start.y + control.y);
        let mut mid3 = Point::new(control.x + end.x, control.y + end.y);
        let mid2 = Point::new((mid1.x + mid3.x + 2) >> 2, (mid1.y + mid3.y + 2) >> 2);
        mid1 = Point::new(mid1.x >> 1, mid1.y >> 1);
        mid3 = Point::new(mid3.x >> 1, mid3.y >> 1);
        self.add_quadratic(&[start, mid1, mid2]);
        self.add_quadratic(&[mid2, mid3, end]);
    }

    /// Кубический сплайн -> ломаная рекурсивным делением (libass: `add_cubic`).
    fn add_cubic(&mut self, points: &[Point; 4]) {
        let [start, control1, control2, end] = *points;
        let split = SplitTest::new(start, end, self.outline_error);
        if !split.needs_subdivision(start, control1) && !split.needs_subdivision(start, control2) {
            self.add_line(start, end);
            return;
        }
        let mut next1 = Point::new(start.x + control1.x, start.y + control1.y);
        let center = Point::new(control1.x + control2.x + 2, control1.y + control2.y + 2);
        let mut next5 = Point::new(control2.x + end.x, control2.y + end.y);
        let mut next2 = Point::new(next1.x + center.x, next1.y + center.y);
        let mut next4 = Point::new(center.x + next5.x, center.y + next5.y);
        let next3 = Point::new((next2.x + next4.x - 1) >> 3, (next2.y + next4.y - 1) >> 3);
        next2 = Point::new(next2.x >> 2, next2.y >> 2);
        next4 = Point::new(next4.x >> 2, next4.y >> 2);
        next1 = Point::new(next1.x >> 1, next1.y >> 1);
        next5 = Point::new(next5.x >> 1, next5.y >> 1);
        self.add_cubic(&[start, next1, next2, next3]);
        self.add_cubic(&[next3, next4, next5, end]);
    }
}

/// Решение «делить ли сплайн» (libass: `OutlineSegment`, `segment_init`, `segment_subdivide`).
///
/// Сплайн делится, если контрольная точка уклоняется от хорды дальше допустимой ошибки
/// либо выходит за концы хорды.
struct SplitTest {
    /// Вектор хорды.
    chord: Point,
    /// Квадрат длины хорды.
    chord_length_squared: i64,
    /// Допустимая ошибка, умноженная на максимум модулей компонентов хорды.
    allowed_error: i64,
}

impl SplitTest {
    fn new(start: Point, end: Point, outline_error: i32) -> Self {
        let x = end.x - start.x;
        let y = end.y - start.y;
        let abs_x = x.abs();
        let abs_y = y.abs();
        Self {
            chord: Point::new(x, y),
            chord_length_squared: i64::from(x) * i64::from(x) + i64::from(y) * i64::from(y),
            allowed_error: i64::from(outline_error) * i64::from(abs_x.max(abs_y)),
        }
    }

    /// Контрольная точка `point` требует деления сплайна, начатого в `start`.
    fn needs_subdivision(&self, start: Point, point: Point) -> bool {
        let x = point.x - start.x;
        let y = point.y - start.y;
        let along = i64::from(self.chord.x) * i64::from(x) + i64::from(self.chord.y) * i64::from(y);
        let across =
            i64::from(self.chord.x) * i64::from(y) - i64::from(self.chord.y) * i64::from(x);
        along < -self.allowed_error
            || along > self.chord_length_squared + self.allowed_error
            || across.abs() > self.allowed_error
    }
}
