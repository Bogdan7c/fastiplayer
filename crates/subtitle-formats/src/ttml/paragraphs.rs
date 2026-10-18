//! Обход дерева `body` -> `div` -> `p` -> `span`: окна показа, стили, регион и «листья» текста.
//!
//! Лист — кусок текста или перенос строки со своим окном показа и итоговым стилем.
//! Дальше `emit` режет абзац по моментам, где набор видимых листьев меняется.

use subtitle_core::SpanStyle;

use super::layout::{RegionBook, RegionRect};
use super::model::{ElementKind, RawChild, RawElement, RawTree, SpaceHandling};
use super::style::{StyleBook, StyleSet};
use super::timing::{TimeParameters, TimeWindow, WindowError};
use crate::error::SubtitleLoadError;
use crate::report::{LineNumber, ParseIssue, ParseIssueKind, SubtitleParseReport};

/// Сколько листьев допускается в одном абзаце: срез по моментам смены стоит
/// O(листья x границы), поэтому враждебный абзац из сотен тысяч `span` недопустим.
const MAX_LEAVES_PER_PARAGRAPH: usize = 2048;

/// Что показывает лист.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(super) enum LeafContent {
    /// Сырой текст (пробелы ещё не обработаны).
    Text(String),
    /// `<br/>`.
    LineBreak,
}

/// Кусок содержимого абзаца со своим окном показа.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(super) struct Leaf {
    pub(super) window: TimeWindow,
    pub(super) content: LeafContent,
    /// Итоговый стиль текста (регион уже учтён).
    pub(super) style: SpanStyle,
    pub(super) language: Option<String>,
    pub(super) space: SpaceHandling,
}

/// Абзац со всем, что нужно для выпуска реплик.
#[derive(Debug, Clone, PartialEq)]
pub(super) struct Paragraph {
    pub(super) window: TimeWindow,
    pub(super) region: Option<RegionRect>,
    /// Итоговые свойства абзаца (для выравнивания блока).
    pub(super) style: StyleSet,
    pub(super) leaves: Vec<Leaf>,
}

/// Итог обхода документа.
#[derive(Debug, Default)]
pub(super) struct CollectedParagraphs {
    pub(super) paragraphs: Vec<Paragraph>,
    /// Позднейший конечный момент среди ВСЕХ окон документа (для абзацев без конца).
    pub(super) latest_finite_micros: i64,
    /// Сколько свойств/конструкций проигнорировано при обходе.
    pub(super) ignored_count: usize,
}

/// Всё неизменное, что нужно обходу.
pub(super) struct DocumentContext<'document> {
    pub(super) tree: &'document RawTree,
    pub(super) styles: &'document StyleBook,
    pub(super) regions: &'document RegionBook,
    pub(super) time: TimeParameters,
}

/// Наследуемое состояние при спуске по дереву.
#[derive(Clone)]
struct Inherited<'document> {
    window: TimeWindow,
    /// Стиль предков БЕЗ региона (регион — самый слабый слой, добавляется в конце).
    style: StyleSet,
    region_id: Option<&'document str>,
    space: SpaceHandling,
    language: Option<&'document str>,
}

/// Обходит документ и собирает абзацы.
pub(super) fn collect_paragraphs(
    context: &DocumentContext<'_>,
    report: &mut SubtitleParseReport,
) -> Result<CollectedParagraphs, SubtitleLoadError> {
    let mut walker = Walker {
        context,
        report,
        collected: CollectedParagraphs::default(),
        paragraph_count: 0,
    };
    let Some(root) = context.tree.element(context.tree.root) else {
        return Ok(walker.collected);
    };
    let inherited = Inherited {
        window: TimeWindow::DOCUMENT,
        style: StyleSet::default(),
        region_id: None,
        space: root.space.unwrap_or_default(),
        language: root.language.as_deref(),
    };
    for body in context.tree.child_elements_of_kind(root, ElementKind::Body) {
        walker.walk_container(body, &inherited)?;
    }
    Ok(walker.collected)
}

struct Walker<'run, 'document> {
    context: &'run DocumentContext<'document>,
    report: &'run mut SubtitleParseReport,
    collected: CollectedParagraphs,
    /// Сколько абзацев встречено (номер «строки» в отчёте).
    paragraph_count: usize,
}

impl<'document> Walker<'_, 'document> {
    /// `body` или `div`: спускаемся к дочерним `div` и `p`.
    fn walk_container(
        &mut self,
        index: usize,
        parent: &Inherited<'document>,
    ) -> Result<(), SubtitleLoadError> {
        let Some(element) = self.context.tree.element(index) else {
            return Ok(());
        };
        let Some(inherited) = self.descend(element, parent, StyleSet::without_background) else {
            return Ok(());
        };
        for child in &element.children {
            let RawChild::Element(child_index) = child else {
                continue;
            };
            match self.context.tree.element(*child_index).map(|e| e.kind) {
                Some(ElementKind::Div) => self.walk_container(*child_index, &inherited)?,
                Some(ElementKind::Paragraph) => self.walk_paragraph(*child_index, &inherited)?,
                _ => {}
            }
        }
        Ok(())
    }

    /// Окно, стиль и наследуемое состояние дочернего элемента. `None` — элемент не показывается.
    ///
    /// `filter_style` отрезает то, что на этом уровне не переносится на текст (фон контейнера).
    fn descend(
        &mut self,
        element: &'document RawElement,
        parent: &Inherited<'document>,
        filter_style: fn(StyleSet) -> StyleSet,
    ) -> Option<Inherited<'document>> {
        let window = match parent.window.child(&element.timing, &self.context.time) {
            Ok(window) => window,
            Err(WindowError::NeverActive) => return None,
            Err(WindowError::InvalidTime) => {
                self.report.record_issue(ParseIssue {
                    line_number: LineNumber::from_zero_based_index(self.paragraph_count),
                    kind: ParseIssueKind::InvalidTimestamp,
                });
                return None;
            }
        };
        self.note_window(window);
        let (specified, ignored) = self.context.styles.specified_for(element);
        self.collected.ignored_count += ignored;
        if element
            .timing
            .time_container
            .as_deref()
            .is_some_and(|c| c != "par")
        {
            // `seq` (последовательное время детей) не поддержан: дети читаются как `par`.
            self.collected.ignored_count += 1;
        }
        Some(Inherited {
            window,
            style: filter_style(specified).over(parent.style),
            region_id: element.region.as_deref().or(parent.region_id),
            space: element.space.unwrap_or(parent.space),
            language: element.language.as_deref().or(parent.language),
        })
    }

    fn note_window(&mut self, window: TimeWindow) {
        let latest = window
            .end_micros
            .unwrap_or(window.begin_micros)
            .max(window.begin_micros);
        self.collected.latest_finite_micros = self.collected.latest_finite_micros.max(latest);
    }

    fn walk_paragraph(
        &mut self,
        index: usize,
        parent: &Inherited<'document>,
    ) -> Result<(), SubtitleLoadError> {
        let Some(element) = self.context.tree.element(index) else {
            return Ok(());
        };
        self.paragraph_count += 1;
        let Some(inherited) = self.descend(element, parent, |style| style) else {
            return Ok(());
        };
        let region = inherited
            .region_id
            .and_then(|id| self.context.regions.get(id));
        if inherited.region_id.is_some() && region.is_none() {
            // Ссылка на несуществующий регион: показываем в положении по умолчанию.
            self.collected.ignored_count += 1;
        }
        let region_style = region.map(|region| region.style).unwrap_or_default();
        let mut leaves = Vec::new();
        self.collect_leaves(element, &inherited, region_style, &mut leaves)?;
        self.collected.paragraphs.push(Paragraph {
            window: inherited.window,
            region: region.map(|region| region.rect),
            style: inherited.style.over(region_style),
            leaves,
        });
        Ok(())
    }

    /// Листья внутри `p` или `span`: текст, вложенные `span` и `br`.
    fn collect_leaves(
        &mut self,
        element: &'document RawElement,
        inherited: &Inherited<'document>,
        region_style: StyleSet,
        leaves: &mut Vec<Leaf>,
    ) -> Result<(), SubtitleLoadError> {
        for child in &element.children {
            match child {
                RawChild::Text(text) => {
                    let leaf = leaf_of(LeafContent::Text(text.clone()), inherited, region_style);
                    push_leaf(leaves, leaf)?;
                }
                RawChild::Element(child_index) => {
                    let Some(child_element) = self.context.tree.element(*child_index) else {
                        continue;
                    };
                    if child_element.region.is_some() {
                        // Свой регион у span/br расщепил бы реплику: не поддержан.
                        self.collected.ignored_count += 1;
                    }
                    match child_element.kind {
                        ElementKind::Span => {
                            let Some(nested) = self.descend(child_element, inherited, |s| s) else {
                                continue;
                            };
                            self.collect_leaves(child_element, &nested, region_style, leaves)?;
                        }
                        ElementKind::LineBreak => {
                            let Some(nested) = self.descend(child_element, inherited, |s| s) else {
                                continue;
                            };
                            let leaf = leaf_of(LeafContent::LineBreak, &nested, region_style);
                            push_leaf(leaves, leaf)?;
                        }
                        _ => {}
                    }
                }
            }
        }
        Ok(())
    }
}

fn leaf_of(content: LeafContent, inherited: &Inherited<'_>, region_style: StyleSet) -> Leaf {
    Leaf {
        window: inherited.window,
        content,
        style: inherited.style.over(region_style).to_span_style(),
        language: inherited.language.map(str::to_owned),
        space: inherited.space,
    }
}

fn push_leaf(leaves: &mut Vec<Leaf>, leaf: Leaf) -> Result<(), SubtitleLoadError> {
    if leaves.len() >= MAX_LEAVES_PER_PARAGRAPH {
        return Err(SubtitleLoadError::TtmlTooComplex);
    }
    leaves.push(leaf);
    Ok(())
}
