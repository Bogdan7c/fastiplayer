//! Стили TTML (подмножество IMSC Text Profile): набор свойств, ссылки `style` и наследование.
//!
//! Порядок приоритета (от слабого к сильному): регион -> `body` -> `div` -> `p` -> `span`;
//! в пределах одного элемента ссылки `style` применяются слева направо, затем атрибуты
//! `tts:*` самого элемента. Свойства, которых нет в нашей модели, считаются и не мешают.

use std::collections::{HashMap, HashSet};

use subtitle_core::{
    CueTextAlign, CueWritingDirection, FontSlant, FontWeight, SpanStyle, SubtitleColor,
    TextStrikethrough, TextUnderline,
};

use super::color::parse_color;
use super::model::{ElementKind, RawElement, RawTree, StyleAttribute};

/// Предел цепочки ссылок `style` на `style`: защита от рекурсии и взрыва ссылок.
const MAX_STYLE_REFERENCE_DEPTH: usize = 16;

/// Вертикальное выравнивание блока внутри региона (`tts:displayAlign`).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) enum DisplayAlign {
    Before,
    Center,
    After,
}

/// Набор заданных свойств. `None` — свойство не задано (наследуется или по умолчанию).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub(super) struct StyleSet {
    pub(super) color: Option<SubtitleColor>,
    /// Цвет фона; полностью прозрачный хранится как есть (он перекрывает фон предка).
    pub(super) background_color: Option<SubtitleColor>,
    pub(super) slant: Option<FontSlant>,
    pub(super) weight: Option<FontWeight>,
    pub(super) underline: Option<TextUnderline>,
    pub(super) strikethrough: Option<TextStrikethrough>,
    pub(super) text_align: Option<CueTextAlign>,
    pub(super) display_align: Option<DisplayAlign>,
    pub(super) writing_direction: Option<CueWritingDirection>,
}

impl StyleSet {
    /// Свойства `self` поверх `base`: заданное в `self` побеждает.
    pub(super) fn over(self, base: Self) -> Self {
        Self {
            color: self.color.or(base.color),
            background_color: self.background_color.or(base.background_color),
            slant: self.slant.or(base.slant),
            weight: self.weight.or(base.weight),
            underline: self.underline.or(base.underline),
            strikethrough: self.strikethrough.or(base.strikethrough),
            text_align: self.text_align.or(base.text_align),
            display_align: self.display_align.or(base.display_align),
            writing_direction: self.writing_direction.or(base.writing_direction),
        }
    }

    /// Без фона: фон `region`/`body`/`div` рисует площадь контейнера, а не строки текста, и
    /// в модели субтитров его нет — на спаны он не переносится.
    pub(super) fn without_background(self) -> Self {
        Self {
            background_color: None,
            ..self
        }
    }

    /// Стиль спана модели; прозрачный фон = «фона нет».
    pub(super) fn to_span_style(self) -> SpanStyle {
        SpanStyle {
            weight: self.weight.unwrap_or_default(),
            slant: self.slant.unwrap_or_default(),
            underline: self.underline.unwrap_or_default(),
            strikethrough: self.strikethrough.unwrap_or_default(),
            color: self.color,
            background: self.background_color.filter(|color| color.alpha != 0),
        }
    }

    /// Применяет атрибуты `tts:*`; возвращает, сколько из них проигнорировано.
    pub(super) fn apply_attributes(&mut self, attributes: &[StyleAttribute]) -> usize {
        attributes
            .iter()
            .filter(|attribute| !self.apply(&attribute.name, attribute.value.trim()))
            .count()
    }

    /// Применяет один атрибут; `false` — не поддержан или значение некорректно.
    fn apply(&mut self, name: &str, value: &str) -> bool {
        match name {
            "color" => assign(&mut self.color, parse_color(value)),
            "backgroundColor" => assign(&mut self.background_color, parse_color(value)),
            "fontStyle" => assign(&mut self.slant, parse_font_style(value)),
            "fontWeight" => assign(&mut self.weight, parse_font_weight(value)),
            "textDecoration" => self.apply_text_decoration(value),
            "textAlign" => assign(&mut self.text_align, parse_text_align(value)),
            "displayAlign" => assign(&mut self.display_align, parse_display_align(value)),
            "writingMode" => assign(&mut self.writing_direction, parse_writing_mode(value)),
            // Направление письма определяет растеризатор по самому тексту (bidi): принимаем.
            "direction" => true,
            // Геометрия региона читается в модуле `layout`; здесь она не стиль текста.
            "origin" | "extent" => true,
            _ => false,
        }
    }

    /// `tts:textDecoration`: токены `underline`, `lineThrough`, `noUnderline`, `noLineThrough`, `none`.
    fn apply_text_decoration(&mut self, value: &str) -> bool {
        let mut all_known = true;
        for token in value.split_whitespace() {
            match token {
                "none" => {
                    self.underline = Some(TextUnderline::None);
                    self.strikethrough = Some(TextStrikethrough::None);
                }
                "underline" => self.underline = Some(TextUnderline::Single),
                "noUnderline" => self.underline = Some(TextUnderline::None),
                "lineThrough" => self.strikethrough = Some(TextStrikethrough::Single),
                "noLineThrough" => self.strikethrough = Some(TextStrikethrough::None),
                // overline, blink и т.п. в модели нет.
                _ => all_known = false,
            }
        }
        all_known
    }
}

/// Записывает разобранное значение; `false`, если разобрать не удалось.
fn assign<T>(slot: &mut Option<T>, parsed: Option<T>) -> bool {
    match parsed {
        Some(value) => {
            *slot = Some(value);
            true
        }
        None => false,
    }
}

fn parse_font_style(value: &str) -> Option<FontSlant> {
    match value {
        "normal" => Some(FontSlant::Normal),
        "italic" | "oblique" => Some(FontSlant::Italic),
        _ => None,
    }
}

fn parse_font_weight(value: &str) -> Option<FontWeight> {
    match value {
        "normal" => Some(FontWeight::Normal),
        "bold" => Some(FontWeight::Bold),
        _ => None,
    }
}

fn parse_text_align(value: &str) -> Option<CueTextAlign> {
    match value {
        "left" => Some(CueTextAlign::Left),
        "center" => Some(CueTextAlign::Center),
        "right" => Some(CueTextAlign::Right),
        "start" => Some(CueTextAlign::Start),
        "end" => Some(CueTextAlign::End),
        _ => None,
    }
}

fn parse_display_align(value: &str) -> Option<DisplayAlign> {
    match value {
        "before" => Some(DisplayAlign::Before),
        "center" => Some(DisplayAlign::Center),
        "after" => Some(DisplayAlign::After),
        _ => None,
    }
}

fn parse_writing_mode(value: &str) -> Option<CueWritingDirection> {
    match value {
        "lrtb" | "lr" | "rltb" | "rl" => Some(CueWritingDirection::Horizontal),
        "tbrl" | "tb" => Some(CueWritingDirection::VerticalRightToLeft),
        "tblr" => Some(CueWritingDirection::VerticalLeftToRight),
        _ => None,
    }
}

/// Определения стилей документа: `xml:id` -> набор свойств (со всеми ссылками раскрытыми).
#[derive(Debug, Default)]
pub(super) struct StyleBook {
    resolved: HashMap<String, StyleSet>,
    /// Сколько свойств в определениях проигнорировано.
    ignored_count: usize,
}

impl StyleBook {
    /// Собирает книгу из всех `<style>` внутри `<head>/<styling>`.
    pub(super) fn from_tree(tree: &RawTree) -> Self {
        let mut definitions: HashMap<&str, &RawElement> = HashMap::new();
        for element in &tree.elements {
            if element.kind == ElementKind::Style
                && let Some(id) = element.id.as_deref()
            {
                // При повторе id действует первое определение.
                definitions.entry(id).or_insert(element);
            }
        }
        let mut book = Self::default();
        for (id, element) in &definitions {
            // Игнорируемые свойства считаем один раз на определение.
            book.ignored_count += StyleSet::default().apply_attributes(&element.style_attributes);
            let mut visiting = HashSet::new();
            resolve_definition(id, &definitions, &mut book.resolved, &mut visiting, 0);
        }
        book
    }

    /// Сколько атрибутов в определениях стилей не поддержано.
    pub(super) fn ignored_count(&self) -> usize {
        self.ignored_count
    }

    /// Свойства элемента: ссылки `style` слева направо, затем собственные атрибуты `tts:*`.
    /// Второе значение — сколько собственных атрибутов проигнорировано.
    pub(super) fn specified_for(&self, element: &RawElement) -> (StyleSet, usize) {
        let mut set = StyleSet::default();
        for reference in &element.style_refs {
            if let Some(referenced) = self.resolved.get(reference) {
                set = (*referenced).over(set);
            }
        }
        let mut own = StyleSet::default();
        let ignored = own.apply_attributes(&element.style_attributes);
        (own.over(set), ignored)
    }
}

/// Раскрывает определение стиля: сначала его ссылки (слева направо), затем свои атрибуты.
fn resolve_definition(
    id: &str,
    definitions: &HashMap<&str, &RawElement>,
    resolved: &mut HashMap<String, StyleSet>,
    visiting: &mut HashSet<String>,
    depth: usize,
) -> StyleSet {
    if let Some(done) = resolved.get(id) {
        return *done;
    }
    let Some(element) = definitions.get(id) else {
        return StyleSet::default();
    };
    // Цикл или слишком длинная цепочка: ссылка игнорируется.
    if depth >= MAX_STYLE_REFERENCE_DEPTH || !visiting.insert(id.to_owned()) {
        return StyleSet::default();
    }
    let mut set = StyleSet::default();
    for reference in &element.style_refs {
        let referenced = resolve_definition(reference, definitions, resolved, visiting, depth + 1);
        set = referenced.over(set);
    }
    let mut own = StyleSet::default();
    own.apply_attributes(&element.style_attributes);
    let result = own.over(set);
    visiting.remove(id);
    resolved.insert(id.to_owned(), result);
    result
}

#[cfg(test)]
mod tests {
    use super::*;

    fn attribute(name: &str, value: &str) -> StyleAttribute {
        StyleAttribute {
            name: name.to_owned(),
            value: value.to_owned(),
        }
    }

    #[test]
    fn attributes_fill_the_matching_properties() {
        let mut set = StyleSet::default();
        let ignored = set.apply_attributes(&[
            attribute("color", "#ff0000"),
            attribute("fontWeight", "bold"),
            attribute("fontStyle", "italic"),
            attribute("textDecoration", "underline lineThrough"),
            attribute("textAlign", "end"),
            attribute("displayAlign", "after"),
        ]);
        assert_eq!(ignored, 0);
        assert_eq!(set.color, Some(SubtitleColor::opaque(255, 0, 0)));
        assert_eq!(set.weight, Some(FontWeight::Bold));
        assert_eq!(set.slant, Some(FontSlant::Italic));
        assert_eq!(set.underline, Some(TextUnderline::Single));
        assert_eq!(set.strikethrough, Some(TextStrikethrough::Single));
        assert_eq!(set.text_align, Some(CueTextAlign::End));
        assert_eq!(set.display_align, Some(DisplayAlign::After));
    }

    #[test]
    fn unsupported_and_invalid_attributes_are_counted_not_applied() {
        let mut set = StyleSet::default();
        let ignored = set.apply_attributes(&[
            attribute("fontFamily", "Arial"),
            attribute("color", "nonsense"),
            attribute("textDecoration", "overline"),
            attribute("textOutline", "black 2px"),
        ]);
        assert_eq!(ignored, 4);
        assert_eq!(set, StyleSet::default());
    }

    #[test]
    fn over_prefers_the_top_layer_and_keeps_the_rest() {
        let base = StyleSet {
            color: Some(SubtitleColor::opaque(1, 1, 1)),
            weight: Some(FontWeight::Bold),
            ..StyleSet::default()
        };
        let top = StyleSet {
            color: Some(SubtitleColor::opaque(2, 2, 2)),
            ..StyleSet::default()
        };
        let merged = top.over(base);
        assert_eq!(merged.color, Some(SubtitleColor::opaque(2, 2, 2)));
        assert_eq!(merged.weight, Some(FontWeight::Bold));
    }

    #[test]
    fn transparent_background_means_no_background_in_span_style() {
        let set = StyleSet {
            background_color: Some(SubtitleColor::TRANSPARENT),
            ..StyleSet::default()
        };
        assert_eq!(set.to_span_style().background, None);
    }
}
