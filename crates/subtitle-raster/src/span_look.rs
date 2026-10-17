//! Внешний вид одного участка текста реплики («стиль символа»), все размеры в пикселях кадра.
//!
//! В libass каждый символ (`GlyphInfo`) несёт собственные копии шрифта, размера, четырёх
//! цветов, обводки, тени и флагов — именно по ним `split_style_runs` режет текст на участки и
//! `render_and_combine_glyphs` выбирает фильтры. [`SpanLook`] — тот же набор значений, только
//! дедуплицированный: одинаковые виды получают один индекс в `SourceText::looks`, поэтому
//! проверка «стиль изменился» сводится к сравнению индексов.
//!
//! Откуда берётся вид:
//! - SRT/WebVTT — из `EventStyle` реплики и стиля спана (`SourceText::from_styled`);
//! - ASS — из состояния рендера на момент символа (модуль `ass`).
//!
//! Конвейер (шейпинг, контуры, раскладка, композиция) читает значения ТОЛЬКО отсюда.

use crate::event_style::{AxisPair, BorderStyle, StyleColors};
use crate::fonts::{FontStyle, FontWeightClass};

/// Подчёркивание и зачёркивание участка (libass: `DECO_UNDERLINE`, `DECO_STRIKETHROUGH`).
///
/// Линии добавляются к контуру глифа (`glyph_render::styled`), поэтому вид участка с линией
/// отличается от вида без неё: участки режутся по этому признаку, как `flags` в libass.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Hash)]
pub(crate) struct TextDecorations {
    /// Подчёркнутый текст.
    pub(crate) underline: bool,
    /// Зачёркнутый текст.
    pub(crate) strike_out: bool,
}

/// Внешний вид символов участка.
#[derive(Debug, Clone, PartialEq)]
pub(crate) struct SpanLook {
    /// Имя семейства шрифта; `None` — встроенный Noto Sans.
    pub(crate) font_family: Option<String>,
    /// Размер шрифта («высота ячейки») в пикселях кадра; положителен и конечен (проверяется
    /// перед рисованием).
    pub(crate) font_size: f64,
    /// Насыщенность для выбора лица (порог 550, как `FontWeightClass`).
    pub(crate) weight: FontWeightClass,
    /// Числовой вес в стиле CSS (400, 700, точное значение `\b<n>`); по нему решается
    /// синтетическое утолщение, для выбора лица достаточно [`Self::weight`].
    pub(crate) weight_value: u16,
    /// Наклон.
    pub(crate) slant: FontStyle,
    /// Четыре цвета участка (прямая альфа: 255 — непрозрачный).
    pub(crate) colors: StyleColors,
    /// Толщина обводки по осям, пиксели.
    pub(crate) border: AxisPair,
    /// Смещение тени по осям, пиксели (отрицательное — влево/вверх).
    pub(crate) shadow: AxisPair,
    /// Вид подложки и обводки участка (в libass он тоже хранится у каждого символа).
    pub(crate) border_style: BorderStyle,
    /// Масштаб букв по осям (`\fscx`, `\fscy`; 1.0 — без масштаба): множит размер глифа и
    /// продвижения (libass: `scale_x`, `scale_y`).
    pub(crate) scale: AxisPair,
    /// Межбуквенный интервал (`\fsp`) в пикселях кадра до масштаба букв: значение сценария,
    /// умноженное на `screen_scale_x` и поделённое на `par_scale_x`. Конвейер домножает его
    /// на `scale.horizontal` (libass: `hspacing_scaled`). Ноль — интервала нет.
    pub(crate) letter_spacing: f64,
    /// Подчёркивание и зачёркивание.
    pub(crate) decorations: TextDecorations,
}

impl SpanLook {
    /// Все числа вида конечны (иначе арифметика конвейера теряет смысл).
    pub(crate) fn numbers_are_finite(&self) -> bool {
        [
            self.border.horizontal,
            self.border.vertical,
            self.shadow.horizontal,
            self.shadow.vertical,
            self.scale.horizontal,
            self.scale.vertical,
            self.letter_spacing,
        ]
        .iter()
        .all(|value| value.is_finite())
    }
}

#[cfg(test)]
mod tests {
    use subtitle_core::SubtitleColor;

    use super::*;

    fn look() -> SpanLook {
        let white = SubtitleColor::opaque(255, 255, 255);
        SpanLook {
            font_family: None,
            font_size: 40.0,
            weight: FontWeightClass::Regular,
            weight_value: 400,
            slant: FontStyle::Normal,
            colors: StyleColors {
                primary: white,
                secondary: white,
                outline: white,
                back: white,
            },
            border: AxisPair::uniform(0.0),
            shadow: AxisPair::uniform(0.0),
            border_style: BorderStyle::OutlineAndShadow,
            scale: AxisPair::uniform(1.0),
            letter_spacing: 0.0,
            decorations: TextDecorations::default(),
        }
    }

    #[test]
    fn non_finite_numbers_are_detected() {
        assert!(look().numbers_are_finite());
        let mut broken = look();
        broken.shadow.vertical = f64::NAN;
        assert!(!broken.numbers_are_finite());
        let mut infinite = look();
        infinite.letter_spacing = f64::INFINITY;
        assert!(!infinite.numbers_are_finite());
    }
}
