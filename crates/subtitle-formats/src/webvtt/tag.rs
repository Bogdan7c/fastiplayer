//! Лексер угловых тегов текста реплики: `<c.red.big>`, `</i>`, `<v Имя>`, `<00:01.500>`.
//!
//! Отличие от токенизатора W3C: если `<` не начинает похожий на тег фрагмент с закрывающей
//! `>` в той же строке (`a < b`, `<b` в конце текста), `<` остаётся буквальным текстом.
//! Браузеры в таких случаях строят тег из хвоста текста; для субтитров безопаснее не
//! терять видимый текст.

use subtitle_core::SubtitleTime;

use super::timestamp::parse_webvtt_timestamp;

/// Вид элемента разметки WebVTT.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) enum ElementKind {
    Class,
    Italic,
    Bold,
    Underline,
    Voice,
    Language,
    Ruby,
    RubyText,
}

impl ElementKind {
    /// Имя тега -> вид. Регистр значим (как в W3C): `<B>` — неизвестный тег.
    fn from_tag_name(name: &str) -> Option<Self> {
        Some(match name {
            "c" => Self::Class,
            "i" => Self::Italic,
            "b" => Self::Bold,
            "u" => Self::Underline,
            "v" => Self::Voice,
            "lang" => Self::Language,
            "ruby" => Self::Ruby,
            "rt" => Self::RubyText,
            _ => return None,
        })
    }
}

/// Разобранный тег-элемент.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(super) struct ElementTag {
    /// `None` — неизвестное имя: тег вырезается, содержимое остаётся.
    pub(super) kind: Option<ElementKind>,
    pub(super) classes: Vec<String>,
    /// Текст после имени и классов (имя говорящего, язык), пробелы схлопнуты.
    pub(super) annotation: String,
}

/// Что представляет собой тег.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(super) enum TagContent {
    OpeningElement(ElementTag),
    ClosingElement(ElementTag),
    /// Караоке-метка; `None` — время записано некорректно (метка вырезается).
    Timestamp(Option<SubtitleTime>),
}

/// Найденный в начале текста тег и сколько байтов он занимает.
#[derive(Debug, PartialEq, Eq)]
pub(super) struct ScannedTag {
    pub(super) content: TagContent,
    pub(super) consumed_bytes: usize,
}

/// Пытается прочитать тег в начале `rest` (там стоит `<`). `None` — `<` обычный символ.
pub(super) fn scan_angle_tag(rest: &str) -> Option<ScannedTag> {
    // Ближайший из `<`/`>`/`\n` после открывающего `<` одним проходом. Внутри тега не бывает ни
    // новой строки, ни ещё одного `<`: если первым встретился не `>`, это был просто символ.
    // Один проход (а не поиск `>` по всему остатку) держит разбор линейным на строке из `<`.
    let after_open = rest.get(1..)?;
    let relative_close = after_open.find(['<', '>', '\n'])?;
    if after_open.as_bytes().get(relative_close) != Some(&b'>') {
        return None;
    }
    let close_index = relative_close + 1;
    let inner = rest.get(1..close_index)?;
    let consumed_bytes = close_index + 1;
    let content = if let Some(closing) = inner.strip_prefix('/') {
        starts_with_ascii_letter(closing)
            .then(|| TagContent::ClosingElement(parse_element(closing)))?
    } else if starts_with_ascii_letter(inner) {
        TagContent::OpeningElement(parse_element(inner))
    } else if inner.starts_with(|ch: char| ch.is_ascii_digit()) {
        TagContent::Timestamp(parse_webvtt_timestamp(inner.trim()))
    } else {
        return None;
    };
    Some(ScannedTag {
        content,
        consumed_bytes,
    })
}

fn starts_with_ascii_letter(text: &str) -> bool {
    text.starts_with(|ch: char| ch.is_ascii_alphabetic())
}

/// `имя.класс1.класс2 аннотация` -> структура. Классы идут сразу за именем, без пробелов.
fn parse_element(body: &str) -> ElementTag {
    let name_end = body
        .find(|ch: char| ch.is_whitespace() || ch == '.')
        .unwrap_or(body.len());
    let (name, after_name) = body.split_at(name_end);
    let classes_end = after_name
        .find(char::is_whitespace)
        .unwrap_or(after_name.len());
    let (class_part, annotation_part) = after_name.split_at(classes_end);
    ElementTag {
        kind: ElementKind::from_tag_name(name),
        classes: class_part
            .split('.')
            .filter(|class| !class.is_empty())
            .map(str::to_owned)
            .collect(),
        annotation: annotation_part
            .split_whitespace()
            .collect::<Vec<_>>()
            .join(" "),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn opening(rest: &str) -> ElementTag {
        match scan_angle_tag(rest).unwrap().content {
            TagContent::OpeningElement(tag) => tag,
            other => panic!("expected opening tag, got {other:?}"),
        }
    }

    #[test]
    fn class_tag_collects_classes_and_consumed_length() {
        let scanned = scan_angle_tag("<c.red.big>text").unwrap();
        assert_eq!(scanned.consumed_bytes, "<c.red.big>".len());
        let TagContent::OpeningElement(tag) = scanned.content else {
            panic!("expected opening tag");
        };
        assert_eq!(tag.kind, Some(ElementKind::Class));
        assert_eq!(tag.classes, ["red", "big"]);
        assert_eq!(tag.annotation, "");
    }

    #[test]
    fn voice_tag_keeps_classes_and_collapses_annotation_whitespace() {
        let tag = opening("<v.loud   Bob  Smith >");
        assert_eq!(tag.kind, Some(ElementKind::Voice));
        assert_eq!(tag.classes, ["loud"]);
        assert_eq!(tag.annotation, "Bob Smith");
    }

    #[test]
    fn closing_tag_and_unknown_tag_names() {
        let scanned = scan_angle_tag("</i>").unwrap();
        assert!(matches!(
            scanned.content,
            TagContent::ClosingElement(ElementTag {
                kind: Some(ElementKind::Italic),
                ..
            })
        ));
        assert_eq!(opening("<B>").kind, None);
        assert_eq!(opening("<font color=red>").kind, None);
    }

    #[test]
    fn timestamp_tags_valid_and_invalid() {
        let valid = scan_angle_tag("<00:01.500>").unwrap();
        assert_eq!(
            valid.content,
            TagContent::Timestamp(Some(SubtitleTime::from_millis(1_500)))
        );
        let invalid = scan_angle_tag("<00:01,500>").unwrap();
        assert_eq!(invalid.content, TagContent::Timestamp(None));
    }

    #[test]
    fn lone_angle_brackets_are_plain_text() {
        for literal in [
            "< b>", "<", "<b", "<3 you", "<>", "</>", "</ i>", "<b <i>", "<b\n>", "<-->",
        ] {
            assert_eq!(scan_angle_tag(literal), None, "{literal:?}");
        }
    }
}
