//! Чтение XML srv3 через `bounded-xml-reader` -> [`RawDocument`].
//!
//! Здесь только структура файла: какой элемент где лежит. Смысл времени, окон и текста
//! придают модули `lines` и `rolling`.

use bounded_xml_reader::{BoundedXmlReader, XmlBudgets, XmlElement, XmlEvent, XmlReadError};

use super::model::{RawDocument, RawParagraph, RawSpan, WindowDefinition};
use super::pen::Pen;
use super::window::{WindowPosition, WindowStyle};
use crate::error::SubtitleLoadError;
use crate::limits::SubtitleParseLimits;

/// Имя корневого элемента srv3.
const ROOT_ELEMENT: &str = "timedtext";
/// Допустимая вложенность: timedtext > body > p > s и небольшой запас на лишние обёртки.
const MAX_XML_DEPTH: usize = 16;
/// Атрибутов на элементе: у `pen` их около 12, запас на расширения YouTube.
const MAX_ATTRIBUTES_PER_ELEMENT: usize = 32;
/// Объявления пространств имён в srv3 не нужны вовсе; небольшой запас на `xmlns` у корня.
const MAX_NAMESPACE_DECLARATIONS_PER_ELEMENT: usize = 4;
/// Всего объявлений пространств имён на документ.
const MAX_NAMESPACE_DECLARATIONS: usize = 16;
/// Суммарный объём префиксов и URI пространств имён.
const MAX_NAMESPACE_BYTES: usize = 1024;

/// Где мы находимся в дереве: от этого зависит смысл текста и дочерних элементов.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Context {
    Root,
    Head,
    Body,
    Paragraph,
    Span,
    /// Неизвестный элемент: его содержимое пропускается.
    Ignored,
}

/// Читает srv3 из UTF-8 текста.
///
/// Бюджеты XML выводятся из `limits.max_input_bytes`: токенов, атрибутов и текста не может
/// быть больше, чем байтов во входе, поэтому они не отсекают честные файлы, но задают
/// конечную границу. Нарушение любого бюджета или синтаксиса — фатальная ошибка файла.
pub(super) fn read_document(
    text: &str,
    limits: &SubtitleParseLimits,
) -> Result<RawDocument, SubtitleLoadError> {
    let budgets = xml_budgets(limits)?;
    let mut reader = BoundedXmlReader::new(text.as_bytes(), budgets).map_err(map_xml_error)?;
    let mut state = ReaderState::default();
    while let Some(event) = reader.next_event().map_err(map_xml_error)? {
        state.handle(event)?;
    }
    state.finish()
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
        .map_err(|_| SubtitleLoadError::Srv3TooComplex)
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
        | XmlReadError::TextBytesExceeded { .. } => SubtitleLoadError::Srv3TooComplex,
        _ => SubtitleLoadError::MalformedSrv3,
    }
}

/// Накопитель чтения.
#[derive(Default)]
struct ReaderState {
    document: RawDocument,
    /// Стек открытых элементов.
    stack: Vec<Context>,
    /// Корень `timedtext` встречен.
    root_seen: bool,
    /// Абзац, который сейчас читается.
    paragraph: Option<RawParagraph>,
    /// Сколько абзацев уже встречено (для `RawParagraph::ordinal`).
    paragraph_count: usize,
}

impl ReaderState {
    fn handle(&mut self, event: XmlEvent) -> Result<(), SubtitleLoadError> {
        match event {
            XmlEvent::StartElement(element) => {
                let context = self.enter(&element)?;
                self.stack.push(context);
            }
            XmlEvent::EmptyElement(element) => {
                let context = self.enter(&element)?;
                self.leave(context);
            }
            XmlEvent::EndElement(_) => {
                if let Some(context) = self.stack.pop() {
                    self.leave(context);
                }
            }
            XmlEvent::Text(text) => self.append_text(text.content()),
        }
        Ok(())
    }

    /// Контекст нового элемента относительно текущего; заодно запоминает определения.
    fn enter(&mut self, element: &XmlElement) -> Result<Context, SubtitleLoadError> {
        let name = element.name().local_name();
        let Some(parent) = self.stack.last().copied() else {
            return self.enter_root(element, name);
        };
        Ok(match (parent, name) {
            (Context::Root, "head") => Context::Head,
            (Context::Root, "body") => Context::Body,
            (Context::Head, "pen") => {
                self.define_pen(element);
                Context::Ignored
            }
            (Context::Head, "ws") => {
                self.define_window_style(element);
                Context::Ignored
            }
            (Context::Head, "wp") => {
                self.define_window_position(element);
                Context::Ignored
            }
            (Context::Body, "w") => {
                self.define_window(element);
                Context::Ignored
            }
            (Context::Body, "p") => {
                self.begin_paragraph(element);
                Context::Paragraph
            }
            (Context::Paragraph, "s") => {
                self.begin_span(element);
                Context::Span
            }
            (Context::Paragraph | Context::Span, "br") => {
                self.append_text("\n");
                Context::Ignored
            }
            _ => Context::Ignored,
        })
    }

    /// Корень: обязан быть `timedtext` формата 3 (или без `format` — терпим).
    fn enter_root(
        &mut self,
        element: &XmlElement,
        name: &str,
    ) -> Result<Context, SubtitleLoadError> {
        if name != ROOT_ELEMENT {
            return Err(SubtitleLoadError::NotSrv3);
        }
        // Отсутствие `format` терпим (часть инструментов его не пишет); `format="1"`/`"2"` —
        // другие форматы с другой структурой, которые этот разборщик не читает.
        if let Some(format) = attribute(element, "format")
            && format != "3"
        {
            return Err(SubtitleLoadError::NotSrv3);
        }
        self.root_seen = true;
        Ok(Context::Root)
    }

    fn leave(&mut self, context: Context) {
        if context == Context::Paragraph
            && let Some(paragraph) = self.paragraph.take()
        {
            self.document.paragraphs.push(paragraph);
        }
    }

    fn define_pen(&mut self, element: &XmlElement) {
        let Some(id) = numeric_id(element) else {
            self.document.ignored_style_items += 1;
            return;
        };
        let parsed = Pen::from_attributes(attribute_pairs(element));
        if parsed.has_ignored_attributes {
            self.document.ignored_style_items += 1;
        }
        // При повторе id действует первое определение.
        self.document.pens.entry(id).or_insert(parsed.pen);
    }

    fn define_window_style(&mut self, element: &XmlElement) {
        let Some(id) = numeric_id(element) else {
            self.document.ignored_style_items += 1;
            return;
        };
        let parsed = WindowStyle::from_attributes(attribute_pairs(element));
        if parsed.has_ignored_attributes {
            self.document.ignored_style_items += 1;
        }
        self.document
            .window_styles
            .entry(id)
            .or_insert(parsed.style);
    }

    fn define_window_position(&mut self, element: &XmlElement) {
        let Some(id) = numeric_id(element) else {
            self.document.ignored_style_items += 1;
            return;
        };
        let parsed = WindowPosition::from_attributes(attribute_pairs(element));
        if parsed.has_ignored_attributes {
            self.document.ignored_style_items += 1;
        }
        self.document
            .window_positions
            .entry(id)
            .or_insert(parsed.position);
    }

    fn define_window(&mut self, element: &XmlElement) {
        let Some(id) = numeric_id(element) else {
            self.document.ignored_style_items += 1;
            return;
        };
        self.document.windows.entry(id).or_insert(WindowDefinition {
            position_id: attribute_number(element, "wp"),
            style_id: attribute_number(element, "ws"),
        });
    }

    fn begin_paragraph(&mut self, element: &XmlElement) {
        let ordinal = self.paragraph_count;
        self.paragraph_count += 1;
        self.paragraph = Some(RawParagraph {
            ordinal,
            start_ms: attribute_number(element, "t"),
            duration_ms: attribute_number(element, "d"),
            window_id: attribute_number(element, "w"),
            position_id: attribute_number(element, "wp"),
            style_id: attribute_number(element, "ws"),
            pen_id: attribute_number(element, "p"),
            is_append: attribute(element, "a") == Some("1"),
            spans: Vec::new(),
        });
    }

    fn begin_span(&mut self, element: &XmlElement) {
        if let Some(paragraph) = self.paragraph.as_mut() {
            paragraph.spans.push(RawSpan {
                offset_ms: attribute_number(element, "t"),
                pen_id: attribute_number(element, "p"),
                text: String::new(),
                is_direct_text: false,
            });
        }
    }

    /// Текст попадает только в абзац или слово; всё остальное (пробелы между элементами) —
    /// мимо.
    fn append_text(&mut self, text: &str) {
        let Some(paragraph) = self.paragraph.as_mut() else {
            return;
        };
        match self.stack.last() {
            Some(Context::Span) => {
                if let Some(span) = paragraph.spans.last_mut() {
                    span.text.push_str(text);
                }
            }
            Some(Context::Paragraph) => {
                // Соседние куски (текст и сущности приходят отдельными событиями) склеиваем.
                if let Some(span) = paragraph
                    .spans
                    .last_mut()
                    .filter(|span| span.is_direct_text)
                {
                    span.text.push_str(text);
                } else {
                    paragraph.spans.push(RawSpan {
                        offset_ms: None,
                        pen_id: None,
                        text: text.to_owned(),
                        is_direct_text: true,
                    });
                }
            }
            _ => {}
        }
    }

    fn finish(self) -> Result<RawDocument, SubtitleLoadError> {
        if self.root_seen {
            Ok(self.document)
        } else {
            Err(SubtitleLoadError::NotSrv3)
        }
    }
}

/// Значение атрибута без пространства имён.
fn attribute<'element>(element: &'element XmlElement, name: &str) -> Option<&'element str> {
    element
        .attributes()
        .iter()
        .find(|candidate| {
            candidate.name().namespace_uri().is_none() && candidate.name().local_name() == name
        })
        .map(|found| found.value())
}

/// Целое неотрицательное значение атрибута; нечитаемое = отсутствующее.
fn attribute_number<T: std::str::FromStr>(element: &XmlElement, name: &str) -> Option<T> {
    attribute(element, name)?.trim().parse().ok()
}

fn numeric_id(element: &XmlElement) -> Option<u32> {
    attribute_number(element, "id")
}

fn attribute_pairs(element: &XmlElement) -> impl Iterator<Item = (&str, &str)> {
    element
        .attributes()
        .iter()
        .map(|attribute| (attribute.name().local_name(), attribute.value()))
}
