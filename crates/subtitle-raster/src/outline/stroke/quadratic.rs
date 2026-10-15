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

//! Обводка квадратичных сплайнов (libass 0.17.5: `estimate_quadratic_error`,
//! `process_quadratic`, `add_quadratic`).

use super::{
    DVector, Normal, SIDE_NEGATIVE, SIDE_POSITIVE, SideMask, StrokeError, StrokerState, vec_crs,
    vec_dot, vec_len,
};
use crate::outline::{Point, SegmentKind};

impl StrokerState {
    /// Оценка ошибки смещённого квадратичного сплайна (libass: `estimate_quadratic_error`).
    ///
    /// `c` и `s` — скалярное и псевдоскалярное произведения нормалей на концах. Возвращает
    /// лучшее смещение центральной точки либо `None`, если ошибка слишком велика.
    fn estimate_quadratic_error(&self, c: f64, s: f64, normal: &[Normal; 2]) -> Option<DVector> {
        // Радиальная ошибка.
        let radial_ok = (3.0 + c) * (3.0 + c) < self.err_q * (1.0 + c);
        if !radial_ok {
            return None;
        }
        let [normal0, normal1] = *normal;
        let mul = 1.0 / (1.0 + c);
        let l0 = 2.0 * normal0.len;
        let l1 = 2.0 * normal1.len;
        let dot0 = l0 + normal1.len * c;
        let crs0 = (l0 * mul - normal1.len) * s;
        let dot1 = l1 + normal0.len * c;
        let crs1 = (l1 * mul - normal0.len) * s;
        // Угловая ошибка. Отрицание условия сохраняет поведение при NaN, как в C.
        if !(crs0.abs() < self.err_a * dot0 && crs1.abs() < self.err_a * dot1) {
            return None;
        }
        Some(DVector {
            x: (normal0.v.x + normal1.v.x) * mul,
            y: (normal0.v.y + normal1.v.y) * mul,
        })
    }

    /// Рекурсивное построение смещённого квадратичного сплайна (libass: `process_quadratic`).
    ///
    /// `points` — три исходные точки, `deriv` — две разности соседних точек в нормальном
    /// пространстве, `normal` — нормали на концах. `first` — часть в начале сегмента.
    pub(super) fn process_quadratic(
        &mut self,
        points: &[Point; 3],
        deriv: &[DVector; 2],
        normal: &[Normal; 2],
        mut dir: SideMask,
        first: bool,
    ) -> Result<(), StrokeError> {
        let [point0, point1, point2] = *points;
        let [normal0, normal1] = *normal;
        let c = vec_dot(normal0.v, normal1.v);
        let s = vec_crs(normal0.v, normal1.v);
        let mut check_dir = dir;
        let skip_dir = if s < 0.0 {
            SIDE_POSITIVE
        } else {
            SIDE_NEGATIVE
        };
        if dir & skip_dir != 0 {
            let abs_s = s.abs();
            let f0 = normal0.len * c + normal1.len;
            let f1 = normal1.len * c + normal0.len;
            let g0 = normal0.len * abs_s;
            let g1 = normal1.len * abs_s;
            // Проверка самопересечения смещённой кривой.
            if f0 < abs_s && f1 < abs_s {
                let d2 = (f0 * normal1.len + f1 * normal0.len) / 2.0;
                if d2 < g0 && d2 < g1 {
                    self.prepare_skip(point0, skip_dir, first)?;
                    if f0 < 0.0 || f1 < 0.0 {
                        let zero = DVector::default();
                        self.emit_point(point0, zero, Some(SegmentKind::Line), skip_dir)?;
                        self.emit_point(point2, zero, Some(SegmentKind::Line), skip_dir)?;
                    } else {
                        let mul = f0 / abs_s;
                        let offs = DVector {
                            x: normal0.v.x * mul,
                            y: normal0.v.y * mul,
                        };
                        self.emit_point(point0, offs, Some(SegmentKind::Line), skip_dir)?;
                    }
                    dir &= !skip_dir;
                    if dir == 0 {
                        self.last_normal = normal1.v;
                        return Ok(());
                    }
                }
                check_dir ^= skip_dir;
            } else if c + g0 < 1.0 && c + g1 < 1.0 {
                check_dir ^= skip_dir;
            }
        }

        if check_dir != 0
            && let Some(result) = self.estimate_quadratic_error(c, s, normal)
        {
            self.emit_first_point(point0, SegmentKind::Quadratic, check_dir)?;
            self.emit_point(point1, result, None, check_dir)?;
            dir &= !check_dir;
            if dir == 0 {
                self.last_normal = normal1.v;
                return Ok(());
            }
        }

        // Деление сплайна пополам (точки: целочисленные средние со сдвигами как в C).
        let mut mid1 = Point::new(point0.x + point1.x, point0.y + point1.y);
        let mut mid3 = Point::new(point1.x + point2.x, point1.y + point2.y);
        let mid2 = Point::new((mid1.x + mid3.x + 2) >> 2, (mid1.y + mid3.y + 2) >> 2);
        mid1 = Point::new(mid1.x >> 1, mid1.y >> 1);
        mid3 = Point::new(mid3.x >> 1, mid3.y >> 1);

        let [deriv0, deriv1] = *deriv;
        let half0 = DVector {
            x: deriv0.x / 2.0,
            y: deriv0.y / 2.0,
        };
        let half2 = DVector {
            x: deriv1.x / 2.0,
            y: deriv1.y / 2.0,
        };
        let half1 = DVector {
            x: (half0.x + half2.x) / 2.0,
            y: (half0.y + half2.y) / 2.0,
        };

        let len = vec_len(half1);
        if len < self.min_len {
            // Вырожденный случай: производная в середине почти нулевая.
            self.emit_first_point(point0, SegmentKind::Line, dir)?;
            self.start_segment(mid2, normal1.v, dir)?;
            self.last_skip &= !dir;
            return self.emit_point(mid2, normal1.v, Some(SegmentKind::Line), dir);
        }

        let scale = 1.0 / len;
        let middle_normal = Normal {
            v: DVector {
                x: half1.x * scale,
                y: half1.y * scale,
            },
            len,
        };
        let first_half = [
            Normal {
                v: normal0.v,
                len: normal0.len / 2.0,
            },
            middle_normal,
        ];
        let second_half = [
            middle_normal,
            Normal {
                v: normal1.v,
                len: normal1.len / 2.0,
            },
        ];
        self.process_quadratic(
            &[point0, mid1, mid2],
            &[half0, half1],
            &first_half,
            dir,
            first,
        )?;
        self.process_quadratic(
            &[mid2, mid3, point2],
            &[half1, half2],
            &second_half,
            dir,
            false,
        )
    }

    /// Обрабатывает исходный квадратичный сплайн (libass: `add_quadratic`).
    pub(super) fn add_quadratic(
        &mut self,
        control: Point,
        end: Point,
        dir: SideMask,
    ) -> Result<(), StrokeError> {
        let dx0 = control.x - self.last_point.x;
        let dy0 = control.y - self.last_point.y;
        if self.is_negligible(dx0, dy0) {
            return self.add_line(end, dir);
        }
        let dx1 = end.x - control.x;
        let dy1 = end.y - control.y;
        if self.is_negligible(dx1, dy1) {
            return self.add_line(end, dir);
        }

        let points = [self.last_point, control, end];
        self.last_point = end;

        let deriv = [
            DVector {
                x: f64::from(dy0) * self.yscale,
                y: -f64::from(dx0) * self.xscale,
            },
            DVector {
                x: f64::from(dy1) * self.yscale,
                y: -f64::from(dx1) * self.xscale,
            },
        ];
        let len0 = vec_len(deriv[0]);
        let scale0 = 1.0 / len0;
        let len1 = vec_len(deriv[1]);
        let scale1 = 1.0 / len1;
        let normal = [
            Normal {
                v: DVector {
                    x: deriv[0].x * scale0,
                    y: deriv[0].y * scale0,
                },
                len: len0,
            },
            Normal {
                v: DVector {
                    x: deriv[1].x * scale1,
                    y: deriv[1].y * scale1,
                },
                len: len1,
            },
        ];

        let first = self.contour_start;
        self.start_segment(points[0], normal[0].v, dir)?;
        self.process_quadratic(&points, &deriv, &normal, dir, first)
    }
}
