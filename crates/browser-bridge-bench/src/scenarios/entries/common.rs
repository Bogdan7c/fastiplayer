//! Общие звенья строк матрицы `entries`: открыть страницу, настоящей рукой войти через вход,
//! собрать итоговые улики. Строки (`*_rows.rs`) только раскладывают их по своим правилам.

use std::time::Duration;

use super::super::capture::{FRAMES_TIMEOUT, MIN_PRESENTED_FRAMES, judge_early_report};
use super::super::context::{ScenarioError, Stand};
use super::super::evidence::{
    SESSION_GRANTED_TIMEOUT, format_browser_log, wait_for_session_granted,
};
use super::super::handoff_run::{ExtensionMode, HANDOFF_MODE_QUERY, HandoffRun, PageReadiness};
use super::super::outcome::Verdict;
use super::super::run_tweaks::RunTweaks;
use super::Entry;
use super::robot::{Robot, wait_for_popup_closed};
use crate::browsers::BrowserKind;
use crate::environment::BenchEnvironment;
use crate::observe::FrameEvidence;
use crate::site::PageStatus;

/// Сколько ждём, пока попап закроется сам после `started`.
pub(super) const POPUP_CLOSE_TIMEOUT: Duration = Duration::from_secs(20);
/// Сколько ждём итоговое сообщение попапа (опрос фоном страницы и ответ).
pub(super) const POPUP_SETTLE_TIMEOUT: Duration = Duration::from_secs(20);

/// Прогон страницы, готовый к входу: страница играет и ждёт, робот с руками на месте.
pub(super) struct ReadyRun<'a, 'env> {
    pub(super) run: HandoffRun<'a>,
    pub(super) robot: Robot<'env>,
}

/// Чем кончилось открытие страницы.
pub(super) enum Opened<'a, 'env> {
    /// Страница готова; можно входить.
    Ready(Box<ReadyRun<'a, 'env>>),
    /// Страница отчиталась раньше (`unsupported` там, где таблица страниц это разрешает) или не
    /// дошла до готовности: вердикт уже готов, браузер закрыт.
    Finished(Verdict),
}

/// Как судить страницу, которая отчиталась `unsupported` ещё до входа.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) enum EarlyUnsupported {
    /// Как везде: честно только там, где таблица страниц стенда это разрешает.
    OnlyWhereTheTableAllows,
    /// Честно везде: страница сама говорит, что браузер с нашим экстеншеном видео не играет.
    /// Для `native-hls`: экстеншен по политике отвечает на `canPlayType(HLS)` пустой строкой
    /// (HLS он не принимает), поэтому страница не доходит до видео и в Chromium.
    Always,
}

/// Открывает страницу стенда в режиме передачи и ждёт готовности.
pub(super) fn open_ready<'a, 'env>(
    stand: &'a Stand,
    environment: &'env BenchEnvironment,
    browser: BrowserKind,
    page_id: &'static str,
    tweaks: &RunTweaks,
    query: &str,
    early_unsupported: EarlyUnsupported,
) -> Result<Opened<'a, 'env>, ScenarioError> {
    let run = HandoffRun::open_tweaked(
        stand,
        environment,
        browser,
        page_id,
        page_id,
        ExtensionMode::Real,
        query,
        tweaks,
    )?;
    let robot = Robot::attach(stand, environment, browser)?;
    match run.wait_page_ready() {
        PageReadiness::Ready => Ok(Opened::Ready(Box::new(ReadyRun { run, robot }))),
        PageReadiness::ReportedEarly(report) => {
            let verdict = if early_unsupported == EarlyUnsupported::Always
                && report.status == PageStatus::Unsupported
            {
                Verdict::unsupported(
                    "the page itself reports unsupported with the extension installed (it hides HLS from canPlayType by policy)",
                )
            } else {
                judge_early_report(page_id, browser, &report)
            };
            run.browser.shutdown()?;
            Ok(Opened::Finished(verdict))
        }
        PageReadiness::TimedOut => {
            let log = format_browser_log(&run.take_page_log());
            run.browser.shutdown()?;
            Ok(Opened::Finished(Verdict::fail(format!(
                "the page never became ready for the entry\n{log}"
            ))))
        }
    }
}

/// Обычный адрес режима передачи.
pub(super) const DEFAULT_QUERY: &str = HANDOFF_MODE_QUERY;

impl ReadyRun<'_, '_> {
    /// Человек входит через `entry`: настоящая рука, никакого `hand_off_tab` от стенда.
    pub(super) fn enter(&self, entry: Entry) -> Result<(), ScenarioError> {
        entry.activate(&self.robot)
    }

    /// Вход через `entry` и ожидание, что попап закроется сам (передача началась).
    /// Возвращает причину провала или `None`.
    pub(super) fn wait_popup_closed(&mut self) -> Option<String> {
        wait_for_popup_closed(self.run.browser.as_mut(), POPUP_CLOSE_TIMEOUT).err()
    }

    /// Пропуск посредника и кадры плеера (≥ 50 показанных).
    pub(super) fn wait_frames(&mut self) -> Result<Result<FrameEvidence, String>, ScenarioError> {
        Ok(
            match wait_for_session_granted(&self.run.stderr_log, SESSION_GRANTED_TIMEOUT)? {
                Ok(()) => self
                    .run
                    .player_log
                    .wait_for_presented_frames(MIN_PRESENTED_FRAMES, FRAMES_TIMEOUT)
                    .map_err(|error| format!("player did not present frames: {error}")),
                Err(message) => Err(format!("{message}; frames were not awaited")),
            },
        )
    }
}

/// Дописывает к вердикту дополнительные провалы строки (сам вердикт `judge_capture` не меняется).
pub(super) fn with_extra_failures(verdict: Verdict, extra: &[String]) -> Verdict {
    if extra.is_empty() {
        return verdict;
    }
    Verdict::fail(format!("{}\n{}", extra.join("; "), verdict.detail))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::scenarios::outcome::Outcome;

    #[test]
    fn extra_failures_turn_a_passing_verdict_into_a_failure_and_name_themselves() {
        let verdict = with_extra_failures(
            Verdict::pass("frames=250"),
            &["the popup did not close by itself".to_owned()],
        );
        assert_eq!(verdict.outcome, Outcome::Fail);
        assert!(verdict.detail.contains("the popup did not close by itself"));
        assert!(verdict.detail.contains("frames=250"));
    }

    #[test]
    fn no_extra_failures_leave_the_verdict_untouched() {
        let verdict = with_extra_failures(Verdict::pass("ok"), &[]);
        assert_eq!(verdict, Verdict::pass("ok"));
    }
}
