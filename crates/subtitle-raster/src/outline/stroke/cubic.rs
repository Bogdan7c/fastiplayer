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

//! Обводка кубических сплайнов (libass 0.17.5: `estimate_cubic_error`, `process_cubic`,
//! `add_cubic`).

use super::{
    DVector, Normal, SIDE_NEGATIVE, SIDE_POSITIVE, SideMask, StrokeError, StrokerState, vec_crs,
    vec_dot, vec_len,
};
use crate::outline::{Point, SegmentKind};

// Флаги условий пересечения/обрезки смещённого сплайна (libass: безымянный enum в ass_outline.c).
// Младшие 6 бит — ожидаемое значение условия, старшие (сдвиг FLAG_COUNT) — маска «проверять».
const FLAG_INTERSECTION: i32 = 1;
const FLAG_ZERO_0: i32 = 2;
const FLAG_ZERO_1: i32 = 4;
const FLAG_CLIP_0: i32 = 8;
const FLAG_CLIP_1: i32 = 16;
const FLAG_DIR_2: i32 = 32;
const FLAG_COUNT: i32 = 6;
const MASK_INTERSECTION: i32 = FLAG_INTERSECTION << FLAG_COUNT;
const MASK_ZERO_0: i32 = FLAG_ZERO_0 << FLAG_COUNT;
const MASK_ZERO_1: i32 = FLAG_ZERO_1 << FLAG_COUNT;
const MASK_CLIP_0: i32 = FLAG_CLIP_0 << FLAG_COUNT;
const MASK_CLIP_1: i32 = FLAG_CLIP_1 << FLAG_COUNT;

/// Взаимное расположение нормалей на концах сплайна и разности его центральных точек.
#[derive(Clone, Copy)]
struct CubicGeometry {
    /// Скалярное произведение нормалей на концах.
    c: f64,
    /// Псевдоскалярное произведение нормалей на концах.
    s: f64,
    /// Скалярные произведения нормалей и разности центральных точек (нормальное пространство).
    dc: [f64; 2],
    /// Псевдоскалярные произведения нормалей и разности центральных точек.
    ds: [f64; 2],
}

/// Результат успешной оценки кубического сплайна: границы, не требующие деления, и смещения
/// двух центральных контрольных точек.
struct CubicEstimate {
    sides: SideMask,
    offsets: [DVector; 2],
}

impl StrokerState {
    /// Оценка ошибки смещённого кубического сплайна (libass: `estimate_cubic_error`).
    ///
    /// `None` — ни одну границу нельзя принять без деления.
    fn estimate_cubic_error(
        &self,
        geometry: CubicGeometry,
        normal: &[Normal; 2],
        check_flags: i32,
        mut dir: SideMask,
    ) -> Option<CubicEstimate> {
        let CubicGeometry { c, s, dc, ds } = geometry;
        let [normal0, normal1] = *normal;
        let t = (ds[0] + ds[1]) / (dc[0] + dc[1]);
        let c1 = 1.0 + c;
        let ss = s * s;
        let ts = t * s;
        let tt = t * t;
        let ttc = tt * c1;
        let ttcc = ttc * c1;

        const W: f64 = 0.4;
        let f0 = [
            10.0 * W * (c - 1.0) + 9.0 * W * tt * c,
            2.0 * (c - 1.0) + 3.0 * tt + 2.0 * ts,
            2.0 * (c - 1.0) + 3.0 * tt - 2.0 * ts,
        ];
        let f1 = [
            18.0 * W * (ss - ttc * c),
            2.0 * ss - 6.0 * ttc - 2.0 * ts * (c + 4.0),
            2.0 * ss - 6.0 * ttc + 2.0 * ts * (c + 4.0),
        ];
        let f2 = [
            9.0 * W * (ttcc - ss) * c,
            3.0 * ss + 3.0 * ttcc + 6.0 * ts * c1,
            3.0 * ss + 3.0 * ttcc - 6.0 * ts * c1,
        ];

        let mut aa = 0.0;
        let mut ab = 0.0;
        let ch = (c1 / 2.0).sqrt();
        let inv_ro0 = 1.5 * ch * (ch + 1.0);
        for ((&f0_i, &f1_i), &f2_i) in f0.iter().zip(&f1).zip(&f2) {
            let a = 2.0 * f2_i + f1_i * inv_ro0;
            let b = f2_i - f0_i * inv_ro0 * inv_ro0;
            aa += a * a;
            ab += a * b;
        }
        let ro = ab / (aa * inv_ro0 + 1e-9); // лучшее приближение

        let mut err2 = 0.0;
        for ((&f0_i, &f1_i), &f2_i) in f0.iter().zip(&f1).zip(&f2) {
            let err = f0_i + ro * (f1_i + ro * f2_i);
            err2 += err * err;
        }
        // Радиальная ошибка; сравнение вынесено, чтобы NaN отвергался как в C.
        let radial_ok = err2 < self.err_c;
        if !radial_ok {
            return None;
        }

        let r = ro * c1 - 1.0;
        let ro0 = t * r - ro * s;
        let ro1 = t * r + ro * s;

        let check_dir = if check_flags & FLAG_DIR_2 != 0 {
            SIDE_NEGATIVE
        } else {
            SIDE_POSITIVE
        };
        if dir & check_dir != 0 {
            let (mut test_s, mut test0, mut test1) = (s, ro0, ro1);
            if check_flags & FLAG_DIR_2 != 0 {
                test_s = -test_s;
                test0 = -test0;
                test1 = -test1;
            }
            let mut flags = 0;
            if 2.0 * test_s * r < dc[0] + dc[1] {
                flags |= FLAG_INTERSECTION;
            }
            if normal0.len - test0 < 0.0 {
                flags |= FLAG_ZERO_0;
            }
            if normal1.len + test1 < 0.0 {
                flags |= FLAG_ZERO_1;
            }
            if normal0.len + dc[0] + test_s - test1 * c < 0.0 {
                flags |= FLAG_CLIP_0;
            }
            if normal1.len + dc[1] + test_s + test0 * c < 0.0 {
                flags |= FLAG_CLIP_1;
            }
            if (flags ^ check_flags) & (check_flags >> FLAG_COUNT) != 0 {
                dir &= !check_dir;
                if dir == 0 {
                    return None;
                }
            }
        }

        let d0c = 2.0 * dc[0];
        let d0s = 2.0 * ds[0];
        let d1c = 2.0 * dc[1];
        let d1s = 2.0 * ds[1];
        let mut dot0 = d0c + 3.0 * normal0.len;
        let mut crs0 = d0s + 3.0 * ro0 * normal0.len;
        let mut dot1 = d1c + 3.0 * normal1.len;
        let mut crs1 = d1s + 3.0 * ro1 * normal1.len;
        // Угловая ошибка (этап 1).
        if !(crs0.abs() < self.err_a * dot0 && crs1.abs() < self.err_a * dot1) {
            return None;
        }

        let cl0 = c * normal0.len;
        let sl0 = s * normal0.len;
        let cl1 = c * normal1.len;
        let sl1 = -s * normal1.len;
        dot0 = d0c - ro0 * d0s + cl0 + ro1 * sl0 + cl1 / 3.0;
        dot1 = d1c - ro1 * d1s + cl1 + ro0 * sl1 + cl0 / 3.0;
        crs0 = d0s + ro0 * d0c - sl0 + ro1 * cl0 - sl1 / 3.0;
        crs1 = d1s + ro1 * d1c - sl1 + ro0 * cl1 - sl0 / 3.0;
        // Угловая ошибка (этап 2).
        if !(crs0.abs() < self.err_a * dot0 && crs1.abs() < self.err_a * dot1) {
            return None;
        }

        Some(CubicEstimate {
            sides: dir,
            offsets: [
                DVector {
                    x: normal0.v.x + normal0.v.y * ro0,
                    y: normal0.v.y - normal0.v.x * ro0,
                },
                DVector {
                    x: normal1.v.x + normal1.v.y * ro1,
                    y: normal1.v.y - normal1.v.x * ro1,
                },
            ],
        })
    }

    /// Рекурсивное построение смещённого кубического сплайна (libass: `process_cubic`).
    ///
    /// `points` — четыре исходные точки, `deriv` — три разности соседних точек в нормальном
    /// пространстве, `normal` — нормали на концах, `first` — часть в начале сегмента.
    pub(super) fn process_cubic(
        &mut self,
        points: &[Point; 4],
        deriv: &[DVector; 3],
        normal: &[Normal; 2],
        mut dir: SideMask,
        first: bool,
    ) -> Result<(), StrokeError> {
        let [point0, point1, point2, point3] = *points;
        let [normal0, normal1] = *normal;
        let c = vec_dot(normal0.v, normal1.v);
        let s = vec_crs(normal0.v, normal1.v);
        let dc = [vec_dot(normal0.v, deriv[1]), vec_dot(normal1.v, deriv[1])];
        let ds = [vec_crs(normal0.v, deriv[1]), vec_crs(normal1.v, deriv[1])];
        let f0 = normal0.len * c + normal1.len + dc[1];
        let f1 = normal1.len * c + normal0.len + dc[0];
        let mut g0 = normal0.len * s - ds[1];
        let mut g1 = normal1.len * s + ds[0];

        let mut abs_s = s;
        let mut check_dir = dir;
        let mut skip_dir = SIDE_NEGATIVE;
        let mut flags = FLAG_INTERSECTION | FLAG_DIR_2;
        if s < 0.0 {
            abs_s = -s;
            skip_dir = SIDE_POSITIVE;
            flags = 0;
            g0 = -g0;
            g1 = -g1;
        }

        let central_points_diverge = dc[0] + dc[1] > 0.0;
        if !central_points_diverge {
            check_dir = 0;
        } else if dir & skip_dir != 0 {
            if f0 < abs_s && f1 < abs_s {
                // Проверка самопересечения.
                let mut d2 = (f0 + dc[1]) * normal1.len + (f1 + dc[0]) * normal0.len;
                d2 = (d2 + vec_dot(deriv[1], deriv[1])) / 2.0;
                if d2 < g0 && d2 < g1 {
                    let mut q = (d2 / (2.0 - d2)).sqrt();
                    let h0 = (f0 * q + g0) * normal1.len;
                    let h1 = (f1 * q + g1) * normal0.len;
                    q *= (4.0 / 3.0) * d2;
                    if h0 > q && h1 > q {
                        self.prepare_skip(point0, skip_dir, first)?;
                        if f0 < 0.0 || f1 < 0.0 {
                            let zero = DVector::default();
                            self.emit_point(point0, zero, Some(SegmentKind::Line), skip_dir)?;
                            self.emit_point(point3, zero, Some(SegmentKind::Line), skip_dir)?;
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
                }
                check_dir ^= skip_dir;
            } else {
                if ds[0] < 0.0 {
                    flags ^= MASK_INTERSECTION;
                }
                if ds[1] < 0.0 {
                    flags ^= MASK_INTERSECTION | FLAG_INTERSECTION;
                }
                let parallel = flags & MASK_INTERSECTION != 0;
                let mut badness = if parallel { 0 } else { 1 };
                if c + g0 < 1.0 {
                    if parallel {
                        flags ^= MASK_ZERO_0 | FLAG_ZERO_0;
                        if c < 0.0 {
                            flags ^= MASK_CLIP_0;
                        }
                        if f0 > abs_s {
                            flags ^= FLAG_ZERO_0 | FLAG_CLIP_0;
                        }
                    }
                    badness += 1;
                } else {
                    flags ^= MASK_INTERSECTION | FLAG_INTERSECTION;
                    if !parallel {
                        flags ^= MASK_ZERO_0;
                        if c > 0.0 {
                            flags ^= MASK_CLIP_0;
                        }
                    }
                }
                if c + g1 < 1.0 {
                    if parallel {
                        flags ^= MASK_ZERO_1 | FLAG_ZERO_1;
                        if c < 0.0 {
                            flags ^= MASK_CLIP_1;
                        }
                        if f1 > abs_s {
                            flags ^= FLAG_ZERO_1 | FLAG_CLIP_1;
                        }
                    }
                    badness += 1;
                } else {
                    flags ^= MASK_INTERSECTION;
                    if !parallel {
                        flags ^= MASK_ZERO_1;
                        if c > 0.0 {
                            flags ^= MASK_CLIP_1;
                        }
                    }
                }
                if badness > 2 {
                    check_dir ^= skip_dir;
                }
            }
        }

        if check_dir != 0 {
            let geometry = CubicGeometry { c, s, dc, ds };
            if let Some(estimate) = self.estimate_cubic_error(geometry, normal, flags, check_dir) {
                let accepted = estimate.sides;
                self.emit_first_point(point0, SegmentKind::Cubic, accepted)?;
                self.emit_point(point1, estimate.offsets[0], None, accepted)?;
                self.emit_point(point2, estimate.offsets[1], None, accepted)?;
                dir &= !accepted;
                if dir == 0 {
                    self.last_normal = normal1.v;
                    return Ok(());
                }
            }
        }

        // Деление сплайна пополам (целочисленные сдвиги — как в libass).
        let mut next1 = Point::new(point0.x + point1.x, point0.y + point1.y);
        let center = Point::new(point1.x + point2.x + 2, point1.y + point2.y + 2);
        let mut next5 = Point::new(point2.x + point3.x, point2.y + point3.y);
        let mut next2 = Point::new(next1.x + center.x, next1.y + center.y);
        let mut next4 = Point::new(center.x + next5.x, center.y + next5.y);
        let next3 = Point::new((next2.x + next4.x - 1) >> 3, (next2.y + next4.y - 1) >> 3);
        next2 = Point::new(next2.x >> 2, next2.y >> 2);
        next4 = Point::new(next4.x >> 2, next4.y >> 2);
        next1 = Point::new(next1.x >> 1, next1.y >> 1);
        next5 = Point::new(next5.x >> 1, next5.y >> 1);
        let next = [point0, next1, next2, next3, next4, next5, point3];

        let half = |vector: DVector| DVector {
            x: vector.x / 2.0,
            y: vector.y / 2.0,
        };
        let average = |a: DVector, b: DVector| DVector {
            x: (a.x + b.x) / 2.0,
            y: (a.y + b.y) / 2.0,
        };
        let deriv_a = half(deriv[0]);
        let center_deriv = half(deriv[1]);
        let deriv_e = half(deriv[2]);
        let deriv_b = average(deriv_a, center_deriv);
        let deriv_d = average(center_deriv, deriv_e);
        let deriv_c = average(deriv_b, deriv_d);

        let len = vec_len(deriv_c);
        let [n0, n1, n2, n3, n4, n5, n6] = next;
        if len < self.min_len {
            // Вырожденный случай: производная в середине почти нулевая.
            let outer_first = Normal {
                v: normal0.v,
                len: normal0.len / 2.0,
            };
            let outer_last = Normal {
                v: normal1.v,
                len: normal1.len / 2.0,
            };
            // Центральную производную «раздаём» соседям и обнуляем (как в libass).
            let deriv_b = DVector {
                x: deriv_b.x + deriv_c.x,
                y: deriv_b.y + deriv_c.y,
            };
            let deriv_d = DVector {
                x: deriv_d.x + deriv_c.x,
                y: deriv_d.y + deriv_c.y,
            };
            let deriv_c = DVector::default();

            let len1 = vec_len(deriv_b);
            let first_inner = if len1 < self.min_len {
                normal0
            } else {
                let scale = 1.0 / len1;
                Normal {
                    v: DVector {
                        x: deriv_b.x * scale,
                        y: deriv_b.y * scale,
                    },
                    len: len1,
                }
            };
            let len2 = vec_len(deriv_d);
            let second_inner = if len2 < self.min_len {
                normal1
            } else {
                let scale = 1.0 / len2;
                Normal {
                    v: DVector {
                        x: deriv_d.x * scale,
                        y: deriv_d.y * scale,
                    },
                    len: len2,
                }
            };
            // Нормали половин: [outer_first, first_inner] и [second_inner, outer_last].
            if len1 < self.min_len {
                self.emit_first_point(n0, SegmentKind::Line, dir)?;
            } else {
                self.process_cubic(
                    &[n0, n1, n2, n3],
                    &[deriv_a, deriv_b, deriv_c],
                    &[outer_first, first_inner],
                    dir,
                    first,
                )?;
            }
            self.start_segment(n2, second_inner.v, dir)?;
            if len2 < self.min_len {
                self.emit_first_point(n3, SegmentKind::Line, dir)?;
            } else {
                self.process_cubic(
                    &[n3, n4, n5, n6],
                    &[deriv_c, deriv_d, deriv_e],
                    &[second_inner, outer_last],
                    dir,
                    false,
                )?;
            }
            return Ok(());
        }

        let scale = 1.0 / len;
        let middle_normal = Normal {
            v: DVector {
                x: deriv_c.x * scale,
                y: deriv_c.y * scale,
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
        self.process_cubic(
            &[n0, n1, n2, n3],
            &[deriv_a, deriv_b, deriv_c],
            &first_half,
            dir,
            first,
        )?;
        self.process_cubic(
            &[n3, n4, n5, n6],
            &[deriv_c, deriv_d, deriv_e],
            &second_half,
            dir,
            false,
        )
    }

    /// Обрабатывает исходный кубический сплайн (libass: `add_cubic`).
    pub(super) fn add_cubic(
        &mut self,
        control1: Point,
        control2: Point,
        end: Point,
        dir: SideMask,
    ) -> Result<(), StrokeError> {
        // Выбор пар точек для производных. Если первая контрольная точка совпала с началом,
        // начальная производная берётся до второй контрольной (и наоборот на конце).
        // `derivative_pair` кодирует выбранные точки так же, как `flags` в libass (9 / 8 / 13).
        let start = self.last_point;
        let mut derivative_pair = DerivativePair::ControlToControl;
        let mut dx0 = control1.x - start.x;
        let mut dy0 = control1.y - start.y;
        if self.is_negligible(dx0, dy0) {
            dx0 = control2.x - start.x;
            dy0 = control2.y - start.y;
            if self.is_negligible(dx0, dy0) {
                return self.add_line(end, dir);
            }
            derivative_pair = DerivativePair::SecondControlToStart;
        }

        let mut dx2 = end.x - control2.x;
        let mut dy2 = end.y - control2.y;
        if self.is_negligible(dx2, dy2) {
            dx2 = end.x - control1.x;
            dy2 = end.y - control1.y;
            if self.is_negligible(dx2, dy2) {
                return self.add_line(end, dir);
            }
            derivative_pair = match derivative_pair {
                DerivativePair::ControlToControl => DerivativePair::EndToFirstControl,
                // Обе пары заменены: в libass это `flags == 12` -> прямая линия.
                DerivativePair::SecondControlToStart | DerivativePair::EndToFirstControl => {
                    return self.add_line(end, dir);
                }
            };
        }

        let points = [start, control1, control2, end];
        self.last_point = end;

        let (dx1, dy1) = match derivative_pair {
            DerivativePair::ControlToControl => (control2.x - control1.x, control2.y - control1.y),
            DerivativePair::SecondControlToStart => (control2.x - start.x, control2.y - start.y),
            DerivativePair::EndToFirstControl => (end.x - control1.x, end.y - control1.y),
        };

        let deriv = [
            DVector {
                x: f64::from(dy0) * self.yscale,
                y: -f64::from(dx0) * self.xscale,
            },
            DVector {
                x: f64::from(dy1) * self.yscale,
                y: -f64::from(dx1) * self.xscale,
            },
            DVector {
                x: f64::from(dy2) * self.yscale,
                y: -f64::from(dx2) * self.xscale,
            },
        ];
        let len0 = vec_len(deriv[0]);
        let scale0 = 1.0 / len0;
        let len2 = vec_len(deriv[2]);
        let scale2 = 1.0 / len2;
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
                    x: deriv[2].x * scale2,
                    y: deriv[2].y * scale2,
                },
                len: len2,
            },
        ];

        let first = self.contour_start;
        self.start_segment(points[0], normal[0].v, dir)?;
        self.process_cubic(&points, &deriv, &normal, dir, first)
    }
}

/// Какими точками задана средняя производная кубического сплайна (libass: переменная `flags`).
#[derive(Clone, Copy)]
enum DerivativePair {
    /// Обычный случай: от первой контрольной ко второй (`flags == 9`).
    ControlToControl,
    /// Первая контрольная совпала с началом: от начала ко второй контрольной (`flags == 8`).
    SecondControlToStart,
    /// Вторая контрольная совпала с концом: от первой контрольной к концу (`flags == 13`).
    EndToFirstControl,
}
