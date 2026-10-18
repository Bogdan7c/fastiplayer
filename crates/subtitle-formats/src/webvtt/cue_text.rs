//! Разбор текста реплики WebVTT в [`StyledText`]: теги, классы, голос, язык, ruby,
//! караоке-метки времени и символьные сущности.
//!
//! Модель: стек открытых элементов. Стиль спана = базовый `::cue` + для каждого открытого
//! элемента (снаружи внутрь) правила по его классам/голосу + собственный эффект тега
//! (`<b>`, `<i>`, `<u>`). Поэтому тег всегда перекрывает стиль из CSS, а внутренний
//! элемент — внешний.
//!
//! Устойчивость: незакрытые теги закрываются концом текста, «лишние» закрывающие теги
//! игнорируются (закрывается только элемент на вершине стека, как в W3C), неизвестные
//! теги вырезаются вместе с классами, но их содержимое остаётся.

use subtitle_core::{
    FontSlant, FontWeight, SpanStyle, StyledText, SubtitleTime, TextSpan, TextUnderline,
};

use super::entities::decode_entity;
use super::style_sheet::{CueStyleSheet, ElementSelectorView};
use super::tag::{ElementKind, ElementTag, TagContent, scan_angle_tag};

/// Интервал показа реплики: метки времени вне него не имеют смысла.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) struct CueTimeWindow {
    pub(super) start: SubtitleTime,
    pub(super) end: SubtitleTime,
}

impl CueTimeWindow {
    /// Метка полезна, только если строго внутри интервала: на `start` текст и так виден,
    /// а на `end` и позже реплика уже скрыта — такая метка считается ошибкой автора и
    /// игнорируется (текст после неё не откладывается).
    fn contains_reveal_moment(&self, moment: SubtitleTime) -> bool {
        moment > self.start && moment < self.end
    }
}

/// Разбирает текст реплики (строки уже склеены через `\n`).
pub(super) fn parse_cue_text(
    text: &str,
    window: CueTimeWindow,
    style_sheet: &CueStyleSheet,
) -> StyledText {
    let mut builder = CueTextBuilder::new(window, style_sheet);
    let mut rest = text;
    while !rest.is_empty() {
        // Обычный текст до ближайшего специального символа переносим пачкой.
        let special_index = rest.find(['<', '&', '\n']).unwrap_or(rest.len());
        let (plain, tail) = rest.split_at(special_index);
        builder.push_text(plain);
        let Some(special) = tail.chars().next() else {
            break;
        };
        let consumed = match special {
            '<' => builder.try_consume_tag(tail),
            '&' => builder.try_consume_entity(tail),
            _ => {
                builder.push_line_break();
                Some(special.len_utf8())
            }
        };
        // Не распознанный `<` или `&` — обычный символ.
        let consumed = consumed.unwrap_or_else(|| {
            builder.push_text(tail.get(..special.len_utf8()).unwrap_or_default());
            special.len_utf8()
        });
        rest = tail.get(consumed..).unwrap_or_default();
    }
    builder.finish()
}

/// Открытый элемент разметки.
struct OpenElement {
    kind: ElementKind,
    classes: Vec<String>,
    /// Имя говорящего (только `<v>`).
    voice: Option<String>,
    /// Языковой тег (только `<lang>`).
    language: Option<String>,
    /// Итоговый стиль под этим элементом: стиль родителя (или `::cue`) + правила и собственный
    /// эффект самого элемента. Считается один раз при открытии, чтобы стоимость сброса текста
    /// не росла с глубиной стека (иначе тысячи незакрытых `<c>` дают O(n^2)).
    style_in_scope: SpanStyle,
    /// Индекс в стеке ближайшего снаружи (включая себя) элемента с голосом.
    voice_owner: Option<usize>,
    /// То же для языка (`<lang>`).
    language_owner: Option<usize>,
}

impl OpenElement {
    /// Собственный эффект тега: `<b>`, `<i>`, `<u>`; остальные виды стиль не меняют.
    fn apply_intrinsic_style(&self, style: &mut SpanStyle) {
        match self.kind {
            ElementKind::Bold => style.weight = FontWeight::Bold,
            ElementKind::Italic => style.slant = FontSlant::Italic,
            ElementKind::Underline => style.underline = TextUnderline::Single,
            _ => {}
        }
    }
}

/// Накопитель спанов; владеет стеком элементов и состоянием ruby/караоке.
struct CueTextBuilder<'sheet> {
    window: CueTimeWindow,
    style_sheet: &'sheet CueStyleSheet,
    /// Стиль `::cue`, вычисленный один раз на реплику.
    base_style: SpanStyle,
    spans: Vec<TextSpan>,
    /// Текст под текущим стеком, ещё не превращённый в спан.
    pending_text: String,
    open_elements: Vec<OpenElement>,
    /// Последняя действующая караоке-метка: ей помечаются все следующие спаны.
    reveal_at: Option<SubtitleTime>,
    /// Для каждого открытого `<ruby>` — индекс первого спана его базового текста.
    ruby_base_starts: Vec<usize>,
    /// Для каждого открытого `<rt>` — накопленный текст аннотации.
    annotation_texts: Vec<String>,
}

impl<'sheet> CueTextBuilder<'sheet> {
    fn new(window: CueTimeWindow, style_sheet: &'sheet CueStyleSheet) -> Self {
        Self {
            window,
            style_sheet,
            base_style: style_sheet.base_style(),
            spans: Vec::new(),
            pending_text: String::new(),
            open_elements: Vec::new(),
            reveal_at: None,
            ruby_base_starts: Vec::new(),
            annotation_texts: Vec::new(),
        }
    }

    /// Текст внутри `<rt>` — это аннотация, а не часть реплики.
    fn push_text(&mut self, fragment: &str) {
        match self.annotation_texts.last_mut() {
            Some(annotation) => annotation.push_str(fragment),
            None => self.pending_text.push_str(fragment),
        }
    }

    fn push_line_break(&mut self) {
        if let Some(annotation) = self.annotation_texts.last_mut() {
            annotation.push(' ');
            return;
        }
        self.flush_pending_text();
        let mut line_break = TextSpan::line_break();
        line_break.reveal_at = self.reveal_at;
        self.spans.push(line_break);
    }

    /// `&…;` в начале `rest`; длина съеденного или `None`, если это просто `&`.
    fn try_consume_entity(&mut self, rest: &str) -> Option<usize> {
        let (decoded, consumed) = decode_entity(rest)?;
        let mut buffer = [0_u8; 4];
        self.push_text(decoded.encode_utf8(&mut buffer));
        Some(consumed)
    }

    /// Тег в начале `rest`; длина съеденного или `None`, если `<` — просто символ.
    fn try_consume_tag(&mut self, rest: &str) -> Option<usize> {
        let scanned = scan_angle_tag(rest)?;
        match scanned.content {
            TagContent::OpeningElement(tag) => self.open_element(tag),
            TagContent::ClosingElement(tag) => {
                if let Some(kind) = tag.kind {
                    self.close_element(kind);
                }
            }
            TagContent::Timestamp(moment) => self.apply_timestamp(moment),
        }
        Some(scanned.consumed_bytes)
    }

    fn apply_timestamp(&mut self, moment: Option<SubtitleTime>) {
        let Some(moment) = moment.filter(|m| self.window.contains_reveal_moment(*m)) else {
            return;
        };
        self.flush_pending_text();
        self.reveal_at = Some(moment);
    }

    fn open_element(&mut self, tag: ElementTag) {
        let Some(kind) = tag.kind else {
            return;
        };
        // `<rt>` допустим только прямо внутри `<ruby>`.
        if kind == ElementKind::RubyText && !self.top_is(ElementKind::Ruby) {
            return;
        }
        self.flush_pending_text();
        match kind {
            ElementKind::Ruby => self.ruby_base_starts.push(self.spans.len()),
            ElementKind::RubyText => self.annotation_texts.push(String::new()),
            _ => {}
        }
        let annotation = (!tag.annotation.is_empty()).then_some(tag.annotation);
        let mut element = OpenElement {
            kind,
            classes: tag.classes,
            voice: annotation.clone().filter(|_| kind == ElementKind::Voice),
            language: annotation.filter(|_| kind == ElementKind::Language),
            style_in_scope: self.current_style(),
            voice_owner: None,
            language_owner: None,
        };
        // Селекторы смотрят только на сам элемент (без потомков/предков), поэтому каскад — это
        // свёртка по стеку, и достаточно дополнить стиль родителя правилами нового элемента.
        let view = ElementSelectorView {
            classes: &element.classes,
            voice: element.voice.as_deref(),
        };
        let mut style_in_scope = element.style_in_scope;
        self.style_sheet
            .apply_for_element(view, &mut style_in_scope);
        element.apply_intrinsic_style(&mut style_in_scope);
        element.style_in_scope = style_in_scope;
        let own_index = self.open_elements.len();
        let parent = self.open_elements.last();
        element.voice_owner = if element.voice.is_some() {
            Some(own_index)
        } else {
            parent.and_then(|parent| parent.voice_owner)
        };
        element.language_owner = if element.language.is_some() {
            Some(own_index)
        } else {
            parent.and_then(|parent| parent.language_owner)
        };
        self.open_elements.push(element);
    }

    /// Закрывает элемент, только если он на вершине стека (иначе тег лишний и игнорируется).
    /// `</ruby>` при открытом `<rt>` сначала закрывает `<rt>`.
    fn close_element(&mut self, kind: ElementKind) {
        if kind == ElementKind::Ruby && self.top_is(ElementKind::RubyText) {
            self.pop_top_element();
        }
        if self.top_is(kind) {
            self.pop_top_element();
        }
    }

    fn top_is(&self, kind: ElementKind) -> bool {
        self.open_elements
            .last()
            .is_some_and(|element| element.kind == kind)
    }

    /// Снимает вершину стека и завершает связанное с ней состояние ruby.
    fn pop_top_element(&mut self) {
        self.flush_pending_text();
        let Some(element) = self.open_elements.pop() else {
            return;
        };
        match element.kind {
            ElementKind::RubyText => self.finish_ruby_annotation(),
            ElementKind::Ruby => {
                self.ruby_base_starts.pop();
            }
            _ => {}
        }
    }

    /// Закрытие `<rt>`: аннотация привязывается к первому ещё не аннотированному
    /// текстовому спану базы, и база следующей пары начинается после неё.
    ///
    /// Ограничение модели: у спана одна аннотация, поэтому при нескольких спанах базы
    /// (`<ruby><b>漢</b>字<rt>…`) она ляжет на первый из них.
    fn finish_ruby_annotation(&mut self) {
        let annotation = self.annotation_texts.pop().unwrap_or_default();
        let annotation = annotation.split_whitespace().collect::<Vec<_>>().join(" ");
        let base_start = self.ruby_base_starts.last().copied().unwrap_or_default();
        if !annotation.is_empty() {
            let target = self
                .spans
                .iter_mut()
                .skip(base_start)
                .find(|span| span.ruby_annotation.is_none() && is_text_span(span));
            if let Some(span) = target {
                span.ruby_annotation = Some(annotation);
            }
        }
        if let Some(start) = self.ruby_base_starts.last_mut() {
            *start = self.spans.len();
        }
    }

    /// Стиль под текущим стеком (см. модульную документацию): O(1), берётся с вершины.
    fn current_style(&self) -> SpanStyle {
        self.open_elements
            .last()
            .map_or(self.base_style, |top| top.style_in_scope)
    }

    /// Ближайший снаружи голос/язык: внутренний элемент перекрывает внешний.
    fn current_voice(&self) -> Option<String> {
        let owner = self.open_elements.last()?.voice_owner?;
        self.open_elements.get(owner)?.voice.clone()
    }

    fn current_language(&self) -> Option<String> {
        let owner = self.open_elements.last()?.language_owner?;
        self.open_elements.get(owner)?.language.clone()
    }

    /// Превращает накопленный текст в спан со стилем, действовавшим до смены тегов.
    fn flush_pending_text(&mut self) {
        if self.pending_text.is_empty() {
            return;
        }
        let mut span = TextSpan::text(std::mem::take(&mut self.pending_text), self.current_style());
        span.voice = self.current_voice();
        span.language = self.current_language();
        span.reveal_at = self.reveal_at;
        self.spans.push(span);
    }

    fn finish(mut self) -> StyledText {
        // Незакрытые элементы закрываются концом реплики (каждый шаг снимает вершину стека).
        while let Some(top) = self.open_elements.last().map(|element| element.kind) {
            self.close_element(top);
        }
        self.flush_pending_text();
        StyledText::new(self.spans)
    }
}

fn is_text_span(span: &TextSpan) -> bool {
    matches!(span.content, subtitle_core::SpanContent::Text(_))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::webvtt::style_block::parse_style_block;
    use subtitle_core::{SpanContent, SubtitleColor};

    const WINDOW: CueTimeWindow = CueTimeWindow {
        start: SubtitleTime::from_millis(1_000),
        end: SubtitleTime::from_millis(5_000),
    };

    fn parse(text: &str) -> Vec<TextSpan> {
        parse_with(text, &CueStyleSheet::default())
    }

    fn parse_with(text: &str, sheet: &CueStyleSheet) -> Vec<TextSpan> {
        parse_cue_text(text, WINDOW, sheet).spans().to_vec()
    }

    fn sheet_from_css(css: &str) -> CueStyleSheet {
        let mut sheet = CueStyleSheet::default();
        sheet.extend(parse_style_block(css).rules);
        sheet
    }

    fn texts(spans: &[TextSpan]) -> Vec<String> {
        spans
            .iter()
            .map(|span| match &span.content {
                SpanContent::Text(text) => text.clone(),
                SpanContent::LineBreak => "\n".to_owned(),
            })
            .collect()
    }

    #[test]
    fn plain_text_and_newline_become_text_and_line_break_spans() {
        let spans = parse("one\ntwo");
        assert_eq!(texts(&spans), ["one", "\n", "two"]);
        assert_eq!(spans[1].content, SpanContent::LineBreak);
    }

    #[test]
    fn bold_italic_underline_nest_and_close_independently() {
        let spans = parse("<b>a<i>b</i>c</b>d<u>e</u>");
        assert_eq!(texts(&spans), ["a", "b", "c", "d", "e"]);
        assert_eq!(spans[0].style.weight, FontWeight::Bold);
        assert_eq!(spans[0].style.slant, FontSlant::Normal);
        assert_eq!(spans[1].style.weight, FontWeight::Bold);
        assert_eq!(spans[1].style.slant, FontSlant::Italic);
        assert_eq!(spans[2].style.slant, FontSlant::Normal);
        assert_eq!(spans[3].style, SpanStyle::default());
        assert_eq!(spans[4].style.underline, TextUnderline::Single);
    }

    #[test]
    fn unclosed_tags_are_closed_by_end_and_stray_closers_are_ignored() {
        let spans = parse("</i>x<b>y<i>z");
        assert_eq!(texts(&spans), ["x", "y", "z"]);
        assert_eq!(spans[0].style, SpanStyle::default());
        assert_eq!(spans[1].style.weight, FontWeight::Bold);
        assert_eq!(spans[2].style.slant, FontSlant::Italic);
    }

    #[test]
    fn misnested_closer_only_closes_the_top_element() {
        // `</b>` при открытом `<i>` игнорируется (W3C), поэтому "y" остаётся жирным и
        // курсивным и сливается с "x" в один спан; после `</i>` остаётся только жирный.
        let spans = parse("<b><i>x</b>y</i>z");
        assert_eq!(texts(&spans), ["xy", "z"]);
        assert_eq!(spans[0].style.weight, FontWeight::Bold);
        assert_eq!(spans[0].style.slant, FontSlant::Italic);
        assert_eq!(spans[1].style.weight, FontWeight::Bold);
        assert_eq!(spans[1].style.slant, FontSlant::Normal);
    }

    #[test]
    fn lone_less_than_and_unterminated_tag_stay_literal() {
        assert_eq!(texts(&parse("a < b and <i")), ["a < b and <i"]);
        assert_eq!(texts(&parse("2 <3 u")), ["2 <3 u"]);
        assert_eq!(texts(&parse("x > y")), ["x > y"]);
    }

    #[test]
    fn unknown_tags_are_stripped_but_text_is_kept() {
        let spans = parse("a<font color=red>b</font><B>c</B>");
        assert_eq!(texts(&spans), ["abc"]);
        assert!(spans.iter().all(|span| span.style == SpanStyle::default()));
    }

    #[test]
    fn entities_decode_and_unknown_ampersands_stay() {
        let spans = parse("Tom &amp; Jerry &lt;3 &#65;&#x42; AT&T &nbsp;&bogus;");
        assert_eq!(
            texts(&spans),
            ["Tom & Jerry <3 AB AT&T \u{A0}&bogus;".to_owned()]
        );
    }

    #[test]
    fn entity_produced_angle_bracket_is_not_a_tag() {
        assert_eq!(texts(&parse("&lt;b&gt;x")), ["<b>x"]);
    }

    #[test]
    fn voice_and_language_apply_to_inner_spans_only() {
        let spans = parse("a<v Bob Smith>b<lang en-US>c</lang>d</v>e");
        assert_eq!(texts(&spans), ["a", "b", "c", "d", "e"]);
        assert_eq!(spans[0].voice, None);
        assert_eq!(spans[1].voice.as_deref(), Some("Bob Smith"));
        assert_eq!(spans[2].voice.as_deref(), Some("Bob Smith"));
        assert_eq!(spans[2].language.as_deref(), Some("en-US"));
        assert_eq!(spans[3].language, None);
        assert_eq!(spans[4].voice, None);
    }

    #[test]
    fn ruby_annotation_is_attached_to_base_and_removed_from_text() {
        let spans = parse("<ruby>漢字<rt>kanji</rt></ruby>!");
        assert_eq!(texts(&spans), ["漢字", "!"]);
        assert_eq!(spans[0].ruby_annotation.as_deref(), Some("kanji"));
        assert_eq!(spans[1].ruby_annotation, None);
    }

    #[test]
    fn several_ruby_pairs_each_get_their_own_annotation() {
        let spans = parse("<ruby>漢<rt>kan</rt>字<rt>ji</rt></ruby>");
        assert_eq!(texts(&spans), ["漢", "字"]);
        assert_eq!(spans[0].ruby_annotation.as_deref(), Some("kan"));
        assert_eq!(spans[1].ruby_annotation.as_deref(), Some("ji"));
    }

    #[test]
    fn nested_ruby_and_unclosed_rt_do_not_panic_or_lose_base_text() {
        let spans = parse("<ruby>A<ruby>B<rt>b</rt></ruby><rt>a</rt></ruby>");
        assert_eq!(texts(&spans), ["A", "B"]);
        assert_eq!(spans[0].ruby_annotation.as_deref(), Some("a"));
        assert_eq!(spans[1].ruby_annotation.as_deref(), Some("b"));
        let unclosed = parse("<ruby>X<rt>x");
        assert_eq!(texts(&unclosed), ["X"]);
        assert_eq!(unclosed[0].ruby_annotation.as_deref(), Some("x"));
    }

    #[test]
    fn rt_outside_ruby_is_stripped_and_its_text_stays_normal() {
        let spans = parse("a<rt>b</rt>c");
        assert_eq!(texts(&spans), ["abc"]);
        assert!(spans.iter().all(|span| span.ruby_annotation.is_none()));
    }

    #[test]
    fn timestamp_tags_set_reveal_for_following_spans_until_next_one() {
        let spans = parse("a<00:02.000>b<c>c</c><00:03.500>d");
        assert_eq!(texts(&spans), ["a", "b", "c", "d"]);
        assert_eq!(spans[0].reveal_at, None);
        assert_eq!(spans[1].reveal_at, Some(SubtitleTime::from_millis(2_000)));
        assert_eq!(spans[2].reveal_at, Some(SubtitleTime::from_millis(2_000)));
        assert_eq!(spans[3].reveal_at, Some(SubtitleTime::from_millis(3_500)));
    }

    #[test]
    fn timestamp_outside_cue_window_or_malformed_is_ignored() {
        // Раньше или ровно на начале, на конце и позже, и битая запись.
        let spans = parse("a<00:00:00.500>b<00:01.000>c<00:05.000>d<00:09.000>e<00:02,000>f");
        assert_eq!(texts(&spans), ["abcdef".to_owned()]);
        assert_eq!(spans[0].reveal_at, None);
    }

    #[test]
    fn line_break_after_timestamp_is_also_delayed() {
        let spans = parse("a<00:02.000>\nb");
        assert_eq!(spans[1].content, SpanContent::LineBreak);
        assert_eq!(spans[1].reveal_at, Some(SubtitleTime::from_millis(2_000)));
    }

    #[test]
    fn base_cue_style_applies_to_all_spans_and_class_style_only_inside_class() {
        let sheet = sheet_from_css(
            "::cue { color: white } ::cue(.red) { color: red; background-color: black }",
        );
        let spans = parse_with("a<c.red>b</c>c", &sheet);
        let white = Some(SubtitleColor::opaque(255, 255, 255));
        assert_eq!(texts(&spans), ["a", "b", "c"]);
        assert_eq!(spans[0].style.color, white);
        assert_eq!(spans[1].style.color, Some(SubtitleColor::opaque(255, 0, 0)));
        assert_eq!(
            spans[1].style.background,
            Some(SubtitleColor::opaque(0, 0, 0))
        );
        assert_eq!(spans[2].style.color, white);
        assert_eq!(spans[2].style.background, None);
    }

    #[test]
    fn classes_on_any_tag_and_multiple_classes_are_styled() {
        let sheet = sheet_from_css("::cue(.a){color:red} ::cue(.b){font-style:italic}");
        let spans = parse_with("<b.a.b>x</b>", &sheet);
        assert_eq!(spans[0].style.color, Some(SubtitleColor::opaque(255, 0, 0)));
        assert_eq!(spans[0].style.slant, FontSlant::Italic);
        assert_eq!(spans[0].style.weight, FontWeight::Bold);
    }

    #[test]
    fn voice_selector_styles_only_matching_voice() {
        let sheet = sheet_from_css("::cue(v[voice=\"Bob\"]) { color: lime }");
        let spans = parse_with("<v Bob>a</v><v Eve>b</v>", &sheet);
        assert_eq!(spans[0].style.color, Some(SubtitleColor::opaque(0, 255, 0)));
        assert_eq!(spans[1].style.color, None);
    }

    #[test]
    fn inline_tag_overrides_class_style_and_inner_class_overrides_outer() {
        let sheet = sheet_from_css(
            "::cue(.plain){font-weight:normal;font-style:normal} ::cue(.o){color:red} ::cue(.i){color:blue}",
        );
        let spans = parse_with("<b.plain>x</b><c.o>y<c.i>z</c></c>", &sheet);
        assert_eq!(spans[0].style.weight, FontWeight::Bold);
        assert_eq!(spans[1].style.color, Some(SubtitleColor::opaque(255, 0, 0)));
        assert_eq!(spans[2].style.color, Some(SubtitleColor::opaque(0, 0, 255)));
    }
}
