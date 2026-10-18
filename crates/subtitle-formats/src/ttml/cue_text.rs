//! Сборка текста реплики из видимых листьев: пробелы по правилам TTML и переносы строк.
//!
//! `xml:space="default"`: любые подряд идущие пробельные символы (включая переводы строки
//! в исходнике) — один пробел; пробелы в начале строки и в конце строки (у `<br/>` и в конце
//! абзаца) отбрасываются. `xml:space="preserve"`: пробелы остаются как есть, а перевод строки
//! в тексте — перенос строки. Правила сквозные: пробел между двумя соседними `span`
//! сохраняется, как в браузере.

use subtitle_core::{SpanContent, SpanStyle, StyledText, TextSpan};

use super::model::SpaceHandling;
use super::paragraphs::{Leaf, LeafContent};

/// Строит `StyledText` из листьев, видимых одновременно (в порядке документа).
/// Пустой результат (нет видимого текста) возвращает `None`.
pub(super) fn build_styled_text(leaves: &[&Leaf]) -> Option<StyledText> {
    let mut builder = TextBuilder::default();
    for leaf in leaves {
        match &leaf.content {
            LeafContent::LineBreak => builder.line_break(),
            LeafContent::Text(text) => builder.append(text, leaf),
        }
    }
    builder.finish()
}

/// Состояние сборки: накопленные спаны и положение внутри строки.
struct TextBuilder {
    spans: Vec<TextSpan>,
    /// Пока в текущей строке нет ни одного видимого символа.
    at_line_start: bool,
    /// Был пробельный символ, который станет пробелом, если за ним последует текст в этой строке.
    pending_space: bool,
}

impl Default for TextBuilder {
    fn default() -> Self {
        Self {
            spans: Vec::new(),
            at_line_start: true,
            pending_space: false,
        }
    }
}

impl TextBuilder {
    fn line_break(&mut self) {
        self.pending_space = false;
        self.at_line_start = true;
        self.spans.push(TextSpan::line_break());
    }

    fn append(&mut self, text: &str, leaf: &Leaf) {
        let mut buffer = String::new();
        match leaf.space {
            SpaceHandling::Default => self.collapse_into(&mut buffer, text),
            SpaceHandling::Preserve => self.preserve_into(&mut buffer, text, leaf),
        }
        self.push_text(buffer, leaf);
    }

    /// Схлопывание пробелов (`xml:space="default"`).
    fn collapse_into(&mut self, buffer: &mut String, text: &str) {
        for character in text.chars() {
            if is_collapsible_space(character) {
                // В начале строки пробел не нужен вовсе.
                self.pending_space = !self.at_line_start;
                continue;
            }
            if self.pending_space {
                buffer.push(' ');
                self.pending_space = false;
            }
            buffer.push(character);
            self.at_line_start = false;
        }
    }

    /// Сохранение пробелов (`xml:space="preserve"`): `\n` становится переносом строки.
    fn preserve_into(&mut self, buffer: &mut String, text: &str, leaf: &Leaf) {
        self.pending_space = false;
        for character in text.chars() {
            match character {
                '\r' => {}
                '\n' => {
                    self.push_text(std::mem::take(buffer), leaf);
                    self.line_break();
                }
                other => {
                    buffer.push(other);
                    self.at_line_start = false;
                }
            }
        }
    }

    /// Добавляет текст, склеивая со спаном того же вида.
    fn push_text(&mut self, text: String, leaf: &Leaf) {
        if text.is_empty() {
            return;
        }
        if let Some(last) = self.spans.last_mut()
            && let SpanContent::Text(existing) = &mut last.content
            && last.style == leaf.style
            && last.language == leaf.language
        {
            existing.push_str(&text);
            return;
        }
        self.spans.push(span_of(text, leaf.style, leaf));
    }

    /// Убирает переносы строк по краям; без видимого текста возвращает `None`.
    fn finish(mut self) -> Option<StyledText> {
        while self
            .spans
            .last()
            .is_some_and(|span| span.content == SpanContent::LineBreak)
        {
            self.spans.pop();
        }
        let leading = self
            .spans
            .iter()
            .take_while(|span| span.content == SpanContent::LineBreak)
            .count();
        self.spans.drain(..leading);
        (!self.spans.is_empty()).then(|| StyledText::new(self.spans))
    }
}

fn span_of(text: String, style: SpanStyle, leaf: &Leaf) -> TextSpan {
    TextSpan {
        language: leaf.language.clone(),
        ..TextSpan::text(text, style)
    }
}

/// Пробельные символы XML (U+00A0 и подобные сюда не входят — это обычные символы).
fn is_collapsible_space(character: char) -> bool {
    matches!(character, ' ' | '\t' | '\n' | '\r')
}

#[cfg(test)]
mod tests {
    use subtitle_core::FontWeight;

    use super::*;
    use crate::ttml::timing::TimeWindow;

    fn leaf(content: LeafContent, space: SpaceHandling) -> Leaf {
        Leaf {
            window: TimeWindow::DOCUMENT,
            content,
            style: SpanStyle::default(),
            language: None,
            space,
        }
    }

    fn text_leaf(text: &str) -> Leaf {
        leaf(LeafContent::Text(text.to_owned()), SpaceHandling::Default)
    }

    fn plain(leaves: &[Leaf]) -> Option<String> {
        let refs: Vec<&Leaf> = leaves.iter().collect();
        build_styled_text(&refs).map(|text| text.plain_text())
    }

    #[test]
    fn runs_of_whitespace_collapse_and_edges_are_trimmed() {
        assert_eq!(
            plain(&[text_leaf("\n   Hello \t\n  world  \n")]).as_deref(),
            Some("Hello world")
        );
    }

    #[test]
    fn whitespace_around_line_breaks_is_dropped() {
        let leaves = [
            text_leaf("one   "),
            leaf(LeafContent::LineBreak, SpaceHandling::Default),
            text_leaf("   two"),
        ];
        assert_eq!(plain(&leaves).as_deref(), Some("one\ntwo"));
    }

    #[test]
    fn space_between_neighbouring_spans_survives_once() {
        let leaves = [text_leaf("a "), text_leaf(" b")];
        assert_eq!(plain(&leaves).as_deref(), Some("a b"));
    }

    #[test]
    fn preserve_keeps_spaces_and_turns_newlines_into_breaks() {
        let leaves = [leaf(
            LeafContent::Text("  a  b\r\nc ".to_owned()),
            SpaceHandling::Preserve,
        )];
        assert_eq!(plain(&leaves).as_deref(), Some("  a  b\nc "));
    }

    #[test]
    fn whitespace_only_or_break_only_content_is_empty() {
        assert_eq!(plain(&[text_leaf("  \n ")]), None);
        let leaves = [leaf(LeafContent::LineBreak, SpaceHandling::Default)];
        assert_eq!(plain(&leaves), None);
    }

    #[test]
    fn different_styles_stay_separate_spans() {
        let bold = Leaf {
            style: SpanStyle {
                weight: FontWeight::Bold,
                ..SpanStyle::default()
            },
            ..text_leaf("bold")
        };
        let leaves = [text_leaf("plain "), bold];
        let refs: Vec<&Leaf> = leaves.iter().collect();
        let text = build_styled_text(&refs).expect("text");
        assert_eq!(text.spans().len(), 2);
        assert_eq!(text.spans()[1].style.weight, FontWeight::Bold);
        assert_eq!(text.plain_text(), "plain bold");
    }
}
