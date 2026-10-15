//! Сдвиг слоя субтитров от стандартного места пресета, в долях картинки.

use subtitle_raster::SubtitleAppearance;

use super::UserSubtitleStyle;

/// Центр картинки по горизонтали в долях ширины: при нём горизонтальный сдвиг нулевой.
const HORIZONTAL_CENTER_FRACTION: f32 = 0.5;

/// Сдвиг низа текста от места пресета в долях высоты картинки (вверх = плюс) для позиции в
/// процентах. Принимает дробные проценты: режим правки на видео двигает слой плавнее 1 %.
pub(in crate::state::subtitles) fn vertical_offset_fraction_for_percent(
    vertical_percent: f32,
) -> f32 {
    vertical_percent / 100.0 - SubtitleAppearance::PRESET_BOTTOM_MARGIN
}

/// Сдвиг центра слоя от середины картинки в долях ширины (вправо = плюс) для позиции в процентах.
pub(in crate::state::subtitles) fn horizontal_offset_fraction_for_percent(
    horizontal_percent: f32,
) -> f32 {
    horizontal_percent / 100.0 - HORIZONTAL_CENTER_FRACTION
}

/// Обратный пересчёт: позиция в процентах (по горизонтали, по вертикали) для сдвига слоя в
/// долях картинки. Нужен режиму правки, чтобы начать перенос от того места, где слой ВИДЕН
/// (после зажимов и подъёма над панелью), а не от сохранённого значения.
pub(in crate::state::subtitles) fn position_percent_for_offset_fractions(
    horizontal_fraction: f32,
    vertical_fraction: f32,
) -> (f32, f32) {
    (
        (horizontal_fraction + HORIZONTAL_CENTER_FRACTION) * 100.0,
        (vertical_fraction + SubtitleAppearance::PRESET_BOTTOM_MARGIN) * 100.0,
    )
}

impl UserSubtitleStyle {
    /// Сдвиг низа текста от места пресета в долях высоты картинки (вверх = плюс).
    ///
    /// `value/100 - нижний отступ пресета`: при значении по умолчанию ровно `0.0`,
    /// поэтому показ байт-в-байт тот же, что до появления позиции.
    pub(crate) fn vertical_offset_fraction(&self) -> f32 {
        vertical_offset_fraction_for_percent(f32::from(self.position.vertical_percent))
    }

    /// Сдвиг центра слоя от середины картинки в долях ширины (вправо = плюс).
    pub(crate) fn horizontal_offset_fraction(&self) -> f32 {
        horizontal_offset_fraction_for_percent(f32::from(self.position.horizontal_percent))
    }
}
