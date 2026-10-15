//! Плоский текст реплики: символы с индексом внешнего вида.
//!
//! [`StyledText`] хранит спаны; конвейер libass работает с массивом символов, у каждого из
//! которых «внешний вид» (цвет, насыщенность, наклон). Здесь спаны разворачиваются в такой
//! массив, а одинаковые внешние виды получают один индекс — тогда проверка «стиль изменился,
//! начать новый участок» (libass: `split_style_runs`) сводится к сравнению индексов.

use subtitle_core::{FontSlant, FontWeight, SpanContent, StyledText, SubtitleColor};

use crate::event_style::EventStyle;
use crate::fonts::{FontStyle, FontWeightClass};

/// Внешний вид символа, который может отличаться между спанами одной реплики.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) struct SpanLook {
    /// Цвет заливки автора; `None` — основной цвет стиля.
    pub(crate) fill_override: Option<SubtitleColor>,
    /// Итоговая насыщенность (стиль события или полужирный спан).
    pub(crate) weight: FontWeightClass,
    /// Итоговый наклон (стиль события или курсивный спан).
    pub(crate) slant: FontStyle,
}

/// Символ реплики и индекс его внешнего вида в [`SourceText::looks`].
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) struct SourceChar {
    /// Символ (`'\n'` — явный перевод строки).
    pub(crate) ch: char,
    /// Индекс в [`SourceText::looks`].
    pub(crate) look: usize,
}

/// Развёрнутый текст реплики.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct SourceText {
    /// Символы в порядке чтения (логический порядок).
    pub(crate) chars: Vec<SourceChar>,
    /// Различные внешние виды; индексируются из [`SourceChar::look`].
    pub(crate) looks: Vec<SpanLook>,
}

impl SourceText {
    /// Разворачивает спаны реплики.
    ///
    /// Как в libass, табуляция превращается в пробел (`ass_get_next_char`); возврат каретки
    /// отбрасывается — иначе он стал бы разделителем абзацев BiDi и пустым глифом.
    pub(crate) fn from_styled(text: &StyledText, style: &EventStyle) -> Self {
        let mut looks: Vec<SpanLook> = Vec::new();
        let mut chars = Vec::new();
        for span in text.spans() {
            let look = SpanLook {
                fill_override: span.style.color,
                weight: if span.style.weight == FontWeight::Bold {
                    FontWeightClass::Bold
                } else {
                    style.font_weight
                },
                slant: if span.style.slant == FontSlant::Italic {
                    FontStyle::Italic
                } else {
                    style.font_slant
                },
            };
            // Линейный поиск внешнего вида: различных видов не больше числа спанов, а число
            // символов реплики ограничено `RasterInputLimits::max_chars_per_cue`, поэтому
            // стоимость ограничена сверху (квадрат от лимита в худшем случае, на практике
            // это единицы видов).
            let look_index = looks
                .iter()
                .position(|known| *known == look)
                .unwrap_or_else(|| {
                    looks.push(look);
                    looks.len() - 1
                });
            match &span.content {
                SpanContent::Text(fragment) => {
                    for ch in fragment.chars() {
                        match ch {
                            '\r' => {}
                            '\t' => chars.push(SourceChar {
                                ch: ' ',
                                look: look_index,
                            }),
                            other => chars.push(SourceChar {
                                ch: other,
                                look: look_index,
                            }),
                        }
                    }
                }
                SpanContent::LineBreak => chars.push(SourceChar {
                    ch: '\n',
                    look: look_index,
                }),
            }
        }
        Self { chars, looks }
    }

    /// В тексте нет ни одного видимого символа (пусто или одни пробелы и переводы строк).
    pub(crate) fn is_blank(&self) -> bool {
        self.chars.iter().all(|source| source.ch.is_whitespace())
    }
}

#[cfg(test)]
mod tests {
    use subtitle_core::{FontWeight, SpanStyle, TextSpan};

    use super::*;
    use crate::event_style::{
        AxisPair, BaseDirection, BorderStyle, StyleColors, TextMargins, WrapStyle,
    };

    fn plain_style() -> EventStyle {
        let white = SubtitleColor {
            red: 255,
            green: 255,
            blue: 255,
            alpha: 255,
        };
        EventStyle {
            font_family: None,
            font_size: 40.0,
            font_weight: FontWeightClass::Regular,
            font_slant: FontStyle::Normal,
            border_style: BorderStyle::OutlineAndShadow,
            border: AxisPair::uniform(0.0),
            shadow: AxisPair::uniform(0.0),
            colors: StyleColors {
                primary: white,
                secondary: white,
                outline: white,
                back: white,
            },
            alignment: subtitle_core::ScreenAnchor::BottomCenter,
            margins: TextMargins {
                left: 0,
                right: 0,
                vertical: 0,
            },
            wrap_style: WrapStyle::Smart,
            line_spacing: 0.0,
            base_direction: BaseDirection::LeftToRight,
        }
    }

    #[test]
    fn spans_flatten_into_chars_with_shared_looks() {
        let bold = SpanStyle {
            weight: FontWeight::Bold,
            ..SpanStyle::default()
        };
        let text = StyledText::new(vec![
            TextSpan::text("a\tb\r", SpanStyle::default()),
            TextSpan::text("c", bold),
            TextSpan {
                content: SpanContent::LineBreak,
                ..TextSpan::text("", SpanStyle::default())
            },
            TextSpan::text("d", SpanStyle::default()),
        ]);
        let source = SourceText::from_styled(&text, &plain_style());
        let flat: String = source.chars.iter().map(|c| c.ch).collect();
        assert_eq!(flat, "a bc\nd", "tab becomes a space, CR is dropped");
        assert_eq!(source.looks.len(), 2, "plain and bold looks only");
        let weights: Vec<_> = source
            .chars
            .iter()
            .map(|c| source.looks[c.look].weight)
            .collect();
        assert_eq!(weights[3], FontWeightClass::Bold);
        assert_eq!(weights[0], FontWeightClass::Regular);
        // Повторный простой спан использует тот же индекс, что и первый.
        assert_eq!(source.chars[0].look, source.chars[5].look);
    }

    #[test]
    fn blank_text_is_detected() {
        let style = plain_style();
        let blank = StyledText::new(vec![TextSpan::text(" \t ", SpanStyle::default())]);
        assert!(SourceText::from_styled(&blank, &style).is_blank());
        assert!(SourceText::from_styled(&StyledText::new(vec![]), &style).is_blank());
        let visible = StyledText::new(vec![TextSpan::text(" x ", SpanStyle::default())]);
        assert!(!SourceText::from_styled(&visible, &style).is_blank());
    }
}
