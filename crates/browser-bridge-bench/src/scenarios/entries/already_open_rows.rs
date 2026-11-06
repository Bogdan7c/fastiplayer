//! Окно плеера уже открыто (решение 216): вход без попапа говорит об этом плашкой
//! `switch_to_player`, а окно плеера НЕ поднимается (иначе зритель потерял бы страницу из виду).
//!
//! Два случая: то же видео тем же входом второй раз; видео ДРУГОЙ вкладки при открытом окне
//! (плеер заменяет видео в том же окне, плашка на той вкладке, где человек вошёл).

use std::time::Duration;

use super::super::context::{ScenarioError, Stand};
use super::super::outcome::Verdict;
use super::super::run_tweaks::RunTweaks;
use super::Entry;
use super::common::{
    DEFAULT_QUERY, EarlyUnsupported, Opened, open_ready, poll_until, with_extra_failures,
};
use super::entry_evidence::collect;
use super::menu_entry::BANNER_TIMEOUT;
use super::start_rows::{prepare_entry, spot_of_video};
use crate::browsers::BrowserKind;
use crate::environment::BenchEnvironment;

/// Плашка «окно плеера уже открыто».
const SWITCH_BANNER: &str = "switch_to_player";
/// Сколько вторая страница наблюдает себя под тенью, прежде чем отчитаться (ей нужны секунды хода часов).
const SECOND_PAGE_SHADOW_WINDOW: Duration = Duration::from_secs(6);
/// Сколько ждём, пока новое окно плеера станет активным.
const PLAYER_FOCUS_TIMEOUT: Duration = Duration::from_secs(15);
/// Сколько ждём вторую страницу.
const SECOND_PAGE_READY: Duration = Duration::from_secs(60);

/// Какой из двух случаев.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) enum AlreadyOpenRow {
    /// То же видео, тот же вход второй раз.
    SameVideoAgain,
    /// Другая вкладка с другим видео при открытом окне.
    OtherTab,
}

impl AlreadyOpenRow {
    pub(super) fn name(self) -> &'static str {
        match self {
            Self::SameVideoAgain => "already-open-same-video",
            Self::OtherTab => "already-open-other-tab",
        }
    }
}

pub(super) fn run_already_open(
    stand: &Stand,
    environment: &BenchEnvironment,
    browser: BrowserKind,
    entry: Entry,
    row: AlreadyOpenRow,
) -> Result<Verdict, ScenarioError> {
    let first_page = "mse-fmp4";
    let mut ready = match open_ready(
        stand,
        environment,
        browser,
        first_page,
        &RunTweaks::default(),
        DEFAULT_QUERY,
        EarlyUnsupported::OnlyWhereTheTableAllows,
    )? {
        Opened::Ready(ready) => *ready,
        Opened::Finished(verdict) => return Ok(verdict),
    };
    prepare_entry(&mut ready, entry)?;
    let mut failures = Vec::new();
    ready.enter_at(entry, spot_of_video(first_page))?;
    failures.extend(ready.wait_entry_started(entry));
    ready.run.tell_page("handed-off");
    let frames = ready.wait_frames()?;
    // Новое окно плеера забирает фокус (решение 216): ждём, пока активным станет не браузер, и запоминаем
    // класс окна плеера. Без этого проверка «окно не поднялось» ничего бы не доказывала.
    let browser_name = browser.catalog_name();
    let player_class = poll_until(PLAYER_FOCUS_TIMEOUT, || {
        ready
            .robot
            .active_window_class()
            .ok()
            .filter(|class| !class.to_lowercase().contains(browser_name))
    });
    let Some(player_class) = player_class else {
        failures.push(format!(
            "the new player window never became the active window (the {browser_name} window stayed active)"
        ));
        return Ok(with_extra_failures(
            Verdict::pass("no player focus".to_owned()),
            &failures,
        ));
    };

    let second_page = match row {
        AlreadyOpenRow::SameVideoAgain => first_page,
        AlreadyOpenRow::OtherTab => {
            let second = "mse-webm";
            ready.run.open_additional_page(second, second)?;
            if !matches!(
                ready
                    .run
                    .wait_page_marker("handoff-ready", SECOND_PAGE_READY),
                super::super::handoff_run::PageReadiness::Ready
            ) {
                failures.push("the second page never became ready".to_owned());
            }
            second
        }
    };
    ready.enter_at(entry, spot_of_video(second_page))?;
    if let Err(problem) = ready.wait_for_banner(SWITCH_BANNER, BANNER_TIMEOUT) {
        failures.push(problem);
    }
    let class_after = ready.robot.active_window_class()?;
    if row == AlreadyOpenRow::OtherTab {
        // Видео второй страницы теперь в плеере: страница отчитывается о своём окне под тенью.
        ready.run.tell_page("handed-off");
        std::thread::sleep(SECOND_PAGE_SHADOW_WINDOW);
    }
    if class_after == player_class {
        failures.push(format!(
            "the player window took the focus ({class_after}); the banner exists so that the viewer switches himself"
        ));
    }
    let evidence = collect(ready, frames)?;
    // Страница второй вкладки в `evidence` отчитывается от своего прогона.
    failures.extend(evidence.failures(second_page));
    let expected_grants = if row == AlreadyOpenRow::OtherTab {
        2
    } else {
        1
    };
    if evidence.host.session_grants != expected_grants {
        failures.push(format!(
            "the player granted {} session(s), expected {expected_grants}",
            evidence.host.session_grants
        ));
    }
    let summary = format!(
        "banner {SWITCH_BANNER} shown; active window before {player_class:?}, after {class_after:?}; grants={}",
        evidence.host.session_grants
    );
    Ok(with_extra_failures(Verdict::pass(summary), &failures))
}
