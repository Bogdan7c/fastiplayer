//! Общий сбор улик для строк входов 10c, у которых страница под тенью не судится по метрикам
//! декодера (на странице несколько видео или она открыта ссылкой): кадры плеера, отметки
//! посредника, отчёт страницы о её собственных проверках, запрещённые строки плеера.

use super::super::context::ScenarioError;
use super::super::evidence::{
    FORBIDDEN_PLAYER_LINES, format_browser_log, host_failures, page_failures,
    unexpected_player_lines,
};
use super::common::ReadyRun;
use crate::observe::{FrameEvidence, HostLogEvidence, read_host_evidence};
use crate::site::{PageReport, SiteError};

/// Что собрано к концу строки.
pub(super) struct EntryEvidence {
    pub(super) frames: Result<FrameEvidence, String>,
    pub(super) host: HostLogEvidence,
    pub(super) page_report: Result<PageReport, SiteError>,
    pub(super) forbidden_player_lines: Vec<String>,
    pub(super) page_log: String,
}

/// Завершает страницу (`finish`), снимает снимок экрана, закрывает браузер и собирает улики.
pub(super) fn collect(
    mut ready: ReadyRun<'_, '_>,
    frames: Result<FrameEvidence, String>,
) -> Result<EntryEvidence, ScenarioError> {
    let page_report = ready.run.finish_page();
    let page_log = format_browser_log(&ready.run.take_page_log());
    let forbidden_player_lines = ready
        .run
        .player_log
        .snapshot_forbidden(FORBIDDEN_PLAYER_LINES)?;
    let _ = ready.robot.take_screenshot("end-of-row");
    let exit = ready.run.browser.shutdown()?;
    let host = read_host_evidence(&exit.stderr_log)?;
    Ok(EntryEvidence {
        frames,
        host,
        page_report,
        forbidden_player_lines,
        page_log,
    })
}

impl EntryEvidence {
    /// Провалы по общим уликам: плеер показал кадры, посредник выдал пропуск, страница честна и
    /// без новых запрещённых строк плеера.
    pub(super) fn failures(&self, page_id: &str) -> Vec<String> {
        let mut failures = Vec::new();
        match &self.frames {
            Ok(found) if found.position_regressions > 0 => failures.push(format!(
                "playback position went backwards {} time(s): {}",
                found.position_regressions, found.summary_line
            )),
            Ok(_) => {}
            Err(message) => failures.push(message.clone()),
        }
        failures.extend(host_failures(&self.host));
        failures.extend(page_failures(&self.page_report, page_id));
        failures.extend(
            unexpected_player_lines(&self.forbidden_player_lines)
                .map(|line| format!("forbidden player line: {line}")),
        );
        failures
    }

    /// Сколько кадров показал плеер (для сводки).
    pub(super) fn presented_frames(&self) -> u64 {
        self.frames
            .as_ref()
            .map_or(0, |found| found.presented_frames)
    }
}
