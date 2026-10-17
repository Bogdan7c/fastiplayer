// Порт из libass 0.17.5: ass_outline.c / ass_outline.h.
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

//! Контур глифа в формате libass: точки 26.6 и сегменты «линия / квадратичная / кубическая».
//!
//! Модуль владеет самим [`Outline`] и его инвариантами; растеризатор ([`crate::raster`]) и
//! обводка ([`stroke`]) только читают контур через [`Outline::segments_view`].
//!
//! ## Формат (libass 0.17.5: `ass_outline.h`)
//!
//! Контур — массив точек и массив сегментов. Сегмент порядка N (1 = линия, 2 = квадратичный
//! сплайн, 3 = кубический) *владеет* N точками подряд (начало и внутренние контрольные),
//! а конечной точкой берёт первую точку следующего сегмента. У последнего сегмента контура
//! (флаг «конец контура») конечная точка — первая точка этого же контура. Поэтому общее число
//! точек равно сумме порядков всех сегментов.

pub(crate) mod pen;
pub(crate) mod stroke;
pub(crate) mod synthetic;
#[cfg(test)]
mod tests;

/// Абсолютный предел координат точек контура (libass: `OUTLINE_MAX`).
///
/// Предел выбран так, чтобы деление кубического сплайна (`8 * OUTLINE_MAX + 4`) не
/// переполняло `i32`.
pub(crate) const OUTLINE_MAX: i32 = (1 << 28) - 1;

/// Точка в формате 26.6 (1/64 пикселя), libass: `ASS_Vector`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub(crate) struct Point {
    /// Горизонталь, 1/64 пикселя.
    pub(crate) x: i32,
    /// Вертикаль, 1/64 пикселя (ось вниз, как на экране).
    pub(crate) y: i32,
}

impl Point {
    /// Создаёт точку из координат 26.6.
    pub(crate) const fn new(x: i32, y: i32) -> Self {
        Self { x, y }
    }

    /// Лежит ли точка в допустимом диапазоне `[-OUTLINE_MAX, OUTLINE_MAX]`.
    pub(crate) fn is_in_outline_range(self) -> bool {
        self.x.unsigned_abs() <= OUTLINE_MAX as u32 && self.y.unsigned_abs() <= OUTLINE_MAX as u32
    }
}

/// Габаритная рамка по контрольным точкам (libass: `ASS_Rect`), границы включительно.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) struct ControlBox {
    /// Минимальная X.
    pub(crate) x_min: i32,
    /// Минимальная Y.
    pub(crate) y_min: i32,
    /// Максимальная X.
    pub(crate) x_max: i32,
    /// Максимальная Y.
    pub(crate) y_max: i32,
}

impl ControlBox {
    /// Пустая рамка (libass: `rectangle_reset`): минимумы больше максимумов.
    pub(crate) const EMPTY: Self = Self {
        x_min: i32::MAX,
        y_min: i32::MAX,
        x_max: i32::MIN,
        y_max: i32::MIN,
    };

    /// Расширяет рамку (libass: `rectangle_update`).
    pub(crate) fn include(&mut self, x_min: i32, y_min: i32, x_max: i32, y_max: i32) {
        self.x_min = self.x_min.min(x_min);
        self.y_min = self.y_min.min(y_min);
        self.x_max = self.x_max.max(x_max);
        self.y_max = self.y_max.max(y_max);
    }

    /// Рамка ничего не содержит (минимум больше максимума).
    pub(crate) fn is_empty(&self) -> bool {
        self.x_min > self.x_max || self.y_min > self.y_max
    }
}

/// Порядок сегмента: сколько точек он владеет (libass: `OUTLINE_LINE_SEGMENT` и т. д.).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum SegmentKind {
    /// Отрезок: владеет 1 точкой (началом).
    Line,
    /// Квадратичный сплайн: владеет 2 точками (начало и контрольная).
    Quadratic,
    /// Кубический сплайн: владеет 3 точками (начало и две контрольные).
    Cubic,
}

impl SegmentKind {
    /// Число точек, которыми владеет сегмент.
    pub(crate) const fn owned_points(self) -> usize {
        match self {
            Self::Line => 1,
            Self::Quadratic => 2,
            Self::Cubic => 3,
        }
    }
}

/// Запись о сегменте: порядок и признак «последний в контуре».
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) struct Segment {
    /// Порядок сегмента.
    pub(crate) kind: SegmentKind,
    /// Сегмент замыкает контур (libass: `OUTLINE_CONTOUR_END`).
    pub(crate) ends_contour: bool,
}

/// Почему контур нельзя построить, преобразовать или прочитать.
#[derive(Debug, Clone, Copy, PartialEq, Eq, thiserror::Error)]
pub(crate) enum OutlineError {
    /// Координата вне `[-OUTLINE_MAX, OUTLINE_MAX]`.
    #[error("outline point is outside of the allowed coordinate range")]
    PointOutOfRange,
    /// Сегменты и точки не согласованы (не хватает точек, лишние точки, контур не закрыт).
    #[error("outline segments and points are inconsistent")]
    Malformed,
    /// Масштаб или матрица выводят координаты за допустимый диапазон.
    #[error("outline transform overflows the coordinate range")]
    TransformOverflow,
}

/// Представление одного сегмента при обходе контура.
///
/// Соответствует паре указателей `cur` / `end` в циклах libass
/// (`ass_rasterizer_set_outline`, `ass_outline_stroke`).
#[derive(Debug, Clone, Copy)]
pub(crate) struct SegmentView<'outline> {
    /// Порядок сегмента.
    pub(crate) kind: SegmentKind,
    /// Точки, которыми владеет сегмент (длина = `kind.owned_points()`): начало, контрольные.
    pub(crate) owned: &'outline [Point],
    /// Конечная точка сегмента (первая точка следующего сегмента или начало контура).
    pub(crate) end: Point,
    /// Первый сегмент своего контура.
    pub(crate) starts_contour: bool,
    /// Последний сегмент своего контура.
    pub(crate) ends_contour: bool,
}

/// Контур: точки 26.6 и сегменты (libass: `ASS_Outline`).
///
/// Инварианты: каждая точка в диапазоне `OUTLINE_MAX` (проверяет [`Self::push_point`]);
/// согласованность сегментов и точек проверяет обход [`Self::segments_view`] — он возвращает
/// [`OutlineError::Malformed`] вместо паники.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub(crate) struct Outline {
    points: Vec<Point>,
    segments: Vec<Segment>,
}

impl Outline {
    /// Пустой контур.
    pub(crate) fn new() -> Self {
        Self::default()
    }

    /// Пустой контур с заранее выделенной ёмкостью (libass: `ass_outline_alloc`).
    pub(crate) fn with_capacity(point_capacity: usize, segment_capacity: usize) -> Self {
        Self {
            points: Vec::with_capacity(point_capacity),
            segments: Vec::with_capacity(segment_capacity),
        }
    }

    /// Все точки контура.
    pub(crate) fn points(&self) -> &[Point] {
        &self.points
    }

    /// Все сегменты контура.
    pub(crate) fn segments(&self) -> &[Segment] {
        &self.segments
    }

    /// В контуре нет точек.
    pub(crate) fn is_empty(&self) -> bool {
        self.points.is_empty()
    }

    /// Добавляет точку, проверяя диапазон (libass: `ass_outline_add_point` без сегмента).
    pub(crate) fn push_point(&mut self, point: Point) -> Result<(), OutlineError> {
        if !point.is_in_outline_range() {
            return Err(OutlineError::PointOutOfRange);
        }
        self.points.push(point);
        Ok(())
    }

    /// Добавляет сегмент (libass: `ass_outline_add_segment`); его точки добавляются отдельно.
    pub(crate) fn push_segment(&mut self, kind: SegmentKind) {
        self.segments.push(Segment {
            kind,
            ends_contour: false,
        });
    }

    /// Заменяет уже добавленную точку (нужно обводчику: `fix_first_point`).
    pub(crate) fn replace_point(&mut self, index: usize, point: Point) -> Result<(), OutlineError> {
        if !point.is_in_outline_range() {
            return Err(OutlineError::PointOutOfRange);
        }
        let slot = self.points.get_mut(index).ok_or(OutlineError::Malformed)?;
        *slot = point;
        Ok(())
    }

    /// Откатывает контур до заданного числа точек и сегментов (для вырожденных контуров).
    pub(crate) fn truncate(&mut self, point_count: usize, segment_count: usize) {
        self.points.truncate(point_count);
        self.segments.truncate(segment_count);
    }

    /// Помечает последний сегмент концом контура (libass: `ass_outline_close_contour`).
    ///
    /// Возвращает `false`, если сегментов ещё нет или контур уже закрыт: вызывающий
    /// обязан отличать это от успеха, молча ничего не делать нельзя.
    #[must_use = "false означает, что закрывать было нечего"]
    pub(crate) fn close_contour(&mut self) -> bool {
        match self.segments.last_mut() {
            Some(segment) if !segment.ends_contour => {
                segment.ends_contour = true;
                true
            }
            _ => false,
        }
    }

    /// Контур закрыт: пустой либо последний сегмент помечен концом контура.
    pub(crate) fn is_closed(&self) -> bool {
        self.segments
            .last()
            .is_none_or(|segment| segment.ends_contour)
    }

    /// Добавляет прямоугольник отдельным контуром (libass: `ass_outline_add_rect`).
    ///
    /// Обход: (x0,y0) -> (x1,y0) -> (x1,y1) -> (x0,y1). Предыдущий контур должен быть закрыт.
    pub(crate) fn add_rect(
        &mut self,
        x0: i32,
        y0: i32,
        x1: i32,
        y1: i32,
    ) -> Result<(), OutlineError> {
        if !self.is_closed() {
            return Err(OutlineError::Malformed);
        }
        let corners = [
            Point::new(x0, y0),
            Point::new(x1, y0),
            Point::new(x1, y1),
            Point::new(x0, y1),
        ];
        if !corners.iter().all(|corner| corner.is_in_outline_range()) {
            return Err(OutlineError::PointOutOfRange);
        }
        self.points.extend_from_slice(&corners);
        for _ in 0..3 {
            self.push_segment(SegmentKind::Line);
        }
        self.segments.push(Segment {
            kind: SegmentKind::Line,
            ends_contour: true,
        });
        Ok(())
    }

    /// Габаритная рамка контрольных точек (libass: `ass_outline_update_cbox`).
    pub(crate) fn update_control_box(&self, control_box: &mut ControlBox) {
        for point in &self.points {
            control_box.include(point.x, point.y, point.x, point.y);
        }
    }

    /// Обходит сегменты, выдавая для каждого его точки и конечную точку.
    ///
    /// Первая ошибка согласованности завершает обход (`Err`), дальше итератор пуст.
    pub(crate) fn segments_view(&self) -> SegmentsView<'_> {
        SegmentsView {
            outline: self,
            segment_index: 0,
            point_index: 0,
            contour_start: 0,
            failed: false,
        }
    }

    /// Проверяет согласованность сегментов и точек (полный обход без накопления).
    pub(crate) fn validate(&self) -> Result<(), OutlineError> {
        self.segments_view().try_for_each(|view| view.map(|_| ()))
    }

    /// Масштабирует контур на `2^order_x` по X и `2^order_y` по Y
    /// (libass 0.17.5: `ass_outline_scale_pow2`).
    ///
    /// Сдвиг выражен умножением на `2^(order + 32)` в i64 и сдвигом вправо на 32, как в C:
    /// так он работает и для отрицательных координат, и для отрицательных порядков.
    /// Пустой контур возвращает пустой результат. Слишком большой масштаб (координата не
    /// помещается после умножения) — [`OutlineError::TransformOverflow`].
    pub(crate) fn scaled_pow2(&self, order_x: i32, order_y: i32) -> Result<Self, OutlineError> {
        if self.points.is_empty() {
            return Ok(Self::new());
        }
        let (limit_x, shift_x) = scale_limit(order_x);
        let (limit_y, shift_y) = scale_limit(order_y);
        if limit_x == 0 || limit_y == 0 {
            return Err(OutlineError::TransformOverflow);
        }
        let mut points = Vec::with_capacity(self.points.len());
        for point in &self.points {
            if point.x.unsigned_abs() > limit_x || point.y.unsigned_abs() > limit_y {
                return Err(OutlineError::TransformOverflow);
            }
            // Равно `point << order`, но корректно для отрицательных координат и порядков.
            points.push(Point::new(
                ((i64::from(point.x) << shift_x) >> 32) as i32,
                ((i64::from(point.y) << shift_y) >> 32) as i32,
            ));
        }
        Ok(Self {
            points,
            segments: self.segments.clone(),
        })
    }

    /// Аффинное преобразование матрицей 2x3 (libass 0.17.5: `ass_outline_transform_2d`).
    ///
    /// `matrix[k] = [mx, my, offset]`; результат округляется как `lrint` (к ближайшему
    /// чётному). Выход за диапазон, включая NaN, — [`OutlineError::TransformOverflow`].
    pub(crate) fn transformed_2d(&self, matrix: &[[f64; 3]; 2]) -> Result<Self, OutlineError> {
        if self.points.is_empty() {
            return Ok(Self::new());
        }
        let limit = f64::from(OUTLINE_MAX);
        let mut points = Vec::with_capacity(self.points.len());
        for point in &self.points {
            let x = matrix[0][0] * f64::from(point.x)
                + matrix[0][1] * f64::from(point.y)
                + matrix[0][2];
            let y = matrix[1][0] * f64::from(point.x)
                + matrix[1][1] * f64::from(point.y)
                + matrix[1][2];
            // Отрицание условия ловит и NaN (libass: `!(fabs(v) < OUTLINE_MAX)`).
            if !(x.abs() < limit && y.abs() < limit) {
                return Err(OutlineError::TransformOverflow);
            }
            points.push(Point::new(
                round_ties_even_to_i32(x),
                round_ties_even_to_i32(y),
            ));
        }
        Ok(Self {
            points,
            segments: self.segments.clone(),
        })
    }

    /// Перспективное преобразование матрицей 3x3 (libass 0.17.5: `ass_outline_transform_3d`).
    ///
    /// Деление на `z` ограничено снизу `0.1`: точки за камерой или почти в её плоскости не
    /// дают бесконечностей, а уходят далеко и отбраковываются проверкой диапазона. Выход за
    /// диапазон, включая NaN, — [`OutlineError::TransformOverflow`].
    pub(crate) fn transformed_3d(&self, matrix: &[[f64; 3]; 3]) -> Result<Self, OutlineError> {
        if self.points.is_empty() {
            return Ok(Self::new());
        }
        let limit = f64::from(OUTLINE_MAX);
        let mut points = Vec::with_capacity(self.points.len());
        for point in &self.points {
            let (px, py) = (f64::from(point.x), f64::from(point.y));
            let x = matrix[0][0] * px + matrix[0][1] * py + matrix[0][2];
            let y = matrix[1][0] * px + matrix[1][1] * py + matrix[1][2];
            let z = matrix[2][0] * px + matrix[2][1] * py + matrix[2][2];
            // `FFMAX(z, 0.1)` из C: при NaN возвращает 0.1 (сравнение ложно); `f64::max`
            // ведёт себя так же (NaN игнорируется).
            let w = 1.0 / z.max(0.1);
            let (x, y) = (x * w, y * w);
            if !(x.abs() < limit && y.abs() < limit) {
                return Err(OutlineError::TransformOverflow);
            }
            points.push(Point::new(
                round_ties_even_to_i32(x),
                round_ties_even_to_i32(y),
            ));
        }
        Ok(Self {
            points,
            segments: self.segments.clone(),
        })
    }
}

/// `lrint` из libass (`ass_lrint`): округление к ближайшему целому, ничьи — к чётному.
///
/// Вызывающий гарантирует `|value| < OUTLINE_MAX`, поэтому приведение не насыщается.
pub(crate) fn round_ties_even_to_i32(value: f64) -> i32 {
    /// 1.5 * 2^52: прибавление этого числа сдвигает порядок так, что единица младшего разряда
    /// `f64` равна 1, и сложение само округляет к ближайшему целому с ничьёй к чётному.
    const ROUNDING_MAGIC: f64 = 6_755_399_441_055_744.0;
    /// Для модуля меньше 2^51 приём точен.
    const MAGIC_LIMIT: f64 = 2_251_799_813_685_248.0;
    if value.abs() < MAGIC_LIMIT {
        // Базовая сборка x86-64 не имеет `roundsd` (SSE4.1), и `round_ties_even` превращается
        // в вызов программной `rint`, а этот путь на каждой точке контура горячий.
        // Без быстрых математических флагов компилятор не сокращает `(x + c) - c` до `x`.
        return ((value + ROUNDING_MAGIC) - ROUNDING_MAGIC) as i32;
    }
    // NaN и огромные значения: прежнее поведение (насыщение приведения).
    value.round_ties_even() as i32
}

/// Предел координаты и величина сдвига для порядка масштаба
/// (libass: начало `ass_outline_scale_pow2`). Возвращает `(предел, order + 32)`.
fn scale_limit(order: i32) -> (u32, u32) {
    if order > 0 {
        // Масштаб вверх: исходная координата не должна выйти за OUTLINE_MAX после сдвига.
        let limit = if order < 32 {
            (OUTLINE_MAX >> order) as u32
        } else {
            0
        };
        // При `order >= 32` предел равен 0, и вызывающий отклоняет такой масштаб до сдвига,
        // поэтому `min(32)` лишь удерживает величину сдвига в допустимых пределах.
        (limit, (order.min(32) + 32) as u32)
    } else {
        // Масштаб вниз: порядок ниже -32 равнозначен -32.
        (OUTLINE_MAX as u32, (order.max(-32) + 32) as u32)
    }
}

/// Итератор по сегментам контура; создаётся [`Outline::segments_view`].
pub(crate) struct SegmentsView<'outline> {
    outline: &'outline Outline,
    segment_index: usize,
    point_index: usize,
    contour_start: usize,
    failed: bool,
}

impl<'outline> Iterator for SegmentsView<'outline> {
    type Item = Result<SegmentView<'outline>, OutlineError>;

    fn next(&mut self) -> Option<Self::Item> {
        if self.failed {
            return None;
        }
        let Some(segment) = self.outline.segments.get(self.segment_index) else {
            // Сегменты кончились: все точки обязаны быть использованы, контур — закрыт.
            if self.point_index != self.outline.points.len()
                || self.contour_start != self.point_index
            {
                self.failed = true;
                return Some(Err(OutlineError::Malformed));
            }
            return None;
        };
        let starts_contour = self.point_index == self.contour_start;
        let owned_end = self.point_index + segment.kind.owned_points();
        let owned = self.outline.points.get(self.point_index..owned_end);
        // Конечная точка: начало контура либо первая точка следующего сегмента.
        let end = if segment.ends_contour {
            self.outline.points.get(self.contour_start).copied()
        } else {
            self.outline.points.get(owned_end).copied()
        };
        let (Some(owned), Some(end)) = (owned, end) else {
            self.failed = true;
            return Some(Err(OutlineError::Malformed));
        };
        let view = SegmentView {
            kind: segment.kind,
            owned,
            end,
            starts_contour,
            ends_contour: segment.ends_contour,
        };
        self.segment_index += 1;
        self.point_index = owned_end;
        if segment.ends_contour {
            self.contour_start = owned_end;
        }
        Some(Ok(view))
    }
}
