// Порт из libass 0.17.5: ass_outline.c (раздел «Outline Stroke Algorithm»).
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

//! Эллиптическая обводка контура (libass 0.17.5: `ass_outline_stroke`).
//!
//! ## Цель алгоритма (из комментария libass)
//! По исходному контуру строятся два контура-границы (смещения на +1 и -1 в «нормальном»
//! пространстве, масштабированном на `[1/xbord, 1/ybord]`), такие что любая точка внутри
//! любой границы (правило ненулевой обмотки) ближе к исходному контуру, чем 1 (с точностью
//! `eps`), а любая точка вне обеих — дальше примерно чем 1. Две границы нужны для
//! самопересекающихся контуров.
//!
//! Каждый сегмент (линия, квадратичный или кубический сплайн) и стык между сегментами
//! обводятся почти независимо. Для сплайнов сначала смещаются отдельные точки, затем
//! оценивается ошибка и при необходимости сплайн рекурсивно делится пополам.
//!
//! Особые случаи:
//! 1. Слишком близкие точки сливаются в одну (иначе производная случайна или деление на 0).
//! 2. Нулевая производная в точке сплайна — добавляется круговая «шапка».
//! 3. Отрицательная кривизна (смещение больше радиуса кривизны) — проблемный сплайн
//!    заменяется ломаной, покрывающей только часть с положительной обмоткой.
//!
//! Структура модуля: общее состояние и линии — здесь, дуги — [`arc`], квадратичные сплайны —
//! [`quadratic`], кубические — [`cubic`].

mod arc;
mod cubic;
mod quadratic;
#[cfg(test)]
mod tests;

use super::{OUTLINE_MAX, Outline, OutlineError, Point, SegmentKind};

/// Битовая маска «в какие границы писать»: бит 0 — первая (положительная) граница,
/// бит 1 — вторая (отрицательная). Соответствует `int dir` в libass.
pub(super) type SideMask = u8;
/// Первая граница (смещение `+offs`).
pub(super) const SIDE_POSITIVE: SideMask = 1;
/// Вторая граница (смещение `-offs`).
pub(super) const SIDE_NEGATIVE: SideMask = 2;
/// Обе границы (`dir = 3` в libass).
const BOTH_SIDES: SideMask = SIDE_POSITIVE | SIDE_NEGATIVE;

/// Двумерный вектор с плавающей точкой (libass: `ASS_DVector`).
#[derive(Debug, Clone, Copy, PartialEq, Default)]
pub(super) struct DVector {
    pub(super) x: f64,
    pub(super) y: f64,
}

/// Нормаль и длина производной, из которой она получена (libass: `Normal`).
#[derive(Debug, Clone, Copy)]
pub(super) struct Normal {
    pub(super) v: DVector,
    pub(super) len: f64,
}

/// Скалярное произведение (libass: `vec_dot`).
pub(super) fn vec_dot(a: DVector, b: DVector) -> f64 {
    a.x * b.x + a.y * b.y
}

/// Псевдоскалярное произведение (libass: `vec_crs`).
pub(super) fn vec_crs(a: DVector, b: DVector) -> f64 {
    a.x * b.y - a.y * b.x
}

/// Длина вектора (libass: `vec_len`).
pub(super) fn vec_len(a: DVector) -> f64 {
    (a.x * a.x + a.y * a.y).sqrt()
}

/// Почему обводка не построена.
#[derive(Debug, Clone, Copy, PartialEq, Eq, thiserror::Error)]
pub(crate) enum StrokeError {
    /// Параметры нарушают `0 < precision <= max(border) <= OUTLINE_MAX` (в libass — assert).
    #[error("stroke parameters are out of range")]
    InvalidParameters,
    /// Ошибка контура: координата вне диапазона или контур несогласован.
    #[error(transparent)]
    Outline(#[from] OutlineError),
}

/// Параметры обводки.
///
/// В libass вызывается как `ass_outline_stroke(.., xbord, ybord, eps)`; смысл величин:
/// `border_x` / `border_y` — полуразмеры обводки в единицах контура (26.6), `precision` —
/// допустимая ошибка в тех же единицах (`STROKER_PRECISION = 16` в `ass_render.c`).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) struct StrokeParameters {
    /// Размер обводки по X.
    pub(crate) border_x: i32,
    /// Размер обводки по Y.
    pub(crate) border_y: i32,
    /// Допустимая ошибка (libass: `eps`).
    pub(crate) precision: i32,
}

/// Две границы обводки (libass: `result` и `result1`).
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct StrokedOutline {
    /// Граница со смещением +1.
    pub(crate) positive: Outline,
    /// Граница со смещением -1.
    pub(crate) negative: Outline,
}

/// Состояние обводчика (libass: `StrokerState`).
pub(super) struct StrokerState {
    /// Результирующие контуры: `[положительный, отрицательный]`.
    result: [Outline; 2],
    /// Индекс первой точки последнего контура в каждом результате.
    contour_first: [usize; 2],
    /// Размеры обводки.
    xbord: f64,
    ybord: f64,
    /// Обратные размеры обводки.
    xscale: f64,
    yscale: f64,
    /// Допустимая ошибка в координатах контура.
    eps: i32,
    /// Контур ещё не содержит точек.
    contour_start: bool,
    /// Флаги пропуска для первой и последней точки контура.
    first_skip: SideMask,
    last_skip: SideMask,
    /// Нормали в первой и последней точках контура.
    first_normal: DVector,
    last_normal: DVector,
    /// Первая и последняя точки текущего контура.
    first_point: Point,
    last_point: Point,
    /// Косинус максимального угла, не требующего «шапки».
    merge_cos: f64,
    /// Косинус максимального угла дуги, аппроксимируемой одним квадратичным сплайном.
    split_cos: f64,
    /// Минимальное расстояние между контрольными точками в нормальном пространстве,
    /// при меньшем включается обработка вырожденных случаев.
    min_len: f64,
    /// Константа точной проверки радиальной ошибки (квадратичный случай).
    err_q: f64,
    /// Константа приближённой проверки радиальной ошибки (кубический случай).
    err_c: f64,
    /// Тангенс максимальной угловой ошибки.
    err_a: f64,
}

/// Обводит контур (libass 0.17.5: `ass_outline_stroke`).
///
/// Возвращает две границы; их рисуют вместе (ненулевая обмотка) как единую область обводки.
pub(crate) fn stroke_outline(
    path: &Outline,
    parameters: StrokeParameters,
) -> Result<StrokedOutline, StrokeError> {
    let radius = parameters.border_x.max(parameters.border_y);
    // В libass это assert: `rad >= eps && rad <= OUTLINE_MAX`; здесь — ошибка, не паника.
    if parameters.precision <= 0
        || parameters.border_x < 0
        || parameters.border_y < 0
        || radius < parameters.precision
        || radius > OUTLINE_MAX
    {
        return Err(StrokeError::InvalidParameters);
    }
    path.validate()?;
    let mut state = StrokerState::new(path, parameters, radius);
    for view in path.segments_view() {
        let view = view?;
        // Начало контура: перо стоит в его первой точке.
        if view.starts_contour
            && let Some(first) = view.owned.first()
        {
            state.last_point = *first;
        }
        match (view.kind, view.owned) {
            (SegmentKind::Line, _) => state.add_line(view.end, BOTH_SIDES)?,
            (SegmentKind::Quadratic, [_, control]) => {
                state.add_quadratic(*control, view.end, BOTH_SIDES)?;
            }
            (SegmentKind::Cubic, [_, first_control, second_control]) => {
                state.add_cubic(*first_control, *second_control, view.end, BOTH_SIDES)?;
            }
            _ => return Err(OutlineError::Malformed.into()),
        }
        if view.ends_contour {
            state.close_contour(BOTH_SIDES)?;
        }
    }
    let [positive, negative] = state.result;
    Ok(StrokedOutline { positive, negative })
}

impl StrokerState {
    /// Начальное состояние и производные константы точности.
    fn new(path: &Outline, parameters: StrokeParameters, radius: i32) -> Self {
        let eps = parameters.precision;
        let rel_err = f64::from(eps) / f64::from(radius);
        let e = (2.0 * rel_err).sqrt();
        Self {
            result: [
                Outline::with_capacity(2 * path.points().len(), 2 * path.segments().len()),
                Outline::with_capacity(2 * path.points().len(), 2 * path.segments().len()),
            ],
            contour_first: [0, 0],
            xbord: f64::from(parameters.border_x),
            ybord: f64::from(parameters.border_y),
            xscale: 1.0 / f64::from(eps.max(parameters.border_x)),
            yscale: 1.0 / f64::from(eps.max(parameters.border_y)),
            eps,
            contour_start: true,
            first_skip: 0,
            last_skip: 0,
            first_normal: DVector::default(),
            last_normal: DVector::default(),
            first_point: Point::default(),
            last_point: Point::default(),
            merge_cos: 1.0 - rel_err,
            split_cos: 1.0 + 8.0 * rel_err - 4.0 * (1.0 + rel_err) * e,
            min_len: rel_err / 4.0,
            err_q: 8.0 * (1.0 + rel_err) * (1.0 + rel_err),
            err_c: 390.0 * rel_err * rel_err,
            err_a: e,
        }
    }

    /// Вектор меньше `eps` по обеим осям — точки считаются совпавшими.
    pub(super) fn is_negligible(&self, dx: i32, dy: i32) -> bool {
        dx > -self.eps && dx < self.eps && dy > -self.eps && dy < self.eps
    }

    /// Добавляет точку в одну или две границы (libass: `emit_point`).
    ///
    /// Смещение `offs` задано в нормальном пространстве и переводится в координаты
    /// умножением на размеры обводки с усечением к нулю (приведение `double -> int32_t`).
    pub(super) fn emit_point(
        &mut self,
        point: Point,
        offs: DVector,
        segment: Option<SegmentKind>,
        dir: SideMask,
    ) -> Result<(), StrokeError> {
        let dx = (self.xbord * offs.x) as i32;
        let dy = (self.ybord * offs.y) as i32;
        if dir & SIDE_POSITIVE != 0 {
            let moved = offset_point(point, dx, dy, 1)?;
            Self::push_to_side(&mut self.result[0], moved, segment)?;
        }
        if dir & SIDE_NEGATIVE != 0 {
            let moved = offset_point(point, dx, dy, -1)?;
            Self::push_to_side(&mut self.result[1], moved, segment)?;
        }
        Ok(())
    }

    /// Добавляет точку (и, если задан, сегмент) в границу `outline`.
    fn push_to_side(
        outline: &mut Outline,
        point: Point,
        segment: Option<SegmentKind>,
    ) -> Result<(), StrokeError> {
        outline.push_point(point)?;
        if let Some(kind) = segment {
            outline.push_segment(kind);
        }
        Ok(())
    }

    /// Заменяет первую точку текущего контура (libass: `fix_first_point`).
    fn fix_first_point(
        &mut self,
        point: Point,
        offs: DVector,
        dir: SideMask,
    ) -> Result<(), StrokeError> {
        let dx = (self.xbord * offs.x) as i32;
        let dy = (self.ybord * offs.y) as i32;
        if dir & SIDE_POSITIVE != 0 {
            let moved = offset_point(point, dx, dy, 1)?;
            self.result[0].replace_point(self.contour_first[0], moved)?;
        }
        if dir & SIDE_NEGATIVE != 0 {
            let moved = offset_point(point, dx, dy, -1)?;
            self.result[1].replace_point(self.contour_first[1], moved)?;
        }
        Ok(())
    }

    /// Начинает сегмент и при необходимости добавляет круговую «шапку» (libass: `start_segment`).
    pub(super) fn start_segment(
        &mut self,
        point: Point,
        normal: DVector,
        mut dir: SideMask,
    ) -> Result<(), StrokeError> {
        if self.contour_start {
            self.contour_start = false;
            self.first_skip = 0;
            self.last_skip = 0;
            self.first_normal = normal;
            self.last_normal = normal;
            self.first_point = point;
            return Ok(());
        }

        let prev = self.last_normal;
        let c = vec_dot(prev, normal);
        if c > self.merge_cos {
            // Угол мал: сливаем нормали без шапки.
            let mul = 1.0 / (1.0 + c);
            self.last_normal.x = (self.last_normal.x + normal.x) * mul;
            self.last_normal.y = (self.last_normal.y + normal.y) * mul;
            return Ok(());
        }
        self.last_normal = normal;

        // Отрицательная кривизна: с какой стороны границы пересекаются.
        let s = vec_crs(prev, normal);
        let skip_dir = if s < 0.0 {
            SIDE_POSITIVE
        } else {
            SIDE_NEGATIVE
        };
        if dir & skip_dir != 0 {
            self.emit_point(
                point,
                prev,
                Some(SegmentKind::Line),
                !self.last_skip & skip_dir,
            )?;
            self.emit_point(point, DVector::default(), Some(SegmentKind::Line), skip_dir)?;
        }
        self.last_skip = skip_dir;

        dir &= !skip_dir;
        if dir == 0 {
            Ok(())
        } else {
            self.draw_arc(point, prev, normal, c, dir)
        }
    }

    /// Как [`Self::emit_point`], но ещё обновляет флаги пропуска (libass: `emit_first_point`).
    pub(super) fn emit_first_point(
        &mut self,
        point: Point,
        segment: SegmentKind,
        dir: SideMask,
    ) -> Result<(), StrokeError> {
        self.last_skip &= !dir;
        self.emit_point(point, self.last_normal, Some(segment), dir)
    }

    /// Готовится пропустить часть кривой (libass: `prepare_skip`).
    pub(super) fn prepare_skip(
        &mut self,
        point: Point,
        dir: SideMask,
        first: bool,
    ) -> Result<(), StrokeError> {
        if first {
            self.first_skip |= dir;
        } else {
            self.emit_point(
                point,
                self.last_normal,
                Some(SegmentKind::Line),
                !self.last_skip & dir,
            )?;
        }
        self.last_skip |= dir;
        Ok(())
    }

    /// Обрабатывает отрезок до `end` (libass: `add_line`).
    pub(super) fn add_line(&mut self, end: Point, dir: SideMask) -> Result<(), StrokeError> {
        let dx = end.x.wrapping_sub(self.last_point.x);
        let dy = end.y.wrapping_sub(self.last_point.y);
        if self.is_negligible(dx, dy) {
            return Ok(());
        }

        let deriv = DVector {
            x: f64::from(dy) * self.yscale,
            y: -f64::from(dx) * self.xscale,
        };
        let scale = 1.0 / vec_len(deriv);
        let normal = DVector {
            x: deriv.x * scale,
            y: deriv.y * scale,
        };
        self.start_segment(self.last_point, normal, dir)?;
        self.emit_first_point(self.last_point, SegmentKind::Line, dir)?;
        self.last_normal = normal;
        self.last_point = end;
        Ok(())
    }

    /// Закрывает контур (libass: `close_contour`).
    pub(super) fn close_contour(&mut self, mut dir: SideMask) -> Result<(), StrokeError> {
        if self.contour_start {
            // Контур из одной точки (или слитых точек): обводка — круг.
            if dir & BOTH_SIDES == BOTH_SIDES {
                dir = SIDE_POSITIVE;
            }
            self.draw_circle(self.last_point, dir)?;
        } else {
            self.add_line(self.first_point, dir)?;
            self.start_segment(self.first_point, self.first_normal, dir)?;
            self.emit_point(
                self.first_point,
                self.first_normal,
                Some(SegmentKind::Line),
                !self.last_skip & dir & self.first_skip,
            )?;
            if self.last_normal != self.first_normal {
                self.fix_first_point(
                    self.first_point,
                    self.last_normal,
                    !self.last_skip & dir & !self.first_skip,
                )?;
            }
            self.contour_start = true;
        }
        if dir & SIDE_POSITIVE != 0 && !self.result[0].close_contour() {
            return Err(OutlineError::Malformed.into());
        }
        if dir & SIDE_NEGATIVE != 0 && !self.result[1].close_contour() {
            return Err(OutlineError::Malformed.into());
        }
        self.contour_first = [self.result[0].points().len(), self.result[1].points().len()];
        Ok(())
    }
}

/// Сдвигает точку на `(dx, dy)` со знаком стороны, проверяя переполнение `i32`.
fn offset_point(point: Point, dx: i32, dy: i32, sign: i32) -> Result<Point, OutlineError> {
    let moved_x = sign
        .checked_mul(dx)
        .and_then(|delta| point.x.checked_add(delta));
    let moved_y = sign
        .checked_mul(dy)
        .and_then(|delta| point.y.checked_add(delta));
    match (moved_x, moved_y) {
        (Some(x), Some(y)) => Ok(Point::new(x, y)),
        _ => Err(OutlineError::PointOutOfRange),
    }
}
