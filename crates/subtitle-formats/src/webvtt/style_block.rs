//! Блок `STYLE`: маленький устойчивый разбор CSS для подмножества `::cue`.
//!
//! Поддерживается: комментарии `/* */`, несколько селекторов через запятую, несколько
//! объявлений, `!important` (допускается и отбрасывается). Всё остальное (неизвестные
//! селекторы и свойства, at-правила) пропускается и увеличивает счётчик
//! `ignored_style_rules`. CSS никогда не делает файл невалидным.
//!
//! Правило подсчёта: каждый неподдерживаемый селектор и каждое неподдерживаемое объявление
//! (в правиле, у которого есть хоть один поддерживаемый селектор) считаются по одному разу;
//! правило без единого поддерживаемого селектора считается один раз за селектор.

use super::css_value::{
    parse_css_color, parse_font_style, parse_font_weight, parse_text_decoration,
};
use super::style_sheet::{CueSelector, StyleDeclarations, StyleRule};

/// Результат разбора одного блока `STYLE`.
#[derive(Debug, Default, PartialEq, Eq)]
pub(super) struct ParsedStyleBlock {
    pub(super) rules: Vec<StyleRule>,
    pub(super) ignored_rule_count: usize,
}

/// Начало и конец комментария CSS.
const COMMENT_START: &str = "/*";
const COMMENT_END: &str = "*/";

/// Разбирает тело блока (строки после слова `STYLE`, склеенные через `\\n`).
pub(super) fn parse_style_block(css: &str) -> ParsedStyleBlock {
    // WebVTT запрещает `-->` внутри блока: такой блок целиком считается мусором.
    if css.contains("-->") {
        return ParsedStyleBlock {
            rules: Vec::new(),
            ignored_rule_count: 1,
        };
    }
    let cleaned = strip_comments(css);
    let mut parsed = ParsedStyleBlock::default();
    let mut rest = cleaned.as_str();
    while let Some(open_index) = rest.find('{') {
        let (prelude, after_open) = rest.split_at(open_index);
        let (body, remainder) = split_balanced_body(after_open.get(1..).unwrap_or_default());
        rest = remainder;
        // Операторы вроде `@import …;` перед правилом отбрасываем как проигнорированные.
        let selector_text = match prelude.rfind(';') {
            Some(semicolon) => {
                parsed.ignored_rule_count += 1;
                prelude.get(semicolon + 1..).unwrap_or_default()
            }
            None => prelude,
        };
        parse_rule(selector_text, body, &mut parsed);
    }
    // Хвост без `{` (например, оборванный селектор) — одно проигнорированное правило.
    if !rest.trim().is_empty() {
        parsed.ignored_rule_count += 1;
    }
    parsed
}

/// Удаляет комментарии; незакрытый комментарий съедает остаток текста.
fn strip_comments(css: &str) -> String {
    let mut output = String::with_capacity(css.len());
    let mut rest = css;
    while let Some(start) = rest.find(COMMENT_START) {
        output.push_str(rest.get(..start).unwrap_or_default());
        let after_start = rest.get(start + COMMENT_START.len()..).unwrap_or_default();
        match after_start.find(COMMENT_END) {
            Some(end) => {
                rest = after_start
                    .get(end + COMMENT_END.len()..)
                    .unwrap_or_default()
            }
            None => return output,
        }
    }
    output.push_str(rest);
    output
}

/// Делит текст после `{` на тело правила и остаток после парной `}` (учитывает вложенные скобки).
/// Если парной `}` нет, телом считается весь остаток.
fn split_balanced_body(after_open: &str) -> (&str, &str) {
    let mut depth = 0_usize;
    for (index, ch) in after_open.char_indices() {
        match ch {
            '{' => depth += 1,
            '}' if depth == 0 => {
                let body = after_open.get(..index).unwrap_or_default();
                let remainder = after_open.get(index + 1..).unwrap_or_default();
                return (body, remainder);
            }
            '}' => depth -= 1,
            _ => {}
        }
    }
    (after_open, "")
}

fn parse_rule(selector_text: &str, body: &str, parsed: &mut ParsedStyleBlock) {
    let selectors: Vec<Option<CueSelector>> =
        selector_text.split(',').map(parse_selector).collect();
    let supported: Vec<&CueSelector> = selectors.iter().flatten().collect();
    parsed.ignored_rule_count += selectors.len() - supported.len();
    if supported.is_empty() {
        return;
    }
    let (declarations, ignored_declarations) = parse_declarations(body);
    parsed.ignored_rule_count += ignored_declarations;
    for selector in supported {
        parsed.rules.push(StyleRule {
            selector: selector.clone(),
            declarations,
        });
    }
}

/// `::cue`, `::cue(.класс)`, `::cue(v[voice="Имя"])`; всё остальное — `None`.
fn parse_selector(text: &str) -> Option<CueSelector> {
    let text = text.trim();
    if text == "::cue" {
        return Some(CueSelector::Base);
    }
    let inner = text.strip_prefix("::cue(")?.strip_suffix(')')?.trim();
    if let Some(class) = inner.strip_prefix('.') {
        return is_simple_identifier(class).then(|| CueSelector::Class(class.to_owned()));
    }
    let attribute = inner.strip_prefix("v[voice=")?.strip_suffix(']')?.trim();
    let voice = strip_matching_quotes(attribute);
    (!voice.is_empty()).then(|| CueSelector::Voice(voice.to_owned()))
}

/// Идентификатор класса без экранирования: буквы, цифры, `-`, `_`.
fn is_simple_identifier(text: &str) -> bool {
    !text.is_empty()
        && text
            .chars()
            .all(|ch| ch.is_alphanumeric() || ch == '-' || ch == '_')
}

fn strip_matching_quotes(text: &str) -> &str {
    for quote in ['"', '\''] {
        if let Some(unquoted) = text
            .strip_prefix(quote)
            .and_then(|rest| rest.strip_suffix(quote))
        {
            return unquoted;
        }
    }
    text
}

/// Объявления тела: (накопленные свойства, число неподдерживаемых). Позднее объявление
/// перекрывает раннее.
fn parse_declarations(body: &str) -> (StyleDeclarations, usize) {
    let mut declarations = StyleDeclarations::default();
    let mut ignored = 0;
    for declaration in body.split(';').map(str::trim).filter(|d| !d.is_empty()) {
        let Some((property, value)) = declaration.split_once(':') else {
            ignored += 1;
            continue;
        };
        let value = strip_important(value);
        if !apply_declaration(
            &mut declarations,
            &property.trim().to_ascii_lowercase(),
            value,
        ) {
            ignored += 1;
        }
    }
    (declarations, ignored)
}

/// Отбрасывает суффикс `!important` (приоритет в нашей модели не различается).
fn strip_important(value: &str) -> &str {
    let value = value.trim();
    match value.rfind('!') {
        Some(bang)
            if value
                .get(bang + 1..)
                .is_some_and(|word| word.trim().eq_ignore_ascii_case("important")) =>
        {
            value.get(..bang).unwrap_or_default().trim()
        }
        _ => value,
    }
}

/// Применяет объявление; `false` — свойство или значение не поддерживается.
fn apply_declaration(declarations: &mut StyleDeclarations, property: &str, value: &str) -> bool {
    match property {
        "color" => assign(&mut declarations.color, parse_css_color(value)),
        // Сокращение `background` поддерживается только когда это просто цвет.
        "background-color" | "background" => {
            assign(&mut declarations.background, parse_css_color(value))
        }
        "font-weight" => assign(&mut declarations.weight, parse_font_weight(value)),
        "font-style" => assign(&mut declarations.slant, parse_font_style(value)),
        "text-decoration" | "text-decoration-line" => match parse_text_decoration(value) {
            Some(decoration) => {
                if decoration.underline.is_some() {
                    declarations.underline = decoration.underline;
                }
                if decoration.strikethrough.is_some() {
                    declarations.strikethrough = decoration.strikethrough;
                }
                true
            }
            None => false,
        },
        _ => false,
    }
}

/// Записывает значение, если оно распознано; возвращает признак «поддержано».
fn assign<T>(slot: &mut Option<T>, parsed: Option<T>) -> bool {
    match parsed {
        Some(value) => {
            *slot = Some(value);
            true
        }
        None => false,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use subtitle_core::{FontSlant, FontWeight, SubtitleColor, TextStrikethrough, TextUnderline};

    fn class_rule(parsed: &ParsedStyleBlock, class: &str) -> StyleDeclarations {
        parsed
            .rules
            .iter()
            .find(|rule| rule.selector == CueSelector::Class(class.to_owned()))
            .unwrap()
            .declarations
    }

    #[test]
    fn base_class_and_voice_selectors_with_all_properties() {
        let parsed = parse_style_block(
            "::cue { color: #fff; background-color: rgba(0,0,0,.5) }\n\
             ::cue(.loud) { font-weight: bold; font-style: italic; text-decoration: underline line-through }\n\
             ::cue(v[voice=\"Bob\"]) { color: lime }",
        );
        assert_eq!(parsed.ignored_rule_count, 0);
        assert_eq!(parsed.rules.len(), 3);
        assert_eq!(parsed.rules[0].selector, CueSelector::Base);
        assert_eq!(
            parsed.rules[0].declarations.color,
            Some(SubtitleColor::opaque(255, 255, 255))
        );
        let loud = class_rule(&parsed, "loud");
        assert_eq!(loud.weight, Some(FontWeight::Bold));
        assert_eq!(loud.slant, Some(FontSlant::Italic));
        assert_eq!(loud.underline, Some(TextUnderline::Single));
        assert_eq!(loud.strikethrough, Some(TextStrikethrough::Single));
        assert_eq!(
            parsed.rules[2].selector,
            CueSelector::Voice("Bob".to_owned())
        );
    }

    #[test]
    fn comments_important_and_selector_lists_are_tolerated() {
        let parsed = parse_style_block(
            "/* header */ ::cue(.a), ::cue(.b) { color: red !IMPORTANT; /* mid */ font-weight: 700 }",
        );
        assert_eq!(parsed.ignored_rule_count, 0);
        assert_eq!(parsed.rules.len(), 2);
        for class in ["a", "b"] {
            let declarations = class_rule(&parsed, class);
            assert_eq!(declarations.color, Some(SubtitleColor::opaque(255, 0, 0)));
            assert_eq!(declarations.weight, Some(FontWeight::Bold));
        }
    }

    #[test]
    fn unsupported_selectors_and_properties_are_counted_not_fatal() {
        let parsed = parse_style_block(
            "::cue(b) { color: red }\n\
             ::cue-region { color: red }\n\
             ::cue(.ok) { color: red; font-size: 2em; text-shadow: 1px 1px; color: nonsense }",
        );
        // два неподдерживаемых селектора + три неподдерживаемых объявления
        assert_eq!(parsed.ignored_rule_count, 5);
        assert_eq!(parsed.rules.len(), 1);
        assert_eq!(
            class_rule(&parsed, "ok").color,
            Some(SubtitleColor::opaque(255, 0, 0))
        );
    }

    #[test]
    fn later_declaration_wins_inside_rule() {
        let parsed = parse_style_block("::cue(.x) { color: red; color: blue }");
        assert_eq!(
            class_rule(&parsed, "x").color,
            Some(SubtitleColor::opaque(0, 0, 255))
        );
    }

    #[test]
    fn broken_css_is_survived() {
        // Незакрытый комментарий съедает остаток; но предыдущее правило сохраняется.
        let parsed =
            parse_style_block("::cue(.a){color:red} /* never closed ::cue(.b){color:blue}");
        assert_eq!(parsed.rules.len(), 1);
        assert_eq!(parsed.ignored_rule_count, 0);
        // Правило без закрывающей скобки читается до конца.
        let parsed = parse_style_block("::cue(.a){color:red");
        assert_eq!(
            class_rule(&parsed, "a").color,
            Some(SubtitleColor::opaque(255, 0, 0))
        );
        // Оборванный селектор и вложенные at-правила считаются, но не ломают разбор.
        let parsed = parse_style_block(
            "@media screen { ::cue(.n){color:red} } ::cue(.after){color:lime} dangling",
        );
        assert_eq!(parsed.rules.len(), 1);
        assert_eq!(parsed.ignored_rule_count, 2);
        assert_eq!(parse_style_block("").rules.len(), 0);
        assert_eq!(parse_style_block("}}{{").rules.len(), 0);
    }

    #[test]
    fn arrow_inside_style_block_discards_it_whole() {
        let parsed = parse_style_block("::cue { color: red } --> ::cue(.a) { color: blue }");
        assert!(parsed.rules.is_empty());
        assert_eq!(parsed.ignored_rule_count, 1);
    }
}
