//! Где и как лежит картинка: область видео, прямоугольники рисования, letterbox, ориентация.
//!
//! Координаты вызывающего — физические пиксели цели с началом в левом верхнем углу (как у
//! `RenderViewport` плеера и у wgpu). GL считает окно снизу вверх, поэтому перед
//! `glViewport`/`glScissor` ось Y переворачивается здесь, в одном месте.

use thiserror::Error;

/// Прямоугольник цели в физических пикселях, начало — левый верхний угол.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct PixelRect {
    /// Левая граница.
    pub x: u32,
    /// Верхняя граница.
    pub y: u32,
    /// Ширина.
    pub width: u32,
    /// Высота.
    pub height: u32,
}

impl PixelRect {
    /// Пустой ли прямоугольник.
    #[must_use]
    pub const fn is_empty(self) -> bool {
        self.width == 0 || self.height == 0
    }

    /// Правая граница (не включительно), без переполнения.
    const fn right(self) -> u64 {
        self.x as u64 + self.width as u64
    }

    /// Нижняя граница (не включительно), без переполнения.
    const fn bottom(self) -> u64 {
        self.y as u64 + self.height as u64
    }

    /// Общая часть двух прямоугольников; `None`, если они не пересекаются.
    #[must_use]
    pub fn intersection(self, other: Self) -> Option<Self> {
        let left = self.x.max(other.x);
        let top = self.y.max(other.y);
        let right = self.right().min(other.right());
        let bottom = self.bottom().min(other.bottom());
        let width = u32::try_from(right.checked_sub(u64::from(left))?).ok()?;
        let height = u32::try_from(bottom.checked_sub(u64::from(top))?).ok()?;
        let rect = Self {
            x: left,
            y: top,
            width,
            height,
        };
        (!rect.is_empty()).then_some(rect)
    }

    /// Части `self` вне `excluded` (до четырёх: сверху, снизу, слева, справа).
    #[must_use]
    pub fn subtract(self, excluded: Self) -> Vec<Self> {
        let Some(excluded) = self.intersection(excluded) else {
            return vec![self];
        };
        // Внутри пересечения все границы помещаются в u32: они не дальше границ `self`.
        let narrow = |value: u64| u32::try_from(value).unwrap_or(u32::MAX);
        let (self_right, self_bottom) = (self.right(), self.bottom());
        let (excluded_right, excluded_bottom) = (excluded.right(), excluded.bottom());
        let mut parts = Vec::with_capacity(4);
        if excluded.y > self.y {
            parts.push(Self {
                height: excluded.y - self.y,
                ..self
            });
        }
        if excluded_bottom < self_bottom {
            parts.push(Self {
                y: narrow(excluded_bottom),
                height: narrow(self_bottom - excluded_bottom),
                ..self
            });
        }
        if excluded.x > self.x {
            parts.push(Self {
                x: self.x,
                y: excluded.y,
                width: excluded.x - self.x,
                height: excluded.height,
            });
        }
        if excluded_right < self_right {
            parts.push(Self {
                x: narrow(excluded_right),
                y: excluded.y,
                width: narrow(self_right - excluded_right),
                height: excluded.height,
            });
        }
        parts
    }
}

/// Части `target`, которые не накрывает ни один из `covered`.
pub(crate) fn uncovered_parts(target: PixelRect, covered: &[PixelRect]) -> Vec<PixelRect> {
    let mut parts = vec![target];
    for cover in covered {
        parts = parts
            .into_iter()
            .flat_map(|part| part.subtract(*cover))
            .collect();
        if parts.is_empty() {
            break;
        }
    }
    parts
}

/// Как экранные координаты внутри области видео переходят в координаты картинки (letterbox).
///
/// `display_uv = viewport_uv × uv_scale + uv_offset`; где `display_uv` вне `[0, 1]`, шейдер
/// рисует чёрный (полосы letterbox). Значения считает вызывающий — в плеере это
/// `render_core::PictureLayout`, тот же источник, что у wgpu.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct PictureMapping {
    /// Масштаб.
    pub uv_scale: [f32; 2],
    /// Сдвиг.
    pub uv_offset: [f32; 2],
}

impl PictureMapping {
    /// Картинка занимает всю область видео.
    pub const FILL: Self = Self {
        uv_scale: [1.0, 1.0],
        uv_offset: [0.0, 0.0],
    };
}

/// Пиксели области видео `viewport`, которые принадлежат картинке, а не полосам letterbox.
///
/// Пиксель — картинка, если `display_uv` в его центре лежит в `[0, 1]` по обеим осям (границы
/// включительно): ровно та проверка, которую раньше делал фрагментный шейдер. Видео рисуется
/// только под scissor-ом этого прямоугольника, полосы заливает `glClear` вместе с остальными
/// незакрытыми частями цели. Так шейдер не ветвится и не тратит выборки на полосы (сессия 12a:
/// −2,4 мс GPU на кадр 1440×900 на T410).
///
/// Счёт в `f64` по точной формуле; шейдер считал в `f32` с интерполяцией, поэтому совпадение
/// гарантировано везде, кроме центров пикселей ровно на границе картинки, — там и раньше решало
/// округление (у wgpu тоже). `None` — картинки нет: пустая область, нулевой масштаб с картинкой
/// вне `[0, 1]` или нечисловое отображение.
#[must_use]
pub(crate) fn picture_pixel_rect(
    viewport: PixelRect,
    picture: PictureMapping,
) -> Option<PixelRect> {
    let columns = picture_pixel_span(
        viewport.x,
        viewport.width,
        picture.uv_scale[0],
        picture.uv_offset[0],
    )?;
    let rows = picture_pixel_span(
        viewport.y,
        viewport.height,
        picture.uv_scale[1],
        picture.uv_offset[1],
    )?;
    Some(PixelRect {
        x: columns.0,
        y: rows.0,
        width: columns.1,
        height: rows.1,
    })
}

/// Одна ось [`picture_pixel_rect`]: `(первый пиксель, число пикселей)` картинки внутри отрезка
/// области `[start, start + length)`.
fn picture_pixel_span(start: u32, length: u32, scale: f32, offset: f32) -> Option<(u32, u32)> {
    if length == 0 || !scale.is_finite() || !offset.is_finite() {
        return None;
    }
    let (scale, offset) = (f64::from(scale), f64::from(offset));
    // display(uv) = uv × scale + offset, uv — доля области под центром пикселя.
    let (first_uv, last_uv) = if scale == 0.0 {
        // Вся область показывает одну точку картинки: она либо внутри, либо нет.
        if (0.0..=1.0).contains(&offset) {
            (0.0, 1.0)
        } else {
            return None;
        }
    } else {
        let (at_zero, at_one) = ((0.0 - offset) / scale, (1.0 - offset) / scale);
        (at_zero.min(at_one), at_zero.max(at_one))
    };
    let length_f = f64::from(length);
    // Центр пикселя i (от начала области) — i + 0.5; он внутри, если uv = (i + 0.5) / length
    // лежит в [first_uv, last_uv].
    let first = (first_uv * length_f - 0.5).ceil().max(0.0);
    let last = (last_uv * length_f - 0.5).floor().min(length_f - 1.0);
    if last < first {
        return None;
    }
    // Оба конца — целые в [0, length): преобразование точное.
    let first_pixel = first as u32;
    let pixel_count = (last - first) as u32 + 1;
    Some((start.checked_add(first_pixel)?, pixel_count))
}

/// Поворот картинки на экране относительно кадра декодера (метаданные контейнера).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum QuarterTurn {
    /// Без поворота.
    Identity,
    /// На 90° по часовой.
    Clockwise90,
    /// На 180°.
    Half,
    /// На 270° по часовой.
    Clockwise270,
}

impl QuarterTurn {
    /// Аффинное преобразование `display_uv → source_uv`: две строки `[a, b, c]`,
    /// `source.x = a·u + b·v + c`. Числа — `display_orientation_uv_transform` wgpu-рендера.
    #[must_use]
    pub(crate) const fn display_to_source_rows(self) -> [[f32; 3]; 2] {
        match self {
            Self::Identity => [[1.0, 0.0, 0.0], [0.0, 1.0, 0.0]],
            Self::Clockwise90 => [[0.0, 1.0, 0.0], [-1.0, 0.0, 1.0]],
            Self::Half => [[-1.0, 0.0, 1.0], [0.0, -1.0, 1.0]],
            Self::Clockwise270 => [[0.0, -1.0, 1.0], [1.0, 0.0, 0.0]],
        }
    }
}

/// Прямоугольник в координатах окна GL (начало внизу слева), готовый для `glViewport`/`glScissor`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) struct GlWindowRect {
    pub(crate) x: i32,
    pub(crate) y: i32,
    pub(crate) width: i32,
    pub(crate) height: i32,
}

/// Прямоугольник не помещается в целые GL.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Error)]
#[error("rectangle {rect:?} does not fit GL window coordinates of a {target_height}-pixel target")]
pub struct RectOutOfRange {
    /// Прямоугольник.
    pub rect: PixelRect,
    /// Высота цели.
    pub target_height: u32,
}

/// Переворот оси Y: верх прямоугольника `y` в координатах «сверху» становится низом
/// `target_height − (y + height)` в координатах GL. Прямоугольник может выходить за цель (GL это
/// допускает), поэтому низ считается со знаком.
pub(crate) fn to_gl_window_rect(
    rect: PixelRect,
    target_height: u32,
) -> Result<GlWindowRect, RectOutOfRange> {
    let error = RectOutOfRange {
        rect,
        target_height,
    };
    let to_i64 = |value: u32| i64::from(value);
    let bottom = to_i64(target_height) - (to_i64(rect.y) + to_i64(rect.height));
    let fit = |value: i64| i32::try_from(value).map_err(|_| error);
    Ok(GlWindowRect {
        x: fit(to_i64(rect.x))?,
        y: fit(bottom)?,
        width: fit(to_i64(rect.width))?,
        height: fit(to_i64(rect.height))?,
    })
}

#[cfg(test)]
mod tests;
