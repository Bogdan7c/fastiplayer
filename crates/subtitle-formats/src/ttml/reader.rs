//! Чтение XML TTML через `bounded-xml-reader` -> [`RawTree`].
//!
//! Здесь только структура файла. Бюджеты XML выводятся из `SubtitleParseLimits`, как в srv3:
//! токенов, атрибутов и текста не может быть больше, чем байтов во входе, поэтому честные
//! файлы не отсекаются, а враждебные получают конечную границу.

use bounded_xml_reader::{BoundedXmlReader, XmlBudgets, XmlElement, XmlEvent, XmlReadError};

use super::model::{ElementKind, RawChild, RawElement, RawTree, SpaceHandling, StyleAttribute};
use crate::error::SubtitleLoadError;
use crate::limits::SubtitleParseLimits;

/// Пространство имён элементов TTML.
pub(super) const TTML_NAMESPACE: &str = "http://www.w3.org/ns/ttml";
/// Пространство имён атрибутов стилей (`tts:`).
const STYLING_NAMESPACE: &str = "http://www.w3.org/ns/ttml#styling";
/// Пространство имён параметров (`ttp:`).
const PARAMETER_NAMESPACE: &str = "http://www.w3.org/ns/ttml#parameter";
/// Пространство имён служебных атрибутов `xml:*`.
const XML_NAMESPACE: &str = "http://www.w3.org/XML/1998/namespace";

/// Допустимая вложенность: tt > body > div > div > p > span > span > ... с запасом на обёртки.
const MAX_XML_DEPTH: usize = 32;
/// Атрибутов на элементе: стили `tts:*` могут идти десятками.
const MAX_ATTRIBUTES_PER_ELEMENT: usize = 64;
/// Объявления пространств имён на одном элементе (tt обычно объявляет 5-8).
const MAX_NAMESPACE_DECLARATIONS_PER_ELEMENT: usize = 16;
/// Всего объявлений пространств имён на документ.
const MAX_NAMESPACE_DECLARATIONS: usize = 64;
/// Суммарный объём префиксов и URI пространств имён.
const MAX_NAMESPACE_BYTES: usize = 4096;

/// Читает TTML из UTF-8 текста.
pub(super) fn read_tree(
    text: &str,
    limits: &SubtitleParseLimits,
) -> Result<RawTree, SubtitleLoadError> {
    let budgets = xml_budgets(limits)?;
    let mut reader = BoundedXmlReader::new(text.as_bytes(), budgets).map_err(map_xml_error)?;
    let mut builder = TreeBuilder::default();
    while let Some(event) = reader.next_event().map_err(map_xml_error)? {
        builder.handle(event)?;
    }
    builder.finish()
}

fn xml_budgets(limits: &SubtitleParseLimits) -> Result<XmlBudgets, SubtitleLoadError> {
    let input_bytes = limits.max_input_bytes;
    XmlBudgets::builder()
        .maximum_document_bytes(input_bytes)
        .maximum_depth(MAX_XML_DEPTH)
        .maximum_tokens(input_bytes)
        .maximum_attributes_per_element(MAX_ATTRIBUTES_PER_ELEMENT)
        .maximum_attribute_count(input_bytes)
        .maximum_attribute_bytes(input_bytes)
        .maximum_namespace_declarations_per_element(MAX_NAMESPACE_DECLARATIONS_PER_ELEMENT)
        .maximum_namespace_declaration_count(MAX_NAMESPACE_DECLARATIONS)
        .maximum_namespace_bytes(MAX_NAMESPACE_BYTES)
        .maximum_text_bytes(input_bytes)
        .build()
        // Все поля заданы выше; ветка защищает от будущего нового обязательного бюджета.
        .map_err(|_| SubtitleLoadError::TtmlTooComplex)
}

/// Ошибки лимитов -> «слишком сложный файл», остальное -> «битый XML».
fn map_xml_error(error: XmlReadError) -> SubtitleLoadError {
    match error {
        XmlReadError::DocumentBytesExceeded { .. }
        | XmlReadError::DepthExceeded { .. }
        | XmlReadError::TokensExceeded { .. }
        | XmlReadError::AttributesPerElementExceeded { .. }
        | XmlReadError::AttributeCountExceeded { .. }
        | XmlReadError::AttributeBytesExceeded { .. }
        | XmlReadError::NamespaceDeclarationsPerElementExceeded { .. }
        | XmlReadError::NamespaceDeclarationCountExceeded { .. }
        | XmlReadError::NamespaceBytesExceeded { .. }
        | XmlReadError::TextBytesExceeded { .. } => SubtitleLoadError::TtmlTooComplex,
        _ => SubtitleLoadError::MalformedTtml,
    }
}

/// Накопитель дерева по потоку событий.
#[derive(Default)]
struct TreeBuilder {
    elements: Vec<RawElement>,
    /// Стек открытых элементов: `Some(индекс)` — элемент попал в дерево, `None` — он
    /// (или его предок) неизвестен и вместе с содержимым пропускается.
    stack: Vec<Option<usize>>,
    root: Option<usize>,
    unsupported_construct_count: usize,
}

impl TreeBuilder {
    fn handle(&mut self, event: XmlEvent) -> Result<(), SubtitleLoadError> {
        match event {
            XmlEvent::StartElement(element) => {
                let slot = self.enter(&element)?;
                self.stack.push(slot);
            }
            XmlEvent::EmptyElement(element) => {
                self.enter(&element)?;
            }
            XmlEvent::EndElement(_) => {
                self.stack.pop();
            }
            XmlEvent::Text(text) => self.append_text(text.content()),
        }
        Ok(())
    }

    /// Открывает элемент; возвращает его место в дереве (или `None` для пропускаемого).
    fn enter(&mut self, element: &XmlElement) -> Result<Option<usize>, SubtitleLoadError> {
        let kind = classify(element);
        let Some(parent_slot) = self.stack.last().copied() else {
            // Первый элемент документа: обязан быть `tt` из пространства имён TTML.
            if kind != Some(ElementKind::Tt) {
                return Err(SubtitleLoadError::NotTtml);
            }
            let index = self.push_element(ElementKind::Tt, element);
            self.root = Some(index);
            return Ok(Some(index));
        };
        // Внутри пропускаемого поддерева ничего не сохраняем.
        let Some(parent_index) = parent_slot else {
            return Ok(None);
        };
        let Some(kind) = kind else {
            self.count_unsupported_element(element);
            return Ok(None);
        };
        let index = self.push_element(kind, element);
        if let Some(parent) = self.elements.get_mut(parent_index) {
            parent.children.push(RawChild::Element(index));
        }
        Ok(Some(index))
    }

    fn push_element(&mut self, kind: ElementKind, element: &XmlElement) -> usize {
        self.elements.push(read_attributes(kind, element));
        self.elements.len() - 1
    }

    /// Анимация (`set`, `animate`) прочитана, но не поддерживается — считаем её.
    fn count_unsupported_element(&mut self, element: &XmlElement) {
        let name = element.name();
        if name.namespace_uri() == Some(TTML_NAMESPACE)
            && matches!(name.local_name(), "set" | "animate")
        {
            self.unsupported_construct_count += 1;
        }
    }

    /// Текст сохраняется только внутри известного элемента; соседние куски склеиваются.
    fn append_text(&mut self, text: &str) {
        let Some(Some(index)) = self.stack.last().copied() else {
            return;
        };
        let Some(parent) = self.elements.get_mut(index) else {
            return;
        };
        if let Some(RawChild::Text(existing)) = parent.children.last_mut() {
            existing.push_str(text);
        } else {
            parent.children.push(RawChild::Text(text.to_owned()));
        }
    }

    fn finish(self) -> Result<RawTree, SubtitleLoadError> {
        let root = self.root.ok_or(SubtitleLoadError::NotTtml)?;
        Ok(RawTree {
            elements: self.elements,
            root,
            unsupported_construct_count: self.unsupported_construct_count,
        })
    }
}

/// Вид элемента; `None` — элемент не TTML или неизвестен.
fn classify(element: &XmlElement) -> Option<ElementKind> {
    let name = element.name();
    if name.namespace_uri() != Some(TTML_NAMESPACE) {
        return None;
    }
    Some(match name.local_name() {
        "tt" => ElementKind::Tt,
        "head" => ElementKind::Head,
        "styling" => ElementKind::Styling,
        "style" => ElementKind::Style,
        "layout" => ElementKind::Layout,
        "region" => ElementKind::Region,
        "body" => ElementKind::Body,
        "div" => ElementKind::Div,
        "p" => ElementKind::Paragraph,
        "span" => ElementKind::Span,
        "br" => ElementKind::LineBreak,
        _ => return None,
    })
}

/// Раскладывает атрибуты элемента по полям [`RawElement`].
fn read_attributes(kind: ElementKind, element: &XmlElement) -> RawElement {
    let mut raw = RawElement::new(kind);
    for attribute in element.attributes() {
        let name = attribute.name();
        let value = attribute.value();
        match name.namespace_uri() {
            // Атрибуты без пространства имён: время, ссылки.
            None => match name.local_name() {
                "begin" => raw.timing.begin = Some(value.to_owned()),
                "end" => raw.timing.end = Some(value.to_owned()),
                "dur" => raw.timing.dur = Some(value.to_owned()),
                "timeContainer" => raw.timing.time_container = Some(value.to_owned()),
                "region" => raw.region = Some(value.to_owned()),
                "style" => {
                    raw.style_refs = value.split_whitespace().map(str::to_owned).collect();
                }
                _ => {}
            },
            Some(XML_NAMESPACE) => match name.local_name() {
                "id" => raw.id = Some(value.to_owned()),
                "lang" => raw.language = Some(value.to_owned()).filter(|lang| !lang.is_empty()),
                "space" => {
                    raw.space = match value {
                        "preserve" => Some(SpaceHandling::Preserve),
                        "default" => Some(SpaceHandling::Default),
                        _ => None,
                    }
                }
                _ => {}
            },
            Some(STYLING_NAMESPACE) => raw.style_attributes.push(StyleAttribute {
                name: name.local_name().to_owned(),
                value: value.to_owned(),
            }),
            Some(PARAMETER_NAMESPACE) => raw.parameters.push(StyleAttribute {
                name: name.local_name().to_owned(),
                value: value.to_owned(),
            }),
            Some(_) => {}
        }
    }
    raw
}
