//! Разметка внутри текста SRT: HTML-подобные теги и ASS-подобные блоки `{\...}`.
//!
//! Поддерживается: `<b> <i> <u> <s>`, `<font color=..>`, `<br>`, `\N` (перевод строки),
//! `{\anN}` (якорь экрана). Остальные теги и `{...}` вырезаются из текста. Всё, что
//! не похоже на тег (`a < b`), остаётся буквальным текстом.
//!
//! Известное ограничение: цвет `<font>`, который не удалось распознать, молча не применяется
//! (в отчёт такие случаи не попадают — это декор, а не повреждение реплики).

use subtitle_core::{
    FontSlant, FontWeight, ScreenAnchor, SpanStyle, StyledText, SubtitleColor, TextSpan,
    TextStrikethrough, TextUnderline,
};

/// Результат разбора разметки одной реплики.
pub(crate) struct SrtMarkup {
    pub(crate) text: StyledText,
    pub(crate) anchor: Option<ScreenAnchor>,
}

/// Открытый тег, влияющий на стиль. Вложенность хранится стеком.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum StyleTag {
    Bold,
    Italic,
    Underline,
    Strikethrough,
    /// `<font>`: цвет может отсутствовать, но закрывающий `</font>` всё равно должен найти пару.
    Font(Option<SubtitleColor>),
}

impl StyleTag {
    /// Тот же вид тега (цвет шрифта при сопоставлении `</font>` не важен).
    fn same_kind(self, other: Self) -> bool {
        std::mem::discriminant(&self) == std::mem::discriminant(&other)
    }

    fn apply_to(self, style: &mut SpanStyle) {
        match self {
            Self::Bold => style.weight = FontWeight::Bold,
            Self::Italic => style.slant = FontSlant::Italic,
            Self::Underline => style.underline = TextUnderline::Single,
            Self::Strikethrough => style.strikethrough = TextStrikethrough::Single,
            Self::Font(Some(color)) => style.color = Some(color),
            Self::Font(None) => {}
        }
    }
}

/// Разбирает разметку SRT. Текст уже без краевых пробелов, строки соединены `\n`.
pub(crate) fn parse_srt_markup(text: &str) -> SrtMarkup {
    let mut builder = MarkupBuilder::default();
    let mut rest = text;
    while let Some(ch) = rest.chars().next() {
        let consumed = match ch {
            '<' => builder.try_consume_angle_tag(rest),
            '{' => builder.try_consume_override_block(rest),
            '\\' if rest.starts_with("\\N") => {
                builder.push_line_break();
                Some("\\N".len())
            }
            '\n' => {
                builder.push_line_break();
                Some(1)
            }
            _ => None,
        };
        match consumed {
            Some(length) => rest = rest.get(length..).unwrap_or_default(),
            None => {
                builder.pending_text.push(ch);
                rest = rest.get(ch.len_utf8()..).unwrap_or_default();
            }
        }
    }
    builder.finish()
}

/// Накопитель спанов; владеет стеком стилей.
#[derive(Default)]
struct MarkupBuilder {
    spans: Vec<TextSpan>,
    /// Текст, накопленный под текущим стилем и ещё не превращённый в спан.
    pending_text: String,
    open_tags: Vec<StyleTag>,
    anchor: Option<ScreenAnchor>,
    /// В оставшемся тексте больше нет `}`: любой следующий `{` — обычный символ.
    no_closing_brace_ahead: bool,
}

impl MarkupBuilder {
    /// Стиль = свёртка стека открытых тегов; внутренние теги перекрывают внешние.
    fn current_style(&self) -> SpanStyle {
        let mut style = SpanStyle::default();
        for tag in &self.open_tags {
            tag.apply_to(&mut style);
        }
        style
    }

    /// Превращает накопленный текст в спан со стилем, действовавшим до смены тегов.
    fn flush_pending_text(&mut self) {
        if self.pending_text.is_empty() {
            return;
        }
        let style = self.current_style();
        let fragment = std::mem::take(&mut self.pending_text);
        self.spans.push(TextSpan::text(fragment, style));
    }

    fn push_line_break(&mut self) {
        self.flush_pending_text();
        self.spans.push(TextSpan::line_break());
    }

    fn open(&mut self, tag: StyleTag) {
        self.flush_pending_text();
        self.open_tags.push(tag);
    }

    /// Закрывает ближайший открытый тег того же вида; «лишний» закрывающий тег игнорируется.
    fn close(&mut self, tag: StyleTag) {
        let Some(position) = self.open_tags.iter().rposition(|open| open.same_kind(tag)) else {
            return;
        };
        self.flush_pending_text();
        self.open_tags.remove(position);
    }

    /// Пытается разобрать тег в начале `rest`; возвращает длину съеденного или `None`,
    /// если `<` — просто символ.
    fn try_consume_angle_tag(&mut self, rest: &str) -> Option<usize> {
        // Ищем ближайший из `<`/`>` после открывающего `<` одним проходом. Если первым встретился
        // другой `<` (или ничего нет), исходный `<` был обычным символом. Так цена разбора
        // линейна: «лишний» `<` не заставляет каждый раз сканировать весь остаток реплики.
        let after_open = rest.get(1..)?;
        let relative_close = after_open.find(['<', '>'])?;
        if after_open.as_bytes().get(relative_close) != Some(&b'>') {
            return None;
        }
        let close_index = relative_close + 1;
        let inner = rest.get(1..close_index)?;
        let (is_closing, body) = match inner.strip_prefix('/') {
            Some(body) => (true, body),
            None => (false, inner),
        };
        if !body.starts_with(|ch: char| ch.is_ascii_alphabetic()) {
            return None;
        }
        let name_end = body
            .find(|ch: char| ch.is_whitespace() || ch == '/')
            .unwrap_or(body.len());
        let name = body.get(..name_end)?.to_ascii_lowercase();
        let attributes = body.get(name_end..).unwrap_or_default();
        let tag = match name.as_str() {
            "b" => Some(StyleTag::Bold),
            "i" => Some(StyleTag::Italic),
            "u" => Some(StyleTag::Underline),
            "s" => Some(StyleTag::Strikethrough),
            "font" => Some(StyleTag::Font(find_font_color(attributes))),
            "br" if !is_closing => {
                self.push_line_break();
                None
            }
            // Неизвестные теги вырезаем, их содержимое остаётся.
            _ => None,
        };
        if let Some(tag) = tag {
            if is_closing {
                self.close(tag);
            } else {
                self.open(tag);
            }
        }
        Some(close_index + 1)
    }

    /// Блок `{...}`: ищем `\anN`, весь блок вырезается из текста.
    fn try_consume_override_block(&mut self, rest: &str) -> Option<usize> {
        if self.no_closing_brace_ahead {
            return None;
        }
        let Some(close_index) = rest.find('}') else {
            // Раз `}` нет в этом остатке, его нет и в более коротких: повторно не ищем (иначе
            // строка из `{{{{…` стоила бы O(n^2)).
            self.no_closing_brace_ahead = true;
            return None;
        };
        let inner = rest.get(1..close_index)?;
        if self.anchor.is_none() {
            self.anchor = find_numpad_anchor(inner);
        }
        Some(close_index + 1)
    }

    fn finish(mut self) -> SrtMarkup {
        // Незакрытые теги закрываются неявно концом реплики.
        self.flush_pending_text();
        SrtMarkup {
            text: trim_edge_line_breaks(self.spans),
            anchor: self.anchor,
        }
    }
}

/// Убирает пустые края: переводы строки в начале/конце и пробельные спаны-пустышки.
fn trim_edge_line_breaks(mut spans: Vec<TextSpan>) -> StyledText {
    let is_blank = |span: &TextSpan| match &span.content {
        subtitle_core::SpanContent::LineBreak => true,
        subtitle_core::SpanContent::Text(text) => text.trim().is_empty(),
    };
    while spans.last().is_some_and(is_blank) {
        spans.pop();
    }
    let leading_blank = spans.iter().take_while(|span| is_blank(span)).count();
    spans.drain(..leading_blank);
    StyledText::new(spans)
}

/// Находит `\anN` (N = 1..=9) в содержимом `{...}`.
fn find_numpad_anchor(block: &str) -> Option<ScreenAnchor> {
    let after = block.split("\\an").nth(1)?;
    let digit = after.chars().next()?.to_digit(10)?;
    ScreenAnchor::from_numpad(u8::try_from(digit).ok()?)
}

/// Ищет атрибут `color` у `<font ...>` (имя атрибута без учёта регистра).
fn find_font_color(attributes: &str) -> Option<SubtitleColor> {
    let mut rest = attributes.trim_start();
    while !rest.is_empty() {
        let key_end = rest
            .find(|ch: char| ch == '=' || ch.is_whitespace())
            .unwrap_or(rest.len());
        let key = rest.get(..key_end)?;
        let after_key = rest.get(key_end..)?.trim_start();
        let Some(after_equals) = after_key.strip_prefix('=') else {
            rest = after_key;
            continue;
        };
        let (value, remainder) = split_attribute_value(after_equals.trim_start());
        if key.eq_ignore_ascii_case("color") {
            return parse_font_color(value);
        }
        rest = remainder.trim_start();
    }
    None
}

/// Значение в кавычках `"..."`/`'...'` или без кавычек (до пробела). Возвращает (значение, остаток).
fn split_attribute_value(text: &str) -> (&str, &str) {
    if let Some(quote) = text.chars().next().filter(|ch| matches!(ch, '"' | '\'')) {
        let body = text.get(1..).unwrap_or_default();
        return match body.find(quote) {
            Some(end) => (
                body.get(..end).unwrap_or_default(),
                body.get(end + 1..).unwrap_or_default(),
            ),
            None => (body, ""),
        };
    }
    let end = text
        .find(|ch: char| ch.is_whitespace() || ch == '/')
        .unwrap_or(text.len());
    (
        text.get(..end).unwrap_or_default(),
        text.get(end..).unwrap_or_default(),
    )
}

/// Имя цвета проверяем раньше hex: `red` не hex, а вот `bed`/`fade` — hex, но не имена.
fn parse_font_color(value: &str) -> Option<SubtitleColor> {
    let value = value.trim();
    SubtitleColor::from_css_name(value)
        .or_else(|_| SubtitleColor::from_hex_rgb_str(value))
        .ok()
}

#[cfg(test)]
mod tests {
    use super::*;
    use subtitle_core::SpanContent;

    fn spans_of(text: &str) -> Vec<(String, SpanStyle)> {
        parse_srt_markup(text)
            .text
            .spans()
            .iter()
            .map(|span| match &span.content {
                SpanContent::Text(text) => (text.clone(), span.style),
                SpanContent::LineBreak => ("\n".to_owned(), span.style),
            })
            .collect()
    }

    #[test]
    fn nested_tags_stack_and_unwind() {
        let spans = spans_of("<b>a<i>b</i>c</b>d");
        assert_eq!(spans.len(), 4);
        assert_eq!(spans[0].1.weight, FontWeight::Bold);
        assert_eq!(spans[0].1.slant, FontSlant::Normal);
        assert_eq!(spans[1].1.slant, FontSlant::Italic);
        assert_eq!(spans[1].1.weight, FontWeight::Bold);
        assert_eq!(spans[2].1.slant, FontSlant::Normal);
        assert_eq!(spans[3].1, SpanStyle::default());
    }

    #[test]
    fn unclosed_tag_lasts_until_end_and_stray_closer_is_ignored() {
        let spans = spans_of("</i>x<u>y");
        assert_eq!(spans[0], ("x".to_owned(), SpanStyle::default()));
        assert_eq!(spans[1].1.underline, TextUnderline::Single);
    }

    #[test]
    fn tags_are_case_insensitive_and_unknown_tags_are_stripped() {
        let spans = spans_of("<I>a</I><span class=x>b</span>");
        assert_eq!(spans[0].1.slant, FontSlant::Italic);
        assert_eq!(spans[1], ("b".to_owned(), SpanStyle::default()));
    }

    #[test]
    fn lone_angle_bracket_is_literal_text() {
        assert_eq!(parse_srt_markup("a < b > c").text.plain_text(), "a < b > c");
        assert_eq!(parse_srt_markup("1 <3 2").text.plain_text(), "1 <3 2");
    }

    #[test]
    fn font_color_accepts_hash_name_single_quotes_and_unquoted() {
        let red = Some(SubtitleColor::opaque(255, 0, 0));
        for source in [
            r##"<font color="#ff0000">x</font>"##,
            r#"<font face="Arial" color='red'>x</font>"#,
            "<FONT COLOR=ff0000>x</FONT>",
            "<font color=#f00>x</font>",
        ] {
            assert_eq!(spans_of(source)[0].1.color, red, "{source}");
        }
    }

    #[test]
    fn unrecognized_font_color_keeps_text_without_color() {
        let spans = spans_of(r#"<font color="nonsense">x</font>y"#);
        assert_eq!(spans[0], ("x".to_owned(), SpanStyle::default()));
    }

    #[test]
    fn anchor_block_is_stripped_and_sets_anchor() {
        let markup = parse_srt_markup(r"{\an8}Top");
        assert_eq!(markup.text.plain_text(), "Top");
        assert_eq!(markup.anchor, Some(ScreenAnchor::TopCenter));
    }

    #[test]
    fn other_override_blocks_are_stripped_and_invalid_anchor_ignored() {
        let markup = parse_srt_markup(r"{\pos(10,20)}{\an0}Hi {unclosed");
        assert_eq!(markup.text.plain_text(), "Hi {unclosed");
        assert_eq!(markup.anchor, None);
    }

    #[test]
    fn backslash_n_and_br_become_line_breaks() {
        assert_eq!(parse_srt_markup(r"a\Nb<br/>c").text.plain_text(), "a\nb\nc");
    }

    #[test]
    fn tag_only_text_is_empty() {
        assert!(parse_srt_markup("<i></i>{\\an8}").text.is_empty());
    }
}
