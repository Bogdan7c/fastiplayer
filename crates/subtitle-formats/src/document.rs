//! Результат разбора одного файла: события, регионы и отчёт.

use subtitle_core::{SubtitleEvent, SubtitleRegion};

use crate::report::SubtitleParseReport;

/// Разобранный документ. Собирается парсером; внешний код только читает.
#[derive(Debug, Clone, PartialEq)]
pub struct ParsedSubtitleDocument {
    events: Vec<SubtitleEvent>,
    regions: Vec<SubtitleRegion>,
    report: SubtitleParseReport,
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
        }
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

    /// Разбирает документ на части, чтобы отдать события в `SubtitleTimeline::from_events`.
    pub fn into_parts(self) -> (Vec<SubtitleEvent>, Vec<SubtitleRegion>, SubtitleParseReport) {
        (self.events, self.regions, self.report)
    }
}
