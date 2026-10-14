//! Модель авторских стилей WebVTT: правила `::cue`, `::cue(.класс)`, `::cue(v[voice="…"])`.
//!
//! Таблица владеет порядком правил (каскад = порядок появления в файле) и знает,
//! как применять правила к стилю спана. Разбор CSS живёт в `style_block`.

use subtitle_core::{
    FontSlant, FontWeight, SpanStyle, SubtitleColor, TextStrikethrough, TextUnderline,
};

/// Какие элементы текста реплики правило затрагивает.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(super) enum CueSelector {
    /// `::cue` — весь текст каждой реплики.
    Base,
    /// `::cue(.имя)` — элементы с этим классом (`<c.имя>`, `<i.имя>`, `<v.имя …>` и т.д.).
    Class(String),
    /// `::cue(v[voice="Имя"])` — содержимое `<v Имя>`.
    Voice(String),
}

/// Набор поддерживаемых свойств одного правила; `None` — свойство правилом не задано.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub(super) struct StyleDeclarations {
    pub(super) color: Option<SubtitleColor>,
    pub(super) background: Option<SubtitleColor>,
    pub(super) weight: Option<FontWeight>,
    pub(super) slant: Option<FontSlant>,
    pub(super) underline: Option<TextUnderline>,
    pub(super) strikethrough: Option<TextStrikethrough>,
}

impl StyleDeclarations {
    /// Накладывает заданные свойства поверх `style`; незаданные не трогает.
    fn apply_to(&self, style: &mut SpanStyle) {
        if let Some(color) = self.color {
            style.color = Some(color);
        }
        if let Some(background) = self.background {
            style.background = Some(background);
        }
        if let Some(weight) = self.weight {
            style.weight = weight;
        }
        if let Some(slant) = self.slant {
            style.slant = slant;
        }
        if let Some(underline) = self.underline {
            style.underline = underline;
        }
        if let Some(strikethrough) = self.strikethrough {
            style.strikethrough = strikethrough;
        }
    }
}

/// Одно правило таблицы.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(super) struct StyleRule {
    pub(super) selector: CueSelector,
    pub(super) declarations: StyleDeclarations,
}

/// Элемент текста реплики с точки зрения селекторов.
#[derive(Debug, Clone, Copy)]
pub(super) struct ElementSelectorView<'element> {
    pub(super) classes: &'element [String],
    /// Имя говорящего, только у элемента `<v>`.
    pub(super) voice: Option<&'element str>,
}

/// Правила стилей, собранные из всех `STYLE`-блоков файла.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub(super) struct CueStyleSheet {
    rules: Vec<StyleRule>,
}

impl CueStyleSheet {
    pub(super) fn extend(&mut self, rules: impl IntoIterator<Item = StyleRule>) {
        self.rules.extend(rules);
    }

    /// Базовый стиль всей реплики (`::cue`), самая нижняя ступень каскада.
    pub(super) fn base_style(&self) -> SpanStyle {
        let mut style = SpanStyle::default();
        for rule in &self.rules {
            if rule.selector == CueSelector::Base {
                rule.declarations.apply_to(&mut style);
            }
        }
        style
    }

    /// Накладывает на `style` правила, подходящие элементу, в порядке появления в файле.
    pub(super) fn apply_for_element(
        &self,
        element: ElementSelectorView<'_>,
        style: &mut SpanStyle,
    ) {
        for rule in &self.rules {
            let matches = match &rule.selector {
                CueSelector::Base => false,
                CueSelector::Class(class) => element.classes.iter().any(|own| own == class),
                CueSelector::Voice(voice) => element.voice == Some(voice.as_str()),
            };
            if matches {
                rule.declarations.apply_to(style);
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn red() -> SubtitleColor {
        SubtitleColor::opaque(255, 0, 0)
    }

    fn rule(selector: CueSelector, declarations: StyleDeclarations) -> StyleRule {
        StyleRule {
            selector,
            declarations,
        }
    }

    #[test]
    fn base_style_collects_only_base_rules_in_source_order() {
        let mut sheet = CueStyleSheet::default();
        sheet.extend([
            rule(
                CueSelector::Base,
                StyleDeclarations {
                    color: Some(red()),
                    ..StyleDeclarations::default()
                },
            ),
            rule(
                CueSelector::Class("x".to_owned()),
                StyleDeclarations {
                    weight: Some(FontWeight::Bold),
                    ..StyleDeclarations::default()
                },
            ),
            rule(
                CueSelector::Base,
                StyleDeclarations {
                    color: Some(SubtitleColor::opaque(0, 0, 255)),
                    ..StyleDeclarations::default()
                },
            ),
        ]);
        let style = sheet.base_style();
        assert_eq!(style.color, Some(SubtitleColor::opaque(0, 0, 255)));
        assert_eq!(style.weight, FontWeight::Normal);
    }

    #[test]
    fn element_rules_match_by_class_or_voice_and_leave_other_fields() {
        let mut sheet = CueStyleSheet::default();
        sheet.extend([
            rule(
                CueSelector::Class("a".to_owned()),
                StyleDeclarations {
                    color: Some(red()),
                    ..StyleDeclarations::default()
                },
            ),
            rule(
                CueSelector::Voice("Bob".to_owned()),
                StyleDeclarations {
                    slant: Some(FontSlant::Italic),
                    ..StyleDeclarations::default()
                },
            ),
        ]);
        let classes = ["b".to_owned(), "a".to_owned()];
        let mut style = SpanStyle {
            weight: FontWeight::Bold,
            ..SpanStyle::default()
        };
        sheet.apply_for_element(
            ElementSelectorView {
                classes: &classes,
                voice: Some("Bob"),
            },
            &mut style,
        );
        assert_eq!(style.color, Some(red()));
        assert_eq!(style.slant, FontSlant::Italic);
        assert_eq!(style.weight, FontWeight::Bold);

        let mut untouched = SpanStyle::default();
        sheet.apply_for_element(
            ElementSelectorView {
                classes: &[],
                voice: Some("Alice"),
            },
            &mut untouched,
        );
        assert_eq!(untouched, SpanStyle::default());
    }
}
