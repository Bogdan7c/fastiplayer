//! Адаптер `skrifa::outline::OutlinePen` -> [`Outline`]
//! (смысл `ass_outline_convert` из libass 0.17.5, `ass_outline.c`).
//!
//! libass получает от FreeType контур в 26.6 с тегами точек (on / conic / cubic) и сам
//! восстанавливает неявные on-curve точки между соседними conic-точками. skrifa уже отдаёт
//! готовые сегменты (`move_to` / `line_to` / `quad_to` / `curve_to` / `close`) с явными
//! on-curve точками, поэтому эта часть `ass_outline_convert` здесь не нужна.
//!
//! Что сохранено из libass:
//! - инверсия оси Y (шрифт: вверх, экран: вниз);
//! - контуры из менее чем трёх точек отбрасываются («degenerate 2-point contours from
//!   broken fonts»);
//! - любая координата вне `OUTLINE_MAX` отвергает весь глиф.
//!
//! ## Округление до 26.6
//! skrifa отдаёт `f32` в пикселях выбранного размера. FreeType масштабирует глиф в 26.6 и
//! округляет каждую точку; здесь каждая координата умножается на 64 и округляется
//! «от нуля» (`f32::round`), затем Y инвертируется уже округлённого значения — как libass
//! инвертирует готовую 26.6-координату FreeType.

use skrifa::outline::OutlinePen;

use super::{Outline, OutlineError, Point, SegmentKind};

/// Почему глиф не удалось превратить в контур.
pub(crate) type PenConversionError = OutlineError;

/// Собирает [`Outline`] из вызовов `OutlinePen`.
///
/// Первая ошибка запоминается, остальные вызовы игнорируются; результат забирает
/// [`Self::finish`], который эту ошибку возвращает (ничего не теряется молча).
#[derive(Debug, Default)]
pub(crate) struct GlyphOutlinePen {
    outline: Outline,
    /// Первая встреченная ошибка; после неё контур больше не строится.
    error: Option<OutlineError>,
    /// Состояние текущего контура; `None` вне контура.
    contour: Option<ContourState>,
}

/// Состояние контура между `move_to` и `close`.
#[derive(Debug, Clone, Copy)]
struct ContourState {
    /// Первая точка контура.
    start: Point,
    /// Текущая on-curve точка (конец последнего сегмента).
    current: Point,
    /// Размеры контура до начала (для отката вырожденного контура).
    points_before: usize,
    segments_before: usize,
    /// Число точек в терминах FreeType (для отбрасывания вырожденных контуров).
    freetype_points: usize,
}

impl GlyphOutlinePen {
    /// Новый пустой адаптер.
    pub(crate) fn new() -> Self {
        Self::default()
    }

    /// Завершает рисование (незакрытый контур закрывается как в FreeType) и отдаёт контур.
    pub(crate) fn finish(mut self) -> Result<Outline, PenConversionError> {
        self.close_current_contour();
        match self.error {
            Some(error) => Err(error),
            None => Ok(self.outline),
        }
    }

    /// Переводит координату шрифта (пиксели, Y вверх) в точку 26.6 (Y вниз).
    fn to_point(x: f32, y: f32) -> Point {
        // `as i32` насыщает; значения вне OUTLINE_MAX отвергает push_point.
        let scaled_x = (x * 64.0).round() as i32;
        let scaled_y = (y * 64.0).round() as i32;
        Point::new(scaled_x, scaled_y.saturating_neg())
    }

    /// Добавляет сегмент с владеемыми точками; ошибку диапазона запоминает.
    fn push_segment(&mut self, kind: SegmentKind, owned: &[Point], end: Point) {
        if self.error.is_some() {
            return;
        }
        let Some(contour) = self.contour.as_mut() else {
            // Сегмент без move_to: нарушение протокола пера, глиф отвергается.
            self.error = Some(OutlineError::Malformed);
            return;
        };
        if !end.is_in_outline_range() {
            self.error = Some(OutlineError::PointOutOfRange);
            return;
        }
        for point in owned {
            if let Err(error) = self.outline.push_point(*point) {
                self.error = Some(error);
                return;
            }
        }
        self.outline.push_segment(kind);
        contour.freetype_points += kind.owned_points();
        contour.current = end;
    }

    /// Закрывает текущий контур: замыкающая линия или вырожденный контур откатывается.
    fn close_current_contour(&mut self) {
        let Some(contour) = self.contour.take() else {
            return;
        };
        if self.error.is_some() {
            return;
        }
        // Меньше трёх точек — вырожденный контур сломанного шрифта: откатываем.
        if contour.freetype_points < 3 || self.outline.segments().len() == contour.segments_before {
            self.rollback(contour);
            return;
        }
        if contour.current == contour.start {
            // Последний сегмент уже вернулся в начало: он и замыкает контур.
            if !self.outline.close_contour() {
                self.error = Some(OutlineError::Malformed);
            }
            return;
        }
        // Иначе добавляем замыкающую линию из текущей точки в начало контура.
        if let Err(error) = self.outline.push_point(contour.current) {
            self.error = Some(error);
            return;
        }
        self.outline.push_segment(SegmentKind::Line);
        if !self.outline.close_contour() {
            self.error = Some(OutlineError::Malformed);
        }
    }

    /// Возвращает контур к размеру до `move_to`.
    fn rollback(&mut self, contour: ContourState) {
        self.outline
            .truncate(contour.points_before, contour.segments_before);
    }
}

impl OutlinePen for GlyphOutlinePen {
    fn move_to(&mut self, x: f32, y: f32) {
        // Новый move_to неявно закрывает предыдущий контур.
        self.close_current_contour();
        if self.error.is_some() {
            return;
        }
        let start = Self::to_point(x, y);
        if !start.is_in_outline_range() {
            self.error = Some(OutlineError::PointOutOfRange);
            return;
        }
        self.contour = Some(ContourState {
            start,
            current: start,
            points_before: self.outline.points().len(),
            segments_before: self.outline.segments().len(),
            freetype_points: 1,
        });
    }

    fn line_to(&mut self, x: f32, y: f32) {
        let Some(current) = self.contour.map(|contour| contour.current) else {
            self.error.get_or_insert(OutlineError::Malformed);
            return;
        };
        self.push_segment(SegmentKind::Line, &[current], Self::to_point(x, y));
    }

    fn quad_to(&mut self, cx0: f32, cy0: f32, x: f32, y: f32) {
        let Some(current) = self.contour.map(|contour| contour.current) else {
            self.error.get_or_insert(OutlineError::Malformed);
            return;
        };
        let control = Self::to_point(cx0, cy0);
        self.push_segment(
            SegmentKind::Quadratic,
            &[current, control],
            Self::to_point(x, y),
        );
    }

    fn curve_to(&mut self, cx0: f32, cy0: f32, cx1: f32, cy1: f32, x: f32, y: f32) {
        let Some(current) = self.contour.map(|contour| contour.current) else {
            self.error.get_or_insert(OutlineError::Malformed);
            return;
        };
        let first_control = Self::to_point(cx0, cy0);
        let second_control = Self::to_point(cx1, cy1);
        self.push_segment(
            SegmentKind::Cubic,
            &[current, first_control, second_control],
            Self::to_point(x, y),
        );
    }

    fn close(&mut self) {
        self.close_current_contour();
    }
}
