//! Результат разбора одного файла: события, регионы и отчёт.

use std::sync::Arc;

use subtitle_core::{AssScript, SubtitleEvent, SubtitleRegion};

use crate::ass::AssParseReport;
use crate::report::SubtitleParseReport;

/// Разобранный документ. Собирается парсером; внешний код только читает.
#[derive(Debug, Clone, PartialEq)]
pub struct ParsedSubtitleDocument {
    events: Vec<SubtitleEvent>,
    regions: Vec<SubtitleRegion>,
    report: SubtitleParseReport,
    ass: Option<AssDocumentDetails>,
}

/// Сведения, которые есть только у документа ASS/SSA: сценарий и счётчики тегов.
#[derive(Debug, Clone, PartialEq)]
struct AssDocumentDetails {
    script: Arc<AssScript>,
    report: AssParseReport,
}

impl ParsedSubtitleDocument {
    pub(crate) fn new(
        events: Vec<SubtitleEvent>,
        regions: Vec<SubtitleRegion>,
        report: SubtitleParseReport,
    ) -> Self {
        Self {
            events,
            regions,
            report,
            ass: None,
        }
    }

    /// Прикрепляет сценарий и отчёт тегов ASS (только для документов `.ass/.ssa`).
    pub(crate) fn with_ass(mut self, script: Arc<AssScript>, report: AssParseReport) -> Self {
        self.ass = Some(AssDocumentDetails { script, report });
        self
    }

    /// События в порядке появления в файле (не отсортированы по времени).
    pub fn events(&self) -> &[SubtitleEvent] {
        &self.events
    }

    /// Регионы размещения (SRT их не имеет).
    pub fn regions(&self) -> &[SubtitleRegion] {
        &self.regions
    }

    pub fn report(&self) -> &SubtitleParseReport {
        &self.report
    }

    /// Сценарий ASS/SSA (`PlayRes`, стили) — нужен растеризатору и тогда, когда событий нет.
    /// `None` для SRT и WebVTT.
    pub fn ass_script(&self) -> Option<&Arc<AssScript>> {
        self.ass.as_ref().map(|details| &details.script)
    }

    /// Счётчики разбора ASS: неподдерживаемые и неизвестные теги, пропущенные стили.
    /// `None` для SRT и WebVTT.
    pub fn ass_report(&self) -> Option<&AssParseReport> {
        self.ass.as_ref().map(|details| &details.report)
    }

    /// Разбирает документ на части, чтобы отдать события в `SubtitleTimeline::from_events`.
    pub fn into_parts(self) -> (Vec<SubtitleEvent>, Vec<SubtitleRegion>, SubtitleParseReport) {
        (self.events, self.regions, self.report)
    }
}
