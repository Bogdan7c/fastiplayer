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

//! Сегмент ломаной растеризатора и разбиение списков сегментов линиями
//! (libass 0.17.5: `struct segment`, `add_line`, `segment_*`, `polyline_split_horz/vert`).
//!
//! Каждый отрезок хранится как полуплоскость `a*x + b*y < c` (нормализованная так, что
//! `max(|a|, |b|) * scale ~ 1 << 60`; в комментарии libass фигурирует `1 << 61` для
//! удвоенного максимума, которым фактически считается `scale`) плюс охватывающий прямоугольник и флаги «точные границы».
//! Все числа целочисленные, как в C; умножения i64, которые теоретически могли бы
//! переполниться, выполнены через `wrapping_*`, чтобы повторить поведение C без паники в
//! debug-сборке (при входах, ограниченных `OUTLINE_MAX`, переполнения не возникает).

use crate::outline::Point;

/// Отрезок идёт вниз (libass: `SEGFLAG_DN`).
pub(super) const SEGFLAG_DN: u8 = 1;
/// Отрезок идёт из левого верхнего угла в правый нижний (`SEGFLAG_UL_DR`).
pub(super) const SEGFLAG_UL_DR: u8 = 2;
/// Левая граница прямоугольника отрезка точная (`SEGFLAG_EXACT_LEFT`).
pub(super) const SEGFLAG_EXACT_LEFT: u8 = 4;
/// Правая граница точная (`SEGFLAG_EXACT_RIGHT`).
pub(super) const SEGFLAG_EXACT_RIGHT: u8 = 8;
/// Верхняя граница точная (`SEGFLAG_EXACT_TOP`).
pub(super) const SEGFLAG_EXACT_TOP: u8 = 16;
/// Нижняя граница точная (`SEGFLAG_EXACT_BOTTOM`).
pub(super) const SEGFLAG_EXACT_BOTTOM: u8 = 32;

/// Отрезок ломаной (libass: `struct segment`).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) struct Segment {
    /// Свободный член полуплоскости.
    pub(super) c: i64,
    /// Коэффициент при x.
    pub(super) a: i32,
    /// Коэффициент при y.
    pub(super) b: i32,
    /// Множитель нормализации для заливки полуплоскости.
    pub(super) scale: i32,
    /// Набор `SEGFLAG_*`.
    pub(super) flags: u8,
    /// Левая граница охватывающего прямоугольника (1/64 пикселя, в координатах текущего окна).
    pub(super) x_min: i32,
    /// Правая граница.
    pub(super) x_max: i32,
    /// Верхняя граница.
    pub(super) y_min: i32,
    /// Нижняя граница.
    pub(super) y_max: i32,
}

/// Линия, которой делится список сегментов.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) enum SplitLine {
    /// Вертикальная линия `x = position`: делит на левую и правую части
    /// (libass: `polyline_split_horz`).
    Vertical(i32),
    /// Горизонтальная линия `y = position`: делит на верхнюю и нижнюю части
    /// (libass: `polyline_split_vert`).
    Horizontal(i32),
}

/// Сколько сегментов каждой из двух групп попало в каждую часть разбиения.
///
/// Группы — «основной контур» и «дополнительный» (`rst->n_first` в libass); порядок внутри
/// части сохраняется: сначала все сегменты группы 0, потом группы 1.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub(super) struct SplitCounts {
    /// Часть до линии (слева/сверху).
    pub(super) before: [usize; 2],
    /// Часть после линии (справа/снизу).
    pub(super) after: [usize; 2],
}

/// Целочисленный логарифм по основанию 2 (libass: `ilog2`); для нуля возвращает 0.
pub(super) fn floor_log2(value: u32) -> u32 {
    value.checked_ilog2().unwrap_or(0)
}

impl Segment {
    /// Строит отрезок `start -> end`; нулевая длина даёт `None` (libass: `add_line`).
    pub(super) fn from_line(start: Point, end: Point) -> Option<Self> {
        let x = end.x.wrapping_sub(start.x);
        let y = end.y.wrapping_sub(start.y);
        if x == 0 && y == 0 {
            return None;
        }

        let mut flags =
            SEGFLAG_EXACT_LEFT | SEGFLAG_EXACT_RIGHT | SEGFLAG_EXACT_TOP | SEGFLAG_EXACT_BOTTOM;
        if x < 0 {
            flags ^= SEGFLAG_UL_DR;
        }
        if y >= 0 {
            flags ^= SEGFLAG_DN | SEGFLAG_UL_DR;
        }

        let a = y;
        let b = x.wrapping_neg();
        let c = i64::from(y) * i64::from(start.x) - i64::from(x) * i64::from(start.y);

        // Нормализация полуплоскости: старший из |x|, |y| доводится до 2^30..2^31.
        let abs_x = x.unsigned_abs();
        let abs_y = y.unsigned_abs();
        let mut max_ab = abs_x.max(abs_y);
        let shift = 30 - floor_log2(max_ab);
        max_ab <<= shift + 1;
        let multiplier = 1_i32 << shift;
        let a = a.wrapping_mul(multiplier);
        let b = b.wrapping_mul(multiplier);
        let c = c.wrapping_mul(i64::from(multiplier));

        // scale = 2^61 / max_ab, приближённо полиномом (libass считает без деления).
        let squared_high = ((u64::from(max_ab) * u64::from(max_ab)) >> 32) as u32;
        let first = ((0x5333_3333_u64 * u64::from(squared_high)) >> 32) as u32;
        let correction = 0x8810_624D_u64.wrapping_sub((0xBBC6_A7EF_u64 * u64::from(max_ab)) >> 32);
        let scale = first.wrapping_add(correction as u32) as i32;

        Some(Self {
            c,
            a,
            b,
            scale,
            flags,
            x_min: start.x.min(end.x),
            x_max: start.x.max(end.x),
            y_min: start.y.min(end.y),
            y_max: start.y.max(end.y),
        })
    }

    /// Сдвигает начало координат по X на `x` с обрезкой слева (libass: `segment_move_x`).
    fn move_x(&mut self, x: i32) {
        self.x_min = self.x_min.wrapping_sub(x);
        self.x_max = self.x_max.wrapping_sub(x);
        self.x_min = self.x_min.max(0);
        self.c = self
            .c
            .wrapping_sub(i64::from(self.a).wrapping_mul(i64::from(x)));

        const TEST: u8 = SEGFLAG_EXACT_LEFT | SEGFLAG_UL_DR;
        if self.x_min == 0 && self.flags & TEST == TEST {
            self.flags &= !SEGFLAG_EXACT_TOP;
        }
    }

    /// Сдвигает начало координат по Y на `y` с обрезкой сверху (libass: `segment_move_y`).
    fn move_y(&mut self, y: i32) {
        self.y_min = self.y_min.wrapping_sub(y);
        self.y_max = self.y_max.wrapping_sub(y);
        self.y_min = self.y_min.max(0);
        self.c = self
            .c
            .wrapping_sub(i64::from(self.b).wrapping_mul(i64::from(y)));

        const TEST: u8 = SEGFLAG_EXACT_TOP | SEGFLAG_UL_DR;
        if self.y_min == 0 && self.flags & TEST == TEST {
            self.flags &= !SEGFLAG_EXACT_LEFT;
        }
    }

    /// Режет вертикальной линией `x`: `self` остаётся левой частью, правая возвращается
    /// (libass: `segment_split_horz`). Требует `x_min < x < x_max`.
    fn split_at_x(&mut self, x: i32) -> Self {
        let mut next = *self;
        next.c = next
            .c
            .wrapping_sub(i64::from(self.a).wrapping_mul(i64::from(x)));
        next.x_min = 0;
        next.x_max = next.x_max.wrapping_sub(x);
        self.x_max = x;

        self.flags &= !SEGFLAG_EXACT_TOP;
        next.flags &= !SEGFLAG_EXACT_BOTTOM;
        if self.flags & SEGFLAG_UL_DR != 0 {
            std::mem::swap(&mut self.flags, &mut next.flags);
        }
        self.flags |= SEGFLAG_EXACT_RIGHT;
        next.flags |= SEGFLAG_EXACT_LEFT;
        next
    }

    /// Режет горизонтальной линией `y`: `self` — верхняя часть, нижняя возвращается
    /// (libass: `segment_split_vert`). Требует `y_min < y < y_max`.
    fn split_at_y(&mut self, y: i32) -> Self {
        let mut next = *self;
        next.c = next
            .c
            .wrapping_sub(i64::from(self.b).wrapping_mul(i64::from(y)));
        next.y_min = 0;
        next.y_max = next.y_max.wrapping_sub(y);
        self.y_max = y;

        self.flags &= !SEGFLAG_EXACT_LEFT;
        next.flags &= !SEGFLAG_EXACT_RIGHT;
        if self.flags & SEGFLAG_UL_DR != 0 {
            std::mem::swap(&mut self.flags, &mut next.flags);
        }
        self.flags |= SEGFLAG_EXACT_BOTTOM;
        next.flags |= SEGFLAG_EXACT_TOP;
        next
    }

    /// Значение `c - a*x - b*y` с обёрткой i64 (общая часть проверок положения).
    fn halfplane_residual(&self, x_term: i64, y_term: i64) -> i64 {
        self.c.wrapping_sub(x_term).wrapping_sub(y_term)
    }

    /// Весь сегмент не левее линии `x` (libass: `segment_check_left`).
    fn lies_right_of(&self, x: i32) -> bool {
        if self.flags & SEGFLAG_EXACT_LEFT != 0 {
            return self.x_min >= x;
        }
        let y = if self.flags & SEGFLAG_UL_DR != 0 {
            self.y_min
        } else {
            self.y_max
        };
        let mut cc = self.halfplane_residual(
            i64::from(self.a).wrapping_mul(i64::from(x)),
            i64::from(self.b).wrapping_mul(i64::from(y)),
        );
        if self.a < 0 {
            cc = cc.wrapping_neg();
        }
        cc >= 0
    }

    /// Весь сегмент не правее линии `x` (libass: `segment_check_right`).
    fn lies_left_of(&self, x: i32) -> bool {
        if self.flags & SEGFLAG_EXACT_RIGHT != 0 {
            return self.x_max <= x;
        }
        let y = if self.flags & SEGFLAG_UL_DR != 0 {
            self.y_max
        } else {
            self.y_min
        };
        let mut cc = self.halfplane_residual(
            i64::from(self.a).wrapping_mul(i64::from(x)),
            i64::from(self.b).wrapping_mul(i64::from(y)),
        );
        if self.a > 0 {
            cc = cc.wrapping_neg();
        }
        cc >= 0
    }

    /// Весь сегмент не выше линии `y` (libass: `segment_check_top`).
    fn lies_below(&self, y: i32) -> bool {
        if self.flags & SEGFLAG_EXACT_TOP != 0 {
            return self.y_min >= y;
        }
        let x = if self.flags & SEGFLAG_UL_DR != 0 {
            self.x_min
        } else {
            self.x_max
        };
        let mut cc = self.halfplane_residual(
            i64::from(self.b).wrapping_mul(i64::from(y)),
            i64::from(self.a).wrapping_mul(i64::from(x)),
        );
        if self.b < 0 {
            cc = cc.wrapping_neg();
        }
        cc >= 0
    }

    /// Весь сегмент не ниже линии `y` (libass: `segment_check_bottom`).
    fn lies_above(&self, y: i32) -> bool {
        if self.flags & SEGFLAG_EXACT_BOTTOM != 0 {
            return self.y_max <= y;
        }
        let x = if self.flags & SEGFLAG_UL_DR != 0 {
            self.x_max
        } else {
            self.x_min
        };
        let mut cc = self.halfplane_residual(
            i64::from(self.b).wrapping_mul(i64::from(y)),
            i64::from(self.a).wrapping_mul(i64::from(x)),
        );
        if self.b > 0 {
            cc = cc.wrapping_neg();
        }
        cc >= 0
    }
}

/// Что стало с одним сегментом после деления линией.
enum Placement {
    /// Сегмент целиком за пределами «до»-части и не нужен ни в одной из частей (`continue` в C).
    Dropped,
    /// Сегмент остаётся только в части «до» линии.
    Before(Segment),
    /// Сегмент уходит только в часть «после» линии (уже сдвинут в её координаты).
    After(Segment),
    /// Линия пересекает сегмент: он разрезан на обе части.
    Both(Segment, Segment),
}

/// Делит ОДИН сегмент линией и обновляет намотку (тело цикла `polyline_split_horz/vert`).
///
/// Вынесено из циклов, чтобы внешний и внутренний (на месте) варианты деления не расходились.
#[inline(always)]
fn split_one(
    mut segment: Segment,
    group: usize,
    line: SplitLine,
    winding: &mut [i32; 2],
) -> Placement {
    match line {
        SplitLine::Vertical(x) => {
            // Вклад в намотку: сегмент касается верхнего края и точен сверху.
            let delta = if segment.y_min == 0 && segment.flags & SEGFLAG_EXACT_TOP != 0 {
                if segment.a < 0 { 1 } else { -1 }
            } else {
                0
            };
            if segment.lies_left_of(x) {
                winding[group] += delta;
                if segment.x_min >= x {
                    return Placement::Dropped;
                }
                segment.x_max = segment.x_max.min(x);
                Placement::Before(segment)
            } else if segment.lies_right_of(x) {
                segment.move_x(x);
                Placement::After(segment)
            } else {
                if segment.flags & SEGFLAG_UL_DR != 0 {
                    winding[group] += delta;
                }
                let next = segment.split_at_x(x);
                Placement::Both(segment, next)
            }
        }
        SplitLine::Horizontal(y) => {
            let delta = if segment.x_min == 0 && segment.flags & SEGFLAG_EXACT_LEFT != 0 {
                if segment.b < 0 { 1 } else { -1 }
            } else {
                0
            };
            if segment.lies_above(y) {
                winding[group] += delta;
                if segment.y_min >= y {
                    return Placement::Dropped;
                }
                segment.y_max = segment.y_max.min(y);
                Placement::Before(segment)
            } else if segment.lies_below(y) {
                segment.move_y(y);
                Placement::After(segment)
            } else {
                if segment.flags & SEGFLAG_UL_DR != 0 {
                    winding[group] += delta;
                }
                let next = segment.split_at_y(y);
                Placement::Both(segment, next)
            }
        }
    }
}

/// Делит список сегментов линией (libass: `polyline_split_horz` / `polyline_split_vert`).
///
/// - `source` — сегменты: сначала `group_sizes[0]` штук группы 0, затем `group_sizes[1]` группы 1;
/// - части, лежащие до линии и после неё, дописываются в `before` и `after` (сегменты, которые
///   линия пересекает, попадают в обе части разрезанными);
/// - `winding[group]` получает вклад сегментов, целиком ушедших «до» линии (счётчик
///   намотки для верхнего левого угла второй части);
/// - сегменты второй части сдвигаются так, что линия становится началом координат.
pub(super) fn split_segments(
    source: &[Segment],
    group_sizes: [usize; 2],
    line: SplitLine,
    before: &mut Vec<Segment>,
    after: &mut Vec<Segment>,
    winding: &mut [i32; 2],
) -> SplitCounts {
    let mut counts = SplitCounts::default();
    before.reserve(source.len());
    after.reserve(source.len());
    for (index, original) in source.iter().enumerate() {
        let group = usize::from(index >= group_sizes[0]);
        match split_one(*original, group, line, winding) {
            Placement::Dropped => {}
            Placement::Before(kept) => {
                before.push(kept);
                counts.before[group] += 1;
            }
            Placement::After(kept) => {
                after.push(kept);
                counts.after[group] += 1;
            }
            Placement::Both(first, second) => {
                before.push(first);
                after.push(second);
                counts.before[group] += 1;
                counts.after[group] += 1;
            }
        }
    }
    counts
}

/// То же, что [`split_segments`], но входом служит хвост `stack[offset..]`, а часть «до»
/// записывается на его же место (libass: `src` совпадает с `dst0`), поэтому копия входа не нужна.
///
/// Запись «до» никогда не обгоняет чтение: на каждый прочитанный сегмент приходится не более
/// одной записи в `stack`. После вызова стек обрезан до конца части «до».
pub(super) fn split_stack_tail(
    stack: &mut Vec<Segment>,
    offset: usize,
    group_sizes: [usize; 2],
    line: SplitLine,
    after: &mut Vec<Segment>,
    winding: &mut [i32; 2],
) -> SplitCounts {
    let mut counts = SplitCounts::default();
    let end = stack.len();
    after.reserve(end.saturating_sub(offset));
    let mut write = offset;
    for read in offset..end {
        let group = usize::from(read - offset >= group_sizes[0]);
        let original = stack[read];
        match split_one(original, group, line, winding) {
            Placement::Dropped => {}
            Placement::Before(kept) => {
                stack[write] = kept;
                write += 1;
                counts.before[group] += 1;
            }
            Placement::After(kept) => {
                after.push(kept);
                counts.after[group] += 1;
            }
            Placement::Both(first, second) => {
                stack[write] = first;
                write += 1;
                after.push(second);
                counts.before[group] += 1;
                counts.after[group] += 1;
            }
        }
    }
    stack.truncate(write);
    counts
}
