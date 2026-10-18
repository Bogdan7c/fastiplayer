//! Сырое дерево TTML после чтения XML: структура и атрибуты как в файле, без смысла.
//!
//! Смысл (время, стили, регионы) придают соседние модули. Дерево хранится «плоско» в
//! массиве: узел ссылается на детей индексами, поэтому обход не требует `Rc`/рекурсивных
//! владельцев, а размер ограничен бюджетами XML.

/// Виды элементов TTML, которые нас интересуют. Всё остальное — [`ElementKind::Other`].
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) enum ElementKind {
    /// Корень `<tt>`.
    Tt,
    /// `<head>`: контейнер стилей и раскладки.
    Head,
    /// `<styling>`: контейнер определений стилей.
    Styling,
    /// `<style>`: определение (внутри `styling`) стиля.
    Style,
    /// `<layout>`: контейнер регионов.
    Layout,
    /// `<region>`: прямоугольник показа.
    Region,
    /// `<body>`.
    Body,
    /// `<div>`.
    Div,
    /// `<p>`: абзац — единица, из которой получаются реплики.
    Paragraph,
    /// `<span>`.
    Span,
    /// `<br/>`: принудительный перенос строки.
    LineBreak,
}

/// Режим обработки пробелов (`xml:space`).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub(super) enum SpaceHandling {
    /// Пробельные символы схлопываются (значение по умолчанию в TTML).
    #[default]
    Default,
    /// Пробельные символы и переводы строки сохраняются как есть.
    Preserve,
}

/// Атрибут стиля `tts:*` (имя без префикса и значение как в файле).
#[derive(Debug, Clone, PartialEq, Eq)]
pub(super) struct StyleAttribute {
    pub(super) name: String,
    pub(super) value: String,
}

/// Атрибуты времени как текст; разбирает модуль `timing`.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub(super) struct TimingAttributes {
    pub(super) begin: Option<String>,
    pub(super) end: Option<String>,
    pub(super) dur: Option<String>,
    /// `timeContainer`: `par` (по умолчанию) или `seq` (не поддержан).
    pub(super) time_container: Option<String>,
}

/// Один элемент дерева.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(super) struct RawElement {
    pub(super) kind: ElementKind,
    /// `xml:id`.
    pub(super) id: Option<String>,
    pub(super) timing: TimingAttributes,
    /// Атрибут `region` (ссылка на `xml:id` региона).
    pub(super) region: Option<String>,
    /// Атрибут `style`: ссылки на стили в порядке записи.
    pub(super) style_refs: Vec<String>,
    /// `xml:space`; `None` — наследуется.
    pub(super) space: Option<SpaceHandling>,
    /// `xml:lang`.
    pub(super) language: Option<String>,
    /// Атрибуты `tts:*` этого элемента.
    pub(super) style_attributes: Vec<StyleAttribute>,
    /// Атрибуты `ttp:*` (параметры документа; осмысленны только на корне).
    pub(super) parameters: Vec<StyleAttribute>,
    /// Дети в порядке документа.
    pub(super) children: Vec<RawChild>,
}

impl RawElement {
    pub(super) fn new(kind: ElementKind) -> Self {
        Self {
            kind,
            id: None,
            timing: TimingAttributes::default(),
            region: None,
            style_refs: Vec::new(),
            space: None,
            language: None,
            style_attributes: Vec::new(),
            parameters: Vec::new(),
            children: Vec::new(),
        }
    }

    /// Значение параметра `ttp:<name>` или атрибута `tts:<name>` корня.
    pub(super) fn parameter(&self, name: &str) -> Option<&str> {
        self.parameters
            .iter()
            .find(|attribute| attribute.name == name)
            .map(|attribute| attribute.value.as_str())
    }

    /// Значение атрибута `tts:<name>`.
    pub(super) fn style_attribute(&self, name: &str) -> Option<&str> {
        self.style_attributes
            .iter()
            .find(|attribute| attribute.name == name)
            .map(|attribute| attribute.value.as_str())
    }
}

/// Ребёнок элемента: текст или другой элемент (по индексу в [`RawTree`]).
#[derive(Debug, Clone, PartialEq, Eq)]
pub(super) enum RawChild {
    Text(String),
    Element(usize),
}

/// Всё дерево документа.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(super) struct RawTree {
    pub(super) elements: Vec<RawElement>,
    /// Индекс корня `<tt>`.
    pub(super) root: usize,
    /// Сколько конструкций прочитано, но не поддерживается (анимация `<set>`, `timeContainer`
    /// и подобное): уходит в счётчик отчёта.
    pub(super) unsupported_construct_count: usize,
}

impl RawTree {
    pub(super) fn element(&self, index: usize) -> Option<&RawElement> {
        self.elements.get(index)
    }

    /// Индексы дочерних ЭЛЕМЕНТОВ указанного вида.
    pub(super) fn child_elements_of_kind(
        &self,
        parent: &RawElement,
        kind: ElementKind,
    ) -> Vec<usize> {
        parent
            .children
            .iter()
            .filter_map(|child| match child {
                RawChild::Element(index)
                    if self
                        .element(*index)
                        .is_some_and(|element| element.kind == kind) =>
                {
                    Some(*index)
                }
                _ => None,
            })
            .collect()
    }
}
