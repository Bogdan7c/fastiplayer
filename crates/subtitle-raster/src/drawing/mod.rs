// Порт из libass 0.17.5: ass_drawing.c (drawing_add_curve, ass_drawing_parse).
//
// Copyright (C) 2016 Vabishchevich Nikolay <vabnick@gmail.com>
// Copyright (C) 2017 Oleg Oshmyan <chortos@inbox.lv>
// Copyright (C) 2006 Evgeniy Stepanov <eugeni.stepanov@gmail.com>
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

//! Векторные рисунки ASS (`\p<n>`): текст команд `m n l b s p c` -> контур 26.6.
//!
//! Модуль владеет только разбором: текст -> [`ParsedDrawing`] (контур и рамка опорных точек).
//! Размещение рисунка в строке (масштаб `2^(n-1)`, `\pbo`, метрики глифа) — забота конвейера
//! текста (`glyph_render::drawing`): рисунок для него один псевдоглиф.
//!
//! ## Арифметика
//! Координаты — `i32` в 26.6; деление сплайна на три — целочисленное с усечением к нулю, сдвиг
//! на единицу — арифметический, как в C. Промежуточные суммы над `i32` выполняются с
//! заворачиванием (в C это неопределённое поведение, на практике — заворачивание): точки,
//! вышедшие за [`OUTLINE_MAX`], всё равно отвергает проверка диапазона при добавлении в контур.
//!
//! ## Границы работы
//! Работа линейна по длине текста рисунка (токенов не больше, чем символов, см. [`tokens`]),
//! контур растёт не быстрее токенов. Текст рисунка — часть текста реплики ASS, длину которого
//! ограничивает `RasterInputLimits::max_chars_per_cue` (проверка в `SubtitleRasterizer`
//! до разбора); выделений «по запросу автора» сверх этого нет.

mod number;
mod tokens;

use crate::outline::{ControlBox, Outline, OutlineError, Point, SegmentKind};
use tokens::{Token, TokenKind, tokenize};

/// Почему рисунок отвергнут.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub(crate) enum DrawingError {
    /// Точка контура вне допустимого диапазона libass (`|координата| > OUTLINE_MAX`).
    #[error("drawing point is out of the outline range: {0}")]
    Outline(#[from] OutlineError),
    /// Токены противоречат друг другу (нарушение внутреннего инварианта токенизатора);
    /// libass здесь падает на `assert`.
    #[error("drawing tokens are inconsistent")]
    InconsistentTokens,
}

/// Результат разбора рисунка.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct ParsedDrawing {
    /// Контур в 26.6; пуст, если команды не дали ни одного отрезка.
    pub(crate) outline: Outline,
    /// Рамка ВСЕХ опорных точек, включая перемещения пера (libass: `ASS_Rect *cbox` из
    /// `ass_drawing_parse`). Для пустого рисунка — перевёрнутая рамка (`rectangle_reset`).
    pub(crate) bounds: ControlBox,
}

impl ParsedDrawing {
    /// Ширина рамки точек в 26.6 (libass: `v->advance = bbox.x_max - bbox.x_min`).
    ///
    /// У пустого рисунка рамка перевёрнута, и разность заворачивается через границу `i32`
    /// (в C — то же заворачивание): получается 1. Это поведение libass, а не ошибка порта.
    pub(crate) fn width(&self) -> i32 {
        self.bounds.x_max.wrapping_sub(self.bounds.x_min)
    }

    /// Высота рамки точек в 26.6 (libass: `v->asc = bbox.y_max - bbox.y_min`); у пустого
    /// рисунка, как и [`Self::width`], равна 1.
    pub(crate) fn height(&self) -> i32 {
        self.bounds.y_max.wrapping_sub(self.bounds.y_min)
    }
}

/// Разбирает текст рисунка в контур (libass: `ass_drawing_parse`).
///
/// `Ok` с пустым контуром — рисунок без отрезков (нет узлов, отвергнут `n` до `m`, одни
/// перемещения); `Err` — координата вне диапазона контура.
pub(crate) fn parse_drawing(text: &str) -> Result<ParsedDrawing, DrawingError> {
    let tokens = tokenize(text.as_bytes());
    let mut builder = ContourBuilder::new();
    let mut index = 0;
    while let Some(token) = tokens.get(index) {
        index += builder.apply(&tokens, index, *token)?;
    }
    builder.finish()
}

/// Строитель контура: перо, признак «контур начат» и рамка точек.
struct ContourBuilder {
    outline: Outline,
    bounds: ControlBox,
    /// Позиция пера, заданная последним `m`/`n` (libass: `pen`).
    pen: Point,
    /// В текущем контуре уже есть отрезки (libass: `started`).
    started: bool,
}

impl ContourBuilder {
    fn new() -> Self {
        Self {
            outline: Outline::new(),
            bounds: ControlBox::EMPTY,
            pen: Point::new(0, 0),
            started: false,
        }
    }

    /// Обрабатывает токен `index` и возвращает, на сколько токенов продвинуться.
    fn apply(
        &mut self,
        tokens: &[Token],
        index: usize,
        token: Token,
    ) -> Result<usize, DrawingError> {
        match token.kind {
            TokenKind::MoveWithoutClosing => {
                self.move_pen(token.point);
                Ok(1)
            }
            TokenKind::Move => {
                self.move_pen(token.point);
                if self.started {
                    self.close_contour()?;
                }
                Ok(1)
            }
            TokenKind::Line => {
                self.include(token.point);
                if !self.started {
                    self.outline.push_point(self.pen)?;
                }
                self.outline.push_point(token.point)?;
                self.outline.push_segment(SegmentKind::Line);
                self.started = true;
                Ok(1)
            }
            TokenKind::CubicBezier => {
                self.add_curve(
                    curve_points(tokens, index.checked_sub(1))?,
                    CurveKind::Bezier,
                )?;
                Ok(3)
            }
            TokenKind::BSpline => {
                self.add_curve(
                    curve_points(tokens, index.checked_sub(1))?,
                    CurveKind::BSpline,
                )?;
                Ok(3)
            }
            TokenKind::ExtendSpline => {
                // Кривая из трёх предыдущих токенов и текущего.
                self.add_curve(
                    curve_points(tokens, index.checked_sub(3))?,
                    CurveKind::BSpline,
                )?;
                Ok(1)
            }
        }
    }

    fn include(&mut self, point: Point) {
        self.bounds.include(point.x, point.y, point.x, point.y);
    }

    fn move_pen(&mut self, point: Point) {
        self.pen = point;
        self.include(point);
    }

    /// Закрывает текущий контур (libass: `add_segment(LINE)` + `close_contour`).
    fn close_contour(&mut self) -> Result<(), DrawingError> {
        self.outline.push_segment(SegmentKind::Line);
        if !self.outline.close_contour() {
            return Err(DrawingError::InconsistentTokens);
        }
        self.started = false;
        Ok(())
    }

    /// Добавляет кубический сегмент (libass: `drawing_add_curve`). Для b-сплайна четыре точки
    /// пересчитываются в кубическую кривую Безье целочисленно.
    fn add_curve(&mut self, mut points: [Point; 4], kind: CurveKind) -> Result<(), DrawingError> {
        for point in points {
            self.include(point);
        }
        if kind == CurveKind::BSpline {
            points = bspline_to_bezier(points);
        }
        let [start, first_control, second_control, end] = points;
        if !self.started {
            self.outline.push_point(start)?;
        }
        self.outline.push_point(first_control)?;
        self.outline.push_point(second_control)?;
        self.outline.push_point(end)?;
        self.outline.push_segment(SegmentKind::Cubic);
        self.started = true;
        Ok(())
    }

    /// Закрывает последний контур и отдаёт результат.
    fn finish(mut self) -> Result<ParsedDrawing, DrawingError> {
        if self.started {
            self.close_contour()?;
        }
        Ok(ParsedDrawing {
            outline: self.outline,
            bounds: self.bounds,
        })
    }
}

/// Какую кривую задают четыре точки.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum CurveKind {
    /// Кубическая Безье: точки используются как есть.
    Bezier,
    /// Равномерный кубический b-сплайн: точки пересчитываются.
    BSpline,
}

/// Четыре последовательные точки токенов, начиная с индекса `start` (`None` — токенов
/// перед текущим не хватает).
fn curve_points(tokens: &[Token], start: Option<usize>) -> Result<[Point; 4], DrawingError> {
    let start = start.ok_or(DrawingError::InconsistentTokens)?;
    match tokens.get(start..start + 4) {
        Some([a, b, c, d]) => Ok([a.point, b.point, c.point, d.point]),
        _ => Err(DrawingError::InconsistentTokens),
    }
}

/// Пересчёт четырёх точек b-сплайна в кубическую Безье (libass: `drawing_add_curve`, ветка
/// `spline`). Деление `/ 3` усекает к нулю, `>> 1` — арифметический сдвиг, как в C.
fn bspline_to_bezier(points: [Point; 4]) -> [Point; 4] {
    let [p0, p1, p2, p3] = points;
    let third = |from: i32, to: i32| to.wrapping_sub(from) / 3;
    let (x01, y01) = (third(p0.x, p1.x), third(p0.y, p1.y));
    let (x12, y12) = (third(p1.x, p2.x), third(p1.y, p2.y));
    let (x23, y23) = (third(p2.x, p3.x), third(p2.y, p3.y));
    let half_difference = |later: i32, earlier: i32| later.wrapping_sub(earlier) >> 1;
    [
        Point::new(
            p1.x.wrapping_add(half_difference(x12, x01)),
            p1.y.wrapping_add(half_difference(y12, y01)),
        ),
        Point::new(p1.x.wrapping_add(x12), p1.y.wrapping_add(y12)),
        Point::new(p2.x.wrapping_sub(x12), p2.y.wrapping_sub(y12)),
        Point::new(
            p2.x.wrapping_add(half_difference(x23, x12)),
            p2.y.wrapping_add(half_difference(y23, y12)),
        ),
    ]
}

#[cfg(test)]
mod tests;
