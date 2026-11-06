//! Строки «какое видео выбрано входом»: на странице несколько видео или сайт мешает правому клику.
//! Судья — журнал сервера: фон качает ТОЛЬКО файл выбранного видео. Кадры плеера и плашка
//! `opened` подтверждают, что выбранное действительно доиграло до рендера.

use std::time::Instant;

use super::super::capture::decoys::count_fetches;
use super::super::context::{ScenarioError, Stand};
use super::super::outcome::Verdict;
use super::super::run_tweaks::RunTweaks;
use super::Entry;
use super::common::{
    DEFAULT_QUERY, EarlyUnsupported, Opened, ReadyRun, open_ready, with_extra_failures,
};
use super::entry_evidence::collect;
use super::menu_entry::ClickSpot;
use super::page_geometry::{CONTEXT_MENU_LINE_MARK, parse_context_menu_line};
use super::start_rows::prepare_entry;
use crate::browsers::BrowserKind;
use crate::environment::BenchEnvironment;

/// Файлы страниц с несколькими видео на сервере стенда.
pub(super) const LONG_FILE: &str = "/media/progressive/h264-aac-long.mp4";
pub(super) const SHORT_FILE: &str = "/media/progressive/h264-aac.mp4";

/// Действие страницы: нажать play на видео, которое стоит на паузе (у страниц без паузы команда ничего не делает).
const RESUME_PAUSED_COMMAND: &str = "hook:resume-paused";

/// Что делает строка до основного входа.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) enum Precondition {
    /// Ничего.
    None,
    /// Сайт забрал правый клик на видео: сначала робот кликает по самому видео и убеждается, что
    /// страница отменила событие (`prevented=true`), а родное меню не открылось.
    SiteTakesRightClickOnVideo,
}

/// Строка выбора видео.
#[derive(Debug, Clone, Copy)]
pub(super) struct PickRow {
    pub(super) entry: Entry,
    pub(super) page_id: &'static str,
    pub(super) click: ClickSpot,
    /// Файл видео, которое обязано уйти в плеер.
    pub(super) expected_file: &'static str,
    /// Файл видео, которое обязано остаться у браузера (`None` — второго видео нет).
    pub(super) untouched_file: Option<&'static str>,
    pub(super) precondition: Precondition,
    /// Короткое имя правила для таблицы.
    pub(super) rule: &'static str,
}

pub(super) fn run_pick(
    stand: &Stand,
    environment: &BenchEnvironment,
    browser: BrowserKind,
    row: PickRow,
) -> Result<Verdict, ScenarioError> {
    let run_started_at = Instant::now();
    let mut ready = match open_ready(
        stand,
        environment,
        browser,
        row.page_id,
        &RunTweaks::default(),
        DEFAULT_QUERY,
        EarlyUnsupported::OnlyWhereTheTableAllows,
    )? {
        Opened::Ready(ready) => *ready,
        Opened::Finished(verdict) => return Ok(verdict),
    };
    prepare_entry(&mut ready, row.entry)?;
    let mut failures = Vec::new();
    let mut notes = Vec::new();
    if row.precondition == Precondition::SiteTakesRightClickOnVideo {
        match site_takes_right_click(&ready) {
            Ok(note) => notes.push(note),
            Err(problem) => failures.push(problem),
        }
    }
    let handed_off_at = Instant::now();
    ready.enter_at(row.entry, row.click)?;
    failures.extend(ready.wait_entry_started(row.entry));
    ready.run.tell_page("handed-off");
    // Видео, стоявшее на паузе, зритель запускает: плеер играет его, пока страница играет.
    ready.run.tell_page(RESUME_PAUSED_COMMAND);
    let frames = ready.wait_frames()?;
    let evidence = collect(ready, frames)?;
    failures.extend(evidence.failures(row.page_id));
    let mut requests = stand.site.media_requests();
    requests.retain(|request| request.at >= run_started_at);
    let chosen = count_fetches(row.expected_file, &requests, handed_off_at);
    if chosen.background_requests == 0 {
        failures.push(format!(
            "the extension background never fetched {}: the clicked video did not reach the player",
            row.expected_file
        ));
    }
    let mut other_summary = String::from("no other video");
    if let Some(other_file) = row.untouched_file {
        let other = count_fetches(other_file, &requests, handed_off_at);
        if other.background_requests > 0 {
            failures.push(format!(
                "the background fetched {other_file} ({} request(s)) although the other video was clicked",
                other.background_requests
            ));
        }
        other_summary = format!(
            "{other_file}: background requests={}",
            other.background_requests
        );
    }
    notes.push(format!(
        "clicked {:?}; {}: background requests={}; {other_summary}; presented_frames={}",
        row.click.target,
        row.expected_file,
        chosen.background_requests,
        evidence.presented_frames()
    ));
    let summary = notes.join("; ");
    Ok(if failures.is_empty() {
        Verdict::pass(summary)
    } else {
        with_extra_failures(
            Verdict::pass(format!("{summary}\n{}", evidence.page_log)),
            &failures,
        )
    })
}

/// Правый клик по самому видео на странице, где сайт забирает событие: страница отменила его
/// (`prevented=true`) и родное меню не открылось. Без этого строка не доказала бы, что обход нужен.
fn site_takes_right_click(ready: &ReadyRun<'_, '_>) -> Result<String, String> {
    let before = ready.run.page_log_lines(CONTEXT_MENU_LINE_MARK).len();
    let menu = ready
        .right_click_and_pick(ClickSpot::VIDEO)
        .map_err(|error| error.to_string())?;
    let seen = wait_new_contextmenu_line(ready, before)?;
    match menu {
        super::native_menu::MenuPick::MenuNotOpened { .. } if seen.prevented => Ok(format!(
            "the site took the right click on the video (target {}, prevented) and no native menu opened",
            seen.target
        )),
        other => Err(format!(
            "expected the site to swallow the right click on the video; menu result {other:?}, page saw prevented={}",
            seen.prevented
        )),
    }
}

fn wait_new_contextmenu_line(
    ready: &ReadyRun<'_, '_>,
    already_seen: usize,
) -> Result<super::page_geometry::ContextMenuSeen, String> {
    let deadline = Instant::now() + std::time::Duration::from_secs(5);
    loop {
        let lines = ready.run.page_log_lines(CONTEXT_MENU_LINE_MARK);
        if let Some(line) = lines.get(already_seen) {
            return parse_context_menu_line(line);
        }
        if Instant::now() >= deadline {
            return Err("the page never saw the right click on the video".to_owned());
        }
        std::thread::sleep(std::time::Duration::from_millis(100));
    }
}
