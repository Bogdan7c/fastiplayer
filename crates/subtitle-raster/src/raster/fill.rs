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

//! Заливка окна: подготовка ломаной и обход четвертного дерева тайлов
//! (libass 0.17.5: `ass_rasterizer_fill`, `rasterizer_fill_level`, `rasterizer_fill_solid`,
//! `rasterizer_fill_halfplane`, `get_fill_flags`).

use super::segment::{
    SEGFLAG_DN, SEGFLAG_EXACT_LEFT, SEGFLAG_UL_DR, Segment, SplitLine, floor_log2, split_segments,
};
use super::tile::{
    TILE_ORDER, TILE_SIZE, fill_generic_tile, fill_halfplane_tile, fill_solid_tile, merge_tile,
};
use super::{FillWindow, RasterError, Rasterizer};

/// Тайл целиком закрашен или пуст (libass: `FLAG_SOLID`).
const FLAG_SOLID: i32 = 1;
/// В тайле есть граница (libass: `FLAG_COMPLEX`).
const FLAG_COMPLEX: i32 = 2;
/// Полуплоскость инвертирована (libass: `FLAG_REVERSE`).
const FLAG_REVERSE: i32 = 4;
/// Нужна общая заливка по нескольким отрезкам (libass: `FLAG_GENERIC`).
const FLAG_GENERIC: i32 = 8;

/// Положение части окна в целевом буфере и её размер.
#[derive(Debug, Clone, Copy)]
struct Region {
    /// Индекс первого байта левого верхнего пикселя в буфере.
    origin: usize,
    /// Ширина в пикселях (кратна тайлу).
    width: usize,
    /// Высота в пикселях (кратна тайлу).
    height: usize,
}

/// Один из двух стеков отрезков (libass: индекс 0 или 1 в `linebuf`).
///
/// Перечисление вместо числа: к стеку нельзя обратиться по неверному индексу.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum StackId {
    First,
    Second,
}

impl StackId {
    /// Второй стек пары (libass: `index ^ 1`).
    fn other(self) -> Self {
        match self {
            Self::First => Self::Second,
            Self::Second => Self::First,
        }
    }
}

/// Два стека отрезков уровней дерева. Узел хранит свои отрезки на вершине одного из стеков;
/// при делении половины уходят: одна остаётся на месте, другая кладётся на вершину второго стека.
#[derive(Debug, Default)]
pub(super) struct LineStacks {
    first: Vec<Segment>,
    second: Vec<Segment>,
}

impl LineStacks {
    fn stack(&self, id: StackId) -> &Vec<Segment> {
        match id {
            StackId::First => &self.first,
            StackId::Second => &self.second,
        }
    }

    fn stack_mut(&mut self, id: StackId) -> &mut Vec<Segment> {
        match id {
            StackId::First => &mut self.first,
            StackId::Second => &mut self.second,
        }
    }

    /// Оба стека одновременно: `(стек id, второй стек)`.
    fn pair_mut(&mut self, id: StackId) -> (&mut Vec<Segment>, &mut Vec<Segment>) {
        match id {
            StackId::First => (&mut self.first, &mut self.second),
            StackId::Second => (&mut self.second, &mut self.first),
        }
    }

    fn clear(&mut self) {
        self.first.clear();
        self.second.clear();
    }
}

/// Какие отрезки и какая намотка относятся к узлу дерева.
#[derive(Debug, Clone, Copy)]
struct LevelInput {
    /// Какой из двух стеков хранит отрезки узла.
    stack: StackId,
    /// Число отрезков основной и дополнительной групп.
    group_sizes: [usize; 2],
    /// Намотка левого нижнего угла для каждой группы.
    winding: [i32; 2],
}

/// Выбирает способ заливки узла по его отрезкам (libass: `get_fill_flags`).
fn fill_flags(lines: &[Segment], winding: i32) -> i32 {
    match lines {
        [] => {
            if winding != 0 {
                FLAG_SOLID
            } else {
                0
            }
        }
        [line] => {
            const TEST: u8 = SEGFLAG_UL_DR | SEGFLAG_EXACT_LEFT;
            let mut winding = winding;
            if ((line.flags & TEST) != TEST) == (line.flags & SEGFLAG_DN == 0) {
                winding += 1;
            }
            match winding {
                0 => FLAG_COMPLEX | FLAG_REVERSE,
                1 => FLAG_COMPLEX,
                _ => FLAG_SOLID,
            }
        }
        _ => FLAG_COMPLEX | FLAG_GENERIC,
    }
}

impl Rasterizer {
    /// Растеризует загруженную ломаную в `buffer` (libass: `ass_rasterizer_fill`).
    ///
    /// `window` задаёт область контура (пиксели) и целевой буфер; ширина и высота кратны
    /// размеру тайла, `buffer.len() >= stride * height`. Ломаная после заливки удаляется
    /// (как в libass): для повторной заливки нужен новый [`Rasterizer::set_outline`].
    pub(crate) fn fill(
        &mut self,
        buffer: &mut [u8],
        window: FillWindow,
    ) -> Result<(), RasterError> {
        let aligned = |value: usize| value > 0 && value.is_multiple_of(TILE_SIZE);
        if !aligned(window.width) || !aligned(window.height) || window.stride < window.width {
            return Err(RasterError::InvalidWindow);
        }
        let required = window
            .stride
            .checked_mul(window.height)
            .ok_or(RasterError::BufferTooSmall)?;
        if buffer.len() < required {
            return Err(RasterError::BufferTooSmall);
        }
        // Размеры окна в 1/64 пикселя должны помещаться в i32.
        let (Ok(width_px), Ok(height_px)) =
            (i32::try_from(window.width), i32::try_from(window.height))
        else {
            return Err(RasterError::InvalidWindow);
        };
        let size_x = width_px.checked_mul(64).ok_or(RasterError::InvalidWindow)?;
        let size_y = height_px
            .checked_mul(64)
            .ok_or(RasterError::InvalidWindow)?;

        // Переносим начало координат в левый верхний угол окна.
        let shift_x = window.x0.wrapping_mul(64);
        let shift_y = window.y0.wrapping_mul(64);
        let mut lines = std::mem::take(&mut self.polyline);
        for line in &mut lines {
            line.x_min = line.x_min.wrapping_sub(shift_x);
            line.x_max = line.x_max.wrapping_sub(shift_x);
            line.y_min = line.y_min.wrapping_sub(shift_y);
            line.y_max = line.y_max.wrapping_sub(shift_y);
            line.c = line.c.wrapping_sub(
                i64::from(line.a)
                    .wrapping_mul(i64::from(shift_x))
                    .wrapping_add(i64::from(line.b).wrapping_mul(i64::from(shift_y))),
            );
        }
        let mut bbox = self.bbox;
        bbox.x_min = bbox.x_min.wrapping_sub(shift_x);
        bbox.x_max = bbox.x_max.wrapping_sub(shift_x);
        bbox.y_min = bbox.y_min.wrapping_sub(shift_y);
        bbox.y_max = bbox.y_max.wrapping_sub(shift_y);

        let mut group_sizes = [self.n_first, lines.len().saturating_sub(self.n_first)];
        let mut winding = [0_i32; 2];

        // Обрезка окном: сначала справа и снизу (выбрасываем лишнее), потом слева и сверху
        // (учитываем намотку отброшенной части).
        if bbox.x_max >= size_x {
            let (kept, counts) = split_keeping_before(
                &lines,
                group_sizes,
                SplitLine::Vertical(size_x),
                &mut winding,
            );
            lines = kept;
            group_sizes = counts;
            winding = [0, 0];
        }
        if bbox.y_max >= size_y {
            let (kept, counts) = split_keeping_before(
                &lines,
                group_sizes,
                SplitLine::Horizontal(size_y),
                &mut winding,
            );
            lines = kept;
            group_sizes = counts;
            winding = [0, 0];
        }
        if bbox.x_min <= 0 {
            let (kept, counts) =
                split_keeping_after(&lines, group_sizes, SplitLine::Vertical(0), &mut winding);
            lines = kept;
            group_sizes = counts;
        }
        if bbox.y_min <= 0 {
            let (kept, counts) =
                split_keeping_after(&lines, group_sizes, SplitLine::Horizontal(0), &mut winding);
            lines = kept;
            group_sizes = counts;
        }

        self.n_first = 0;
        self.bbox = crate::outline::ControlBox::EMPTY;
        self.stacks.clear();
        *self.stacks.stack_mut(StackId::First) = lines;
        let result = self.fill_level(
            buffer,
            window.stride,
            Region {
                origin: 0,
                width: window.width,
                height: window.height,
            },
            LevelInput {
                stack: StackId::First,
                group_sizes,
                winding,
            },
        );
        // Ломаная расходуется в любом случае, в том числе при ошибке.
        self.stacks.clear();
        result
    }

    /// Рекурсивный обход четвертного дерева (libass: `rasterizer_fill_level`).
    ///
    /// Отрезки узла лежат на вершине стека `input.stack_index`; по возвращении они удалены.
    fn fill_level(
        &mut self,
        buffer: &mut [u8],
        stride: usize,
        region: Region,
        input: LevelInput,
    ) -> Result<(), RasterError> {
        let LevelInput {
            stack: index,
            group_sizes,
            winding,
        } = input;
        let total = group_sizes[0] + group_sizes[1];
        let offset = self
            .stacks
            .stack(index)
            .len()
            .checked_sub(total)
            .ok_or(RasterError::SegmentOutsideTile)?;

        // Решение о способе заливки.
        let (flags0, flags1, flags, first_line, second_line) = {
            let node_lines = self
                .stacks
                .stack(index)
                .get(offset..)
                .ok_or(RasterError::SegmentOutsideTile)?;
            let (lines0, lines1) = node_lines.split_at(group_sizes[0]);
            let flags0 = fill_flags(lines0, winding[0]);
            let flags1 = fill_flags(lines1, winding[1]);
            (
                flags0,
                flags1,
                (flags0 | flags1) ^ FLAG_COMPLEX,
                lines0.first().copied(),
                lines1.first().copied(),
            )
        };

        if flags & (FLAG_SOLID | FLAG_COMPLEX) != 0 {
            fill_solid_region(buffer, stride, region, flags & FLAG_SOLID != 0)?;
            self.stacks.stack_mut(index).truncate(offset);
            return Ok(());
        }
        if flags & FLAG_GENERIC == 0 && (flags0 ^ flags1) & FLAG_COMPLEX != 0 {
            let line = if flags1 & FLAG_COMPLEX != 0 {
                second_line
            } else {
                first_line
            };
            let line = line.ok_or(RasterError::SegmentOutsideTile)?;
            let scale = if flags & FLAG_REVERSE != 0 {
                line.scale.wrapping_neg()
            } else {
                line.scale
            };
            fill_halfplane_region(buffer, stride, region, (line.a, line.b, line.c), scale)?;
            self.stacks.stack_mut(index).truncate(offset);
            return Ok(());
        }
        if region.width == TILE_SIZE && region.height == TILE_SIZE {
            self.fill_single_tile(
                buffer,
                stride,
                region.origin,
                input,
                offset,
                [flags0, flags1],
            )?;
            self.stacks.stack_mut(index).truncate(offset);
            return Ok(());
        }

        // Делим окно пополам по длинной стороне и спускаемся в обе половины.
        let mut next_input = input;
        let mut region0 = region;
        let mut region1 = region;
        let split_line = if region.width > region.height {
            let first_width = 1_usize << floor_log2((region.width - 1) as u32);
            region0.width = first_width;
            region1.width = region.width - first_width;
            region1.origin += first_width;
            SplitLine::Vertical((first_width as i32) << 6)
        } else {
            let first_height = 1_usize << floor_log2((region.height - 1) as u32);
            region0.height = first_height;
            region1.height = region.height - first_height;
            region1.origin += first_height * stride;
            SplitLine::Horizontal((first_height as i32) << 6)
        };

        let mut winding1 = winding;
        let other = index.other();
        self.scratch.clear();
        if let Some(node_lines) = self.stacks.stack(index).get(offset..) {
            self.scratch.extend_from_slice(node_lines);
        }
        self.stacks.stack_mut(index).truncate(offset);
        let counts = {
            let (before, after) = self.stacks.pair_mut(index);
            split_segments(
                &self.scratch,
                group_sizes,
                split_line,
                before,
                after,
                &mut winding1,
            )
        };

        next_input.group_sizes = counts.before;
        self.fill_level(buffer, stride, region0, next_input)?;
        self.fill_level(
            buffer,
            stride,
            region1,
            LevelInput {
                stack: other,
                group_sizes: counts.after,
                winding: winding1,
            },
        )
    }

    /// Заливает один тайл 16x16 (ветка `width == height == tile` в `rasterizer_fill_level`).
    fn fill_single_tile(
        &mut self,
        buffer: &mut [u8],
        stride: usize,
        origin: usize,
        input: LevelInput,
        offset: usize,
        flags: [i32; 2],
    ) -> Result<(), RasterError> {
        let node_lines = self
            .stacks
            .stack(input.stack)
            .get(offset..)
            .ok_or(RasterError::SegmentOutsideTile)?;
        let (lines0, lines1) = node_lines.split_at(input.group_sizes[0]);
        let [flags0, flags1] = flags;
        let [winding0, winding1] = input.winding;

        if flags1 & FLAG_COMPLEX == 0 {
            return fill_generic_tile(buffer, origin, stride, lines0, winding0);
        }
        if flags0 & FLAG_COMPLEX == 0 {
            return fill_generic_tile(buffer, origin, stride, lines1, winding1);
        }
        // Обе группы с границей: первую рисуем в буфер, вторую во временный тайл и объединяем.
        if flags0 & FLAG_GENERIC != 0 {
            fill_generic_tile(buffer, origin, stride, lines0, winding0)?;
        } else {
            let line = lines0.first().ok_or(RasterError::SegmentOutsideTile)?;
            let scale = if flags0 & FLAG_REVERSE != 0 {
                line.scale.wrapping_neg()
            } else {
                line.scale
            };
            fill_halfplane_tile(buffer, origin, stride, line.a, line.b, line.c, scale)?;
        }
        if flags1 & FLAG_GENERIC != 0 {
            fill_generic_tile(&mut self.tile, 0, TILE_SIZE, lines1, winding1)?;
        } else {
            let line = lines1.first().ok_or(RasterError::SegmentOutsideTile)?;
            let scale = if flags1 & FLAG_REVERSE != 0 {
                line.scale.wrapping_neg()
            } else {
                line.scale
            };
            fill_halfplane_tile(&mut self.tile, 0, TILE_SIZE, line.a, line.b, line.c, scale)?;
        }
        merge_tile(buffer, origin, stride, &self.tile)
    }
}

/// Делит `lines` линией и оставляет часть «до» (остальное отбрасывается, намотка накапливается).
fn split_keeping_before(
    lines: &[Segment],
    group_sizes: [usize; 2],
    line: SplitLine,
    winding: &mut [i32; 2],
) -> (Vec<Segment>, [usize; 2]) {
    let mut before = Vec::with_capacity(lines.len());
    let mut discarded = Vec::new();
    let counts = split_segments(
        lines,
        group_sizes,
        line,
        &mut before,
        &mut discarded,
        winding,
    );
    (before, counts.before)
}

/// Делит `lines` линией и оставляет часть «после» (часть «до» отбрасывается).
fn split_keeping_after(
    lines: &[Segment],
    group_sizes: [usize; 2],
    line: SplitLine,
    winding: &mut [i32; 2],
) -> (Vec<Segment>, [usize; 2]) {
    let mut discarded = Vec::new();
    let mut after = Vec::with_capacity(lines.len());
    let counts = split_segments(
        lines,
        group_sizes,
        line,
        &mut discarded,
        &mut after,
        winding,
    );
    (after, counts.after)
}

/// Заливает область сплошным значением по тайлам (libass: `rasterizer_fill_solid`).
fn fill_solid_region(
    buffer: &mut [u8],
    stride: usize,
    region: Region,
    covered: bool,
) -> Result<(), RasterError> {
    for tile_y in 0..region.height / TILE_SIZE {
        for tile_x in 0..region.width / TILE_SIZE {
            let origin = region.origin + tile_y * TILE_SIZE * stride + tile_x * TILE_SIZE;
            fill_solid_tile(buffer, origin, stride, covered)?;
        }
    }
    Ok(())
}

/// Заливает область полуплоскостью, выбирая для каждого тайла сплошную или точную заливку
/// (libass: `rasterizer_fill_halfplane`).
fn fill_halfplane_region(
    buffer: &mut [u8],
    stride: usize,
    region: Region,
    plane: (i32, i32, i64),
    scale: i32,
) -> Result<(), RasterError> {
    let (a, b, c) = plane;
    if region.width == TILE_SIZE && region.height == TILE_SIZE {
        return fill_halfplane_tile(buffer, region.origin, stride, a, b, c, scale);
    }

    let abs_a = a.unsigned_abs();
    let abs_b = b.unsigned_abs();
    let size = i64::from(abs_a.wrapping_add(abs_b)) << (TILE_ORDER + 5);
    let offs = (i64::from(a) + i64::from(b)) * (1_i64 << (TILE_ORDER + 5));

    for tile_y in 0..region.height / TILE_SIZE {
        for tile_x in 0..region.width / TILE_SIZE {
            let origin = region.origin + tile_y * TILE_SIZE * stride + tile_x * TILE_SIZE;
            let shift = (i64::from(a) * tile_x as i64 + i64::from(b) * tile_y as i64)
                * (1_i64 << (TILE_ORDER + 6));
            let cc = c - shift;
            let offs_c = offs - cc;
            if offs_c.abs() < size {
                fill_halfplane_tile(buffer, origin, stride, a, b, cc, scale)?;
            } else {
                // Тайл целиком по одну сторону границы: знак определяет закраску.
                let covered = (((offs_c >> 32) as u32) ^ (scale as u32)) & 0x8000_0000 != 0;
                fill_solid_tile(buffer, origin, stride, covered)?;
            }
        }
    }
    Ok(())
}
