//! Текст реплики со стилями автора: последовательность спанов.

use crate::color::SubtitleColor;
use crate::time::SubtitleTime;

/// Насыщенность шрифта.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Default)]
pub enum FontWeight {
    #[default]
    Normal,
    Bold,
}

/// Наклон шрифта.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Default)]
pub enum FontSlant {
    #[default]
    Normal,
    Italic,
}

/// Подчёркивание.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Default)]
pub enum TextUnderline {
    #[default]
    None,
    Single,
}

/// Зачёркивание.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Default)]
pub enum TextStrikethrough {
    #[default]
    None,
    Single,
}

/// Авторский стиль спана. По умолчанию всё «обычное», а `None` у цветов означает
/// «взять из пользовательского стиля» (он применяется позже, при растеризации).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Default)]
pub struct SpanStyle {
    pub weight: FontWeight,
    pub slant: FontSlant,
    pub underline: TextUnderline,
    pub strikethrough: TextStrikethrough,
    pub color: Option<SubtitleColor>,
    pub background: Option<SubtitleColor>,
}

/// Содержимое спана.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum SpanContent {
    /// Фрагмент текста без переводов строки.
    Text(String),
    /// Явный перевод строки внутри реплики.
    LineBreak,
}

/// Один фрагмент реплики с единым стилем.
///
/// Это простой value-тип без инвариантов, поэтому поля публичные: парсеры
/// заполняют их напрямую, а `..TextSpan::text(..)` удобен в тестах.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TextSpan {
    pub content: SpanContent,
    pub style: SpanStyle,
    /// Говорящий (`<v Имя>` в WebVTT).
    pub voice: Option<String>,
    /// Сырой языковой тег из `<lang ..>`; сознательно не валидируется.
    pub language: Option<String>,
    /// Аннотация ruby (`<rt>`) для этого спана; в `plain_text` не входит.
    pub ruby_annotation: Option<String>,
    /// Момент, с которого спан виден (караоке-теги WebVTT); `None` — виден сразу.
    pub reveal_at: Option<SubtitleTime>,
}

impl TextSpan {
    /// Текстовый спан без голоса, языка, ruby и отложенного показа.
    pub fn text(content: impl Into<String>, style: SpanStyle) -> Self {
        Self {
            content: SpanContent::Text(content.into()),
            style,
            voice: None,
            language: None,
            ruby_annotation: None,
            reveal_at: None,
        }
    }

    /// Перевод строки со стилем по умолчанию.
    pub fn line_break() -> Self {
        Self {
            content: SpanContent::LineBreak,
            ..Self::text(String::new(), SpanStyle::default())
        }
    }
}

/// Текст реплики целиком.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct StyledText {
    spans: Vec<TextSpan>,
}

impl StyledText {
    pub fn new(spans: Vec<TextSpan>) -> Self {
        Self { spans }
    }

    pub fn spans(&self) -> &[TextSpan] {
        &self.spans
    }

    /// Текст без стилей: `LineBreak` превращается в `\n`, ruby-аннотации не включаются.
    pub fn plain_text(&self) -> String {
        let mut text = String::new();
        for span in &self.spans {
            match &span.content {
                SpanContent::Text(fragment) => text.push_str(fragment),
                SpanContent::LineBreak => text.push('\n'),
            }
        }
        text
    }

    /// Нет ни одного спана.
    pub fn is_empty(&self) -> bool {
        self.spans.is_empty()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn plain_text_joins_spans_and_converts_line_breaks() {
        let bold = SpanStyle {
            weight: FontWeight::Bold,
            ..SpanStyle::default()
        };
        let text = StyledText::new(vec![
            TextSpan::text("Привет", bold),
            TextSpan::line_break(),
            TextSpan::text("мир", SpanStyle::default()),
        ]);
        assert_eq!(text.plain_text(), "Привет\nмир");
        assert!(!text.is_empty());
    }

    #[test]
    fn plain_text_excludes_ruby_annotation() {
        let mut span = TextSpan::text("漢字", SpanStyle::default());
        span.ruby_annotation = Some("かんじ".to_owned());
        assert_eq!(StyledText::new(vec![span]).plain_text(), "漢字");
    }

    #[test]
    fn empty_text_has_no_spans_and_empty_plain_text() {
        let text = StyledText::default();
        assert!(text.is_empty());
        assert_eq!(text.plain_text(), "");
    }

    #[test]
    fn default_style_defers_colors_to_user_style() {
        let style = SpanStyle::default();
        assert_eq!(style.color, None);
        assert_eq!(style.background, None);
        assert_eq!(style.weight, FontWeight::Normal);
    }
}
