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
//

//! Круговые дуги обводки (libass 0.17.5: `process_arc`, `draw_arc`, `draw_circle`).
//!
//! Дуга строится цепочкой квадратичных сплайнов; число делений выбирается так, чтобы угол
//! одного сплайна не превышал `split_cos`.

use super::{DVector, SIDE_NEGATIVE, SideMask, StrokeError, StrokerState};
use crate::outline::{OutlineError, Point, SegmentKind};

/// Максимальная глубина деления дуги (libass: `max_subdiv`).
const MAX_SUBDIVISION: usize = 15;

impl StrokerState {
    /// Рекурсивно строит часть дуги (libass: `process_arc`).
    ///
    /// `multipliers[level]` — предвычисленный коэффициент для середины дуги на этом уровне.
    fn process_arc(
        &mut self,
        center_point: Point,
        normal0: DVector,
        normal1: DVector,
        multipliers: &[f64],
        level: usize,
        dir: SideMask,
    ) -> Result<(), StrokeError> {
        let multiplier = *multipliers.get(level).ok_or(OutlineError::Malformed)?;
        let middle = DVector {
            x: (normal0.x + normal1.x) * multiplier,
            y: (normal0.y + normal1.y) * multiplier,
        };
        if level > 0 {
            self.process_arc(center_point, normal0, middle, multipliers, level - 1, dir)?;
            return self.process_arc(center_point, middle, normal1, multipliers, level - 1, dir);
        }
        self.emit_point(center_point, normal0, Some(SegmentKind::Quadratic), dir)?;
        self.emit_point(center_point, middle, None, dir)
    }

    /// Таблица коэффициентов деления для дуги с косинусом угла `cosine`.
    ///
    /// Возвращает (таблица, позиция первого значимого элемента). Угол делится пополам, пока
    /// косинус не превысит `split_cos`.
    fn arc_multipliers(&self, mut cosine: f64) -> ([f64; MAX_SUBDIVISION + 1], usize) {
        let mut multipliers = [0.0_f64; MAX_SUBDIVISION + 1];
        let mut position = MAX_SUBDIVISION;
        while cosine < self.split_cos && position > 0 {
            let multiplier = 0.5_f64.sqrt() / (1.0 + cosine).sqrt();
            // Индекс безопасен: `position <= MAX_SUBDIVISION` по условию цикла.
            if let Some(slot) = multipliers.get_mut(position) {
                *slot = multiplier;
            }
            cosine = (1.0 + cosine) * multiplier;
            position -= 1;
        }
        if let Some(slot) = multipliers.get_mut(position) {
            *slot = 1.0 / (1.0 + cosine);
        }
        (multipliers, position)
    }

    /// Строит дугу между нормалями (libass: `draw_arc`); `cosine` — их скалярное произведение.
    pub(super) fn draw_arc(
        &mut self,
        center_point: Point,
        normal0: DVector,
        normal1: DVector,
        mut cosine: f64,
        dir: SideMask,
    ) -> Result<(), StrokeError> {
        let mut middle = DVector::default();
        let mut small_angle = true;
        if cosine < 0.0 {
            // Угол больше 90 градусов: дуга делится средней нормалью на две части.
            let mut multiplier = if dir & SIDE_NEGATIVE != 0 {
                -0.5_f64.sqrt()
            } else {
                0.5_f64.sqrt()
            };
            multiplier /= (1.0 - cosine).sqrt();
            middle.x = (normal1.y - normal0.y) * multiplier;
            middle.y = (normal0.x - normal1.x) * multiplier;
            cosine = (0.5 + 0.5 * cosine).max(0.0).sqrt();
            small_angle = false;
        }

        let (multipliers, position) = self.arc_multipliers(cosine);
        let table = multipliers.get(position..).ok_or(OutlineError::Malformed)?;
        let level = MAX_SUBDIVISION - position;
        if small_angle {
            self.process_arc(center_point, normal0, normal1, table, level, dir)
        } else {
            self.process_arc(center_point, normal0, middle, table, level, dir)?;
            self.process_arc(center_point, middle, normal1, table, level, dir)
        }
    }

    /// Строит полный круг вокруг точки (libass: `draw_circle`).
    pub(super) fn draw_circle(
        &mut self,
        center_point: Point,
        dir: SideMask,
    ) -> Result<(), StrokeError> {
        let (multipliers, position) = self.arc_multipliers(0.0);
        let table = multipliers.get(position..).ok_or(OutlineError::Malformed)?;
        let level = MAX_SUBDIVISION - position;
        let normals = [
            DVector { x: 1.0, y: 0.0 },
            DVector { x: 0.0, y: 1.0 },
            DVector { x: -1.0, y: 0.0 },
            DVector { x: 0.0, y: -1.0 },
        ];
        // Четыре четверти круга: каждая нормаль -> следующая по кругу.
        for (from, to) in normals.iter().zip(normals.iter().cycle().skip(1)) {
            self.process_arc(center_point, *from, *to, table, level, dir)?;
        }
        Ok(())
    }
}
