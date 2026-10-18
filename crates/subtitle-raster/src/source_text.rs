//! Плоский текст реплики: символы с индексом внешнего вида.
//!
//! [`StyledText`] хранит спаны; конвейер libass работает с массивом символов, у каждого из
//! которых «внешний вид» (цвет, насыщенность, наклон). Здесь спаны разворачиваются в такой
//! массив, а одинаковые внешние виды получают один индекс — тогда проверка «стиль изменился,
//! начать новый участок» (libass: `split_style_runs`) сводится к сравнению индексов.

use subtitle_core::{
    FontSlant, FontWeight, SpanContent, SpanStyle, StyledText, TextStrikethrough, TextUnderline,
};

use crate::event_fade::EventFade;
use crate::event_style::{AxisPair, EventStyle};
use crate::fonts::{FontStyle, FontWeightClass};
use crate::glyph_transform::{Rotation3d, ShearFactors};
use crate::karaoke::KaraokeMark;
pub(crate) use crate::span_look::{SpanBlur, SpanLook, TextDecorations};

/// Символ-заместитель рисунка (U+FFFC OBJECT REPLACEMENT CHARACTER, libass: `code = 0xfffc`).
pub(crate) const DRAWING_SYMBOL: char = '\u{fffc}';

/// Номер рисунка в [`SourceText::drawings`].
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) struct DrawingId(pub(crate) usize);

/// Рисунок `\p<n>` в тексте реплики: сырой текст команд и параметры размещения, которые
/// планировщик реплики знает, а конвейер текста — нет (масштаб кадра, `\pbo`).
///
/// В libass рисунок — один псевдоглиф (`GlyphInfo::drawing_text`): без шейпинга и поиска
/// шрифта, со своими метриками (`get_outline_glyph`).
#[derive(Debug, Clone, PartialEq)]
pub(crate) struct DrawingSource {
    /// Команды рисования как написано в реплике (`m 0 0 l 10 0 ...`).
    pub(crate) commands: String,
    /// Масштаб контура рисунка в пиксели кадра по осям (libass: `transform.scale` в
    /// `get_outline_glyph`): `\fscx`/`\fscy` * `2^-(n-1)` * масштаб кадра (по X ещё делится
    /// на `par_scale_x`).
    pub(crate) outline_scale: AxisPair,
    /// Смещение базовой линии `\pbo` в единицах рисунка, усечённое до целого (libass:
    /// `drawing_pbo`).
    pub(crate) baseline_offset: i32,
}

/// Символ реплики и индекс его внешнего вида в [`SourceText::looks`].
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) struct SourceChar {
    /// Символ (`'\n'` — явный перевод строки).
    pub(crate) ch: char,
    /// Индекс в [`SourceText::looks`].
    pub(crate) look: usize,
    /// Затухание символа (libass: `info->fade = state->fade` в момент добавления символа).
    /// Не входит в вид: libass не режет участки по затуханию, а берёт его у ПЕРВОГО символа
    /// участка (см. `compose`). У SRT/WebVTT затухания нет.
    pub(crate) fade: EventFade,
    /// Символ — заместитель рисунка (`ch == DRAWING_SYMBOL`): его глиф строится из команд
    /// рисования, а не из шрифта. У обычных символов `None`.
    pub(crate) drawing: Option<DrawingId>,
    /// Теги караоке, накопленные к этому символу (libass: копия `effect_*` в `GlyphInfo`).
    /// У SRT/WebVTT — [`KaraokeMark::NONE`].
    pub(crate) karaoke: KaraokeMark,
}

/// Включён ли кернинг пар в шейпинге (libass: `Kerning:` заголовка ASS, `ass_shaper_set_kerning`).
///
/// У SRT/WebVTT кернинг выключен, как у libass по умолчанию.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub(crate) enum KerningMode {
    /// Кернинг выключен.
    #[default]
    Disabled,
    /// Кернинг включён.
    Enabled,
}

/// Развёрнутый текст реплики.
#[derive(Debug, Clone, PartialEq)]
pub(crate) struct SourceText {
    /// Символы в порядке чтения (логический порядок).
    pub(crate) chars: Vec<SourceChar>,
    /// Различные внешние виды; индексируются из [`SourceChar::look`].
    pub(crate) looks: Vec<SpanLook>,
    /// Кернинг пар при шейпинге.
    pub(crate) kerning: KerningMode,
    /// Рисунки реплики; индексируются из [`SourceChar::drawing`]. У SRT/WebVTT пусто.
    pub(crate) drawings: Vec<DrawingSource>,
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
            let look = Self::span_look(&span.style, style);
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
                                fade: EventFade::NONE,
                                drawing: None,
                                karaoke: KaraokeMark::NONE,
                            }),
                            other => chars.push(SourceChar {
                                ch: other,
                                look: look_index,
                                fade: EventFade::NONE,
                                drawing: None,
                                karaoke: KaraokeMark::NONE,
                            }),
                        }
                    }
                }
                SpanContent::LineBreak => chars.push(SourceChar {
                    ch: '\n',
                    look: look_index,
                    fade: EventFade::NONE,
                    drawing: None,
                    karaoke: KaraokeMark::NONE,
                }),
            }
        }
        Self {
            chars,
            looks,
            kerning: KerningMode::Disabled,
            drawings: Vec::new(),
        }
    }

    /// Вид символов спана: стиль реплики плюс авторские цвет, жирность и наклон спана.
    ///
    /// Подчёркивание и зачёркивание берутся из спана (SRT `<u>`/`<s>`, WebVTT `text-decoration`).
    fn span_look(span_style: &SpanStyle, style: &EventStyle) -> SpanLook {
        let (author_color, author_weight, author_slant) =
            (span_style.color, span_style.weight, span_style.slant);
        let weight = if author_weight == FontWeight::Bold {
            FontWeightClass::Bold
        } else {
            style.font_weight
        };
        let mut colors = style.colors;
        if let Some(color) = author_color {
            colors.primary = color;
        }
        SpanLook {
            font_family: style.font_family.clone(),
            font_size: style.font_size,
            weight,
            weight_value: if weight == FontWeightClass::Bold {
                700
            } else {
                400
            },
            slant: if author_slant == FontSlant::Italic {
                FontStyle::Italic
            } else {
                style.font_slant
            },
            colors,
            border: style.border,
            shadow: style.shadow,
            border_style: style.border_style,
            scale: crate::event_style::AxisPair::uniform(1.0),
            letter_spacing: 0.0,
            decorations: TextDecorations {
                underline: span_style.underline != TextUnderline::None,
                strike_out: span_style.strikethrough != TextStrikethrough::None,
            },
            rotation: Rotation3d::default(),
            shear: ShearFactors::default(),
            blur: SpanBlur::NONE,
            camera_scale: 1.0,
        }
    }

    /// В тексте нет ни одного видимого символа (пусто или одни пробелы и переводы строк).
    pub(crate) fn is_blank(&self) -> bool {
        self.chars.iter().all(|source| source.ch.is_whitespace())
    }
}

#[cfg(test)]
mod tests {
    use subtitle_core::{FontWeight, SpanStyle, SubtitleColor, TextSpan};

    use super::*;
    use crate::event_fade::EventFade;
    use crate::event_style::{
        AxisPair, BaseDirection, BorderStyle, BreakOpportunities, EventPlacement, StyleColors,
        TextMargins, WrapStyle,
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
            placement: EventPlacement::ByMargins,
            wrap_style: WrapStyle::Smart,
            break_opportunities: BreakOpportunities::SpacesOnly,
            line_spacing: 0.0,
            base_direction: BaseDirection::LeftToRight,
            fade: EventFade::NONE,
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
