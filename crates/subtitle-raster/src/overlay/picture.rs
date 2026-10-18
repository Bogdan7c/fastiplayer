//! Полноцветные картинки в оверлее: формат пикселей и конструктор авторской картинки (PGS).
//!
//! Контракт оверлея изначально «маска покрытия + один цвет» (см. документацию модуля
//! `overlay`). Для текста это остаётся правилом. Растровые субтитры Blu-ray рисует автор
//! палитрой, и один цвет на маску для них не годится, поэтому здесь добавлен второй формат
//! пикселей. Контракт для рендера (GPU сейчас, GLES2 потом):
//!
//! - пиксель картинки - `[r, g, b, a]` с ПРЕМУЛЬТИПЛИЦИРОВАННОЙ альфой (канал уже умножен
//!   на `a`), строки идут подряд с шагом [`SubtitleBitmap::stride`] байт;
//! - цвет картинки ([`SubtitleBitmap::color`]) для RGBA работает как множитель
//!   непрозрачности: используется ТОЛЬКО `color.alpha`, `red/green/blue` игнорируются.
//!   Цвет по умолчанию - непрозрачный белый, то есть множитель 1;
//! - итоговый вклад пикселя: `source = texel_rgba * (color.alpha / 255)` (все четыре канала),
//!   результат кладётся на кадр оператором premultiplied over:
//!   `out = source + dest * (1 - source.a)`. В GLES2 это один `texture2D` и один множитель,
//!   режим смешивания `ONE, ONE_MINUS_SRC_ALPHA`.

use std::sync::Arc;

use subtitle_core::{PictureForcedFlag, SubtitleColor};

use super::mask_view::required_coverage_len;
use super::{OverlayPlacementGroup, div255};
use super::{PixelPoint, SubtitleBitmap, SubtitleBitmapError, SubtitleBitmapKind, blend_pixel};

/// Как устроены байты буфера пикселей картинки.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum BitmapPixelFormat {
    /// Один байт на пиксель: покрытие 0..=255. Цвет задаёт [`SubtitleBitmap::color`].
    Coverage,
    /// Четыре байта на пиксель `[r, g, b, a]`, цвет премультиплицирован на альфу.
    PremultipliedRgba,
}

impl BitmapPixelFormat {
    /// Сколько байт занимает один пиксель.
    pub const fn bytes_per_pixel(self) -> usize {
        match self {
            Self::Coverage => 1,
            Self::PremultipliedRgba => 4,
        }
    }
}

/// Непрозрачный белый: множитель непрозрачности 1 для RGBA-картинки.
pub(crate) const OPAQUE_WHITE: SubtitleColor = SubtitleColor::opaque(255, 255, 255);

impl SubtitleBitmap {
    /// Строит авторскую RGBA-картинку из тесно упакованных пикселей
    /// (`stride = width * 4`, премультиплицированная альфа).
    ///
    /// Цвет - непрозрачный белый ([`OPAQUE_WHITE`]); группу размещения и владельца задаёт
    /// растеризатор. Раскладка проверяется так же, как у маски (непустая площадь, длина буфера).
    pub(crate) fn picture(
        origin: PixelPoint,
        width: u32,
        height: u32,
        rgba: Arc<[u8]>,
        forced: PictureForcedFlag,
    ) -> Result<Self, SubtitleBitmapError> {
        if width == 0 || height == 0 {
            return Err(SubtitleBitmapError::EmptyArea { width, height });
        }
        let stride =
            (width as usize).saturating_mul(BitmapPixelFormat::PremultipliedRgba.bytes_per_pixel());
        let required = required_coverage_len(stride, height, stride).unwrap_or(usize::MAX);
        if rgba.len() < required {
            return Err(SubtitleBitmapError::CoverageTooShort {
                actual: rgba.len(),
                required,
            });
        }
        Ok(Self {
            origin,
            width,
            height,
            stride,
            coverage: rgba,
            coverage_offset: 0,
            color: OPAQUE_WHITE,
            kind: SubtitleBitmapKind::Picture,
            format: BitmapPixelFormat::PremultipliedRgba,
            forced,
            placement_group: OverlayPlacementGroup::Main,
            owner: None,
        })
    }

    /// Формат байтов картинки. Потребитель обязан проверить его до чтения пикселей.
    pub fn pixel_format(&self) -> BitmapPixelFormat {
        self.format
    }

    /// Окно RGBA-пикселей (`[r, g, b, a]`, премультиплицированная альфа, шаг
    /// [`Self::stride`] байт) или `None`, если картинка - маска покрытия.
    pub fn rgba_pixels(&self) -> Option<&[u8]> {
        match self.format {
            BitmapPixelFormat::PremultipliedRgba => Some(self.coverage_window()),
            BitmapPixelFormat::Coverage => None,
        }
    }

    /// Пометка forced объекта-картинки (показывать даже при выключенных субтитрах). Фильтрацию
    /// по ней делает вызывающий код, растеризатор рисует любые картинки запроса. У масок
    /// текста всегда [`PictureForcedFlag::Regular`].
    pub fn forced(&self) -> PictureForcedFlag {
        self.forced
    }

    /// Строка RGBA-картинки `row` (ровно `width * 4` байт) или `None` за пределами картинки /
    /// у маски покрытия.
    pub fn rgba_row(&self, row: u32) -> Option<&[u8]> {
        if row >= self.height || self.format != BitmapPixelFormat::PremultipliedRgba {
            return None;
        }
        let start = (row as usize).checked_mul(self.stride)?;
        let end = start.checked_add((self.width as usize).checked_mul(4)?)?;
        self.coverage_window().get(start..end)
    }
}

/// Видимая часть отрезка картинки вдоль одной оси холста.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) struct ClippedAxis {
    /// Сколько первых пикселей картинки лежит за началом холста (отрицательный origin).
    pub(super) skipped: usize,
    /// Сколько пикселей картинки попадает на холст.
    pub(super) visible: usize,
    /// Координата на холсте, с которой начинается видимая часть.
    pub(super) destination: usize,
}

/// Обрезает отрезок `[origin, origin + length)` по холсту `[0, canvas_length)`.
/// `None` — отрезок целиком вне холста. Арифметика в `i64`: переполнений нет.
pub(super) fn clip_axis(origin: i32, length: u32, canvas_length: usize) -> Option<ClippedAxis> {
    let canvas_length = i64::try_from(canvas_length).ok()?;
    let start = i64::from(origin);
    let end = start + i64::from(length);
    let visible_start = start.max(0);
    let visible_end = end.min(canvas_length);
    if visible_start >= visible_end {
        return None;
    }
    Some(ClippedAxis {
        skipped: usize::try_from(visible_start - start).ok()?,
        visible: usize::try_from(visible_end - visible_start).ok()?,
        destination: usize::try_from(visible_start).ok()?,
    })
}

/// Накладывает RGBA-картинку на холст `pixels` (прямая альфа, ширина `canvas_width`).
///
/// Источник сначала умножается на непрозрачность `color.alpha`, затем переводится в прямой
/// цвет и идёт через тот же оператор over, что и маски текста (`blend_pixel`).
pub(super) fn composite_picture(pixels: &mut [u8], canvas_width: usize, bitmap: &SubtitleBitmap) {
    let canvas_height = pixels.len() / 4 / canvas_width.max(1);
    let (Some(columns), Some(rows)) = (
        clip_axis(bitmap.origin().x, bitmap.width(), canvas_width),
        clip_axis(bitmap.origin().y, bitmap.height(), canvas_height),
    ) else {
        return; // Картинка целиком вне холста.
    };
    let opacity = u32::from(bitmap.color().alpha);
    for row_offset in 0..rows.visible {
        let Some(rgba_row) = bitmap.rgba_row((rows.skipped + row_offset) as u32) else {
            return;
        };
        let row_start = (rows.destination + row_offset) * canvas_width + columns.destination;
        // Берём только видимые колонки: иначе правая часть «заворачивалась» бы на
        // следующую строку холста, а левая за краем пропадала бы вместе со всей картинкой.
        let visible_texels = rgba_row
            .as_chunks::<4>()
            .0
            .iter()
            .skip(columns.skipped)
            .take(columns.visible);
        for (column, texel) in visible_texels.enumerate() {
            let [red, green, blue, alpha] = [texel[0], texel[1], texel[2], texel[3]].map(u32::from);
            let effective_alpha = div255(alpha * opacity);
            if effective_alpha == 0 {
                continue;
            }
            // Премультиплицированный канал -> прямой: c = premult * 255 / alpha.
            let straight = |premultiplied: u32| -> u8 {
                u8::try_from(((premultiplied * 255 + alpha / 2) / alpha).min(255))
                    .unwrap_or(u8::MAX)
            };
            let color = SubtitleColor {
                red: straight(red),
                green: straight(green),
                blue: straight(blue),
                alpha: u8::try_from(effective_alpha).unwrap_or(u8::MAX),
            };
            let offset = (row_start + column) * 4;
            let Some(pixel) = pixels.get_mut(offset..offset + 4) else {
                return;
            };
            // Покрытие 255: вся непрозрачность уже учтена в `color.alpha`.
            blend_pixel(pixel, color, 255);
        }
    }
}
