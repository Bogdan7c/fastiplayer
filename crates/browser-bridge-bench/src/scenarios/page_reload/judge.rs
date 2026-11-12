//! Чистые приговоры сценария `page-reload` (сессия 16c): на вход только собранные улики, ни ввода-вывода.
//!
//! Улики — строки журнала плеера (окно, процесс, сеансы, полный экран), строки журнала страницы
//! (загрузки документов, состояние видео), сообщение о конце сеанса и числа кадров. Разбор строк тоже
//! здесь, чтобы формат журнала страницы (договор с `site/lib/page-reload-kit.js`) был виден в одном месте.

use std::time::Duration;

use crate::observe::{EndNotice, HostLogEvidence};
use crate::scenarios::capture::max_real_frames;
use crate::scenarios::evidence::{host_failures, unexpected_player_lines};
use crate::scenarios::outcome::Verdict;

/// Окно создано (одна строка на окно).
pub(super) const WINDOW_CREATED_MARKER: &str = "Browser window created";
/// Начало процесса плеера (одна строка на процесс).
pub(super) const PLAYER_PROCESS_MARKER: &str = "=== fastiplayer browser session ===";
/// Сеанс открыт (`intent=… grant=…`): один на сеанс, замена даёт вторую строку.
pub(super) const SESSION_OPENED_MARKER: &str = "Browser session opened";
/// Окно сменило полный экран.
pub(super) const FULLSCREEN_MARKER: &str = "Browser window fullscreen changed";
/// Исход выдачи пропуска при замене прежнего сеанса.
pub(super) const REPLACED_PREVIOUS_GRANT: &str = "grant=ReplacedPrevious";

/// Метки журнала страницы (договор со страницей).
pub(super) const PAGE_STATE_MARKER: &str = "page-reload: state ";
pub(super) const PAGE_LOAD_MARKER: &str = "page-reload: load ";
/// Вид документа «плеер внутри iframe» (метка `view` у документа рамки).
const FRAME_INNER_VIEW: &str = "frame-inner";

/// Конец сеанса не раньше, чем за столько после ухода страницы (срок экстеншена по умолчанию — 10 с).
pub(super) const MIN_WAIT_BEFORE_END: Duration = Duration::from_secs(9);
/// Конец «сразу» (другой сайт) не позже: заведомо меньше срока ожидания.
pub(super) const MAX_IMMEDIATE_END: Duration = Duration::from_secs(6);
/// Сколько новых кадров рендера после замены доказывают, что поток идёт.
pub(super) const MIN_FRAMES_AFTER_REPLACEMENT: u64 = 30;
/// Сколько настоящих кадров должен сам показать браузер, играя видео другого сайта.
pub(super) const MIN_BROWSER_FRAMES: u64 = 30;
/// Самое короткое окно жизни второго документа, на котором счётчик браузера что-то доказывает.
pub(super) const MIN_SECOND_DOCUMENT_WINDOW_S: f64 = 6.0;

/// Что строка обязана доказать.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) enum Expectation {
    /// Новый документ вкладки с видео того же сайта: замена в том же окне (`grant=ReplacedPrevious`).
    Replacement,
    /// Видео нет: сеанс кончается только после срока ожидания экстеншена.
    EndsAfterWait,
    /// Другой сайт: сеанс кончается сразу, видео играет браузер.
    EndsAtOnce,
}

/// Что должен показать второй документ строки: вид и номер серии (для проверки, что ушли именно туда).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) struct ExpectedDocument {
    pub(super) view: &'static str,
    pub(super) episode: &'static str,
}

/// Строки журнала плеера по смыслу, в порядке появления.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub(super) struct PlayerJournal {
    pub(super) window_created: Vec<String>,
    pub(super) process_starts: Vec<String>,
    pub(super) session_opened: Vec<String>,
    pub(super) fullscreen_changes: Vec<String>,
}

/// Строка «документ загрузился» страницы.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(super) struct DocumentLoad {
    pub(super) document: String,
    pub(super) second_load: bool,
    pub(super) view: String,
    pub(super) episode: String,
    pub(super) origin: String,
}

/// Строка состояния видео страницы (раз в секунду).
#[derive(Debug, Clone, PartialEq)]
pub(super) struct DocumentState {
    pub(super) document: String,
    pub(super) second_load: bool,
    pub(super) current_time_s: f64,
    pub(super) real_frames: u64,
    pub(super) probe_available: bool,
}

/// Первый признак конца сеанса после ухода страницы и через сколько он появился.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(super) struct EndSignal {
    pub(super) line: String,
    pub(super) after_navigation: Duration,
}

/// Что стенд увидел у старого сеанса после ухода страницы (строки без замены).
#[derive(Debug)]
pub(super) struct EndObservation {
    pub(super) first_signal: Result<EndSignal, String>,
    pub(super) notice: Result<EndNotice, String>,
    pub(super) window_closed: Result<(), String>,
}

/// Всё, что собрал прогон строки: вход приговора.
#[derive(Debug)]
pub(super) struct ReloadEvidence {
    /// Шаги прогона, которые не удались (передача, ожидание замены): по одному тексту на шаг.
    pub(super) failed_steps: Vec<String>,
    pub(super) journal: PlayerJournal,
    pub(super) loads: Vec<DocumentLoad>,
    pub(super) states: Vec<DocumentState>,
    /// Прирост показанных кадров после замены (`None` — строка замены не ждала).
    pub(super) frame_growth: Option<Result<u64, String>>,
    /// Конец старого сеанса (`None` — строка его не ждала).
    pub(super) end: Option<EndObservation>,
    pub(super) host: HostLogEvidence,
    pub(super) forbidden_player_lines: Vec<String>,
    /// Хвост журнала страницы: главная улика при провале.
    pub(super) page_log: String,
}

/// Значение поля `name=` в строке журнала (левая граница — пробел).
pub(super) fn field<'line>(line: &'line str, name: &str) -> Option<&'line str> {
    let marker = format!(" {name}=");
    let start = line.find(&marker)? + marker.len();
    line[start..].split_whitespace().next()
}

/// Разбор строки загрузки документа; чужая или битая строка — `None`.
pub(super) fn parse_load(line: &str) -> Option<DocumentLoad> {
    if !line.contains(PAGE_LOAD_MARKER) {
        return None;
    }
    Some(DocumentLoad {
        document: field(line, "doc")?.to_owned(),
        second_load: field(line, "second_load")? == "true",
        view: field(line, "view")?.to_owned(),
        episode: field(line, "ep")?.to_owned(),
        origin: field(line, "origin")?.to_owned(),
    })
}

/// Разбор строки состояния видео; чужая или битая строка — `None`.
pub(super) fn parse_state(line: &str) -> Option<DocumentState> {
    if !line.contains(PAGE_STATE_MARKER) {
        return None;
    }
    Some(DocumentState {
        document: field(line, "doc")?.to_owned(),
        second_load: field(line, "second_load")? == "true",
        current_time_s: field(line, "ct")?.parse().ok()?,
        real_frames: field(line, "real_frames")?.parse().ok()?,
        probe_available: field(line, "probe")? == "true",
    })
}

/// Приговор строки.
pub(super) fn judge(
    expectation: Expectation,
    expected_second: &[ExpectedDocument],
    origin_changes: bool,
    evidence: &ReloadEvidence,
) -> Verdict {
    let mut failures: Vec<String> = evidence.failed_steps.clone();
    failures.extend(host_failures(&evidence.host));
    failures.extend(
        unexpected_player_lines(&evidence.forbidden_player_lines)
            .map(|line| format!("forbidden player line: {line}")),
    );
    failures.extend(window_failures(&evidence.journal));
    failures.extend(second_documents_failures(
        &evidence.loads,
        expected_second,
        origin_changes,
    ));
    let summary = match expectation {
        Expectation::Replacement => {
            failures.extend(replacement_failures(evidence));
            replacement_summary(evidence)
        }
        Expectation::EndsAfterWait => {
            failures.extend(old_end_failures(evidence, WhenEnds::AfterWait));
            end_summary(evidence)
        }
        Expectation::EndsAtOnce => {
            failures.extend(old_end_failures(evidence, WhenEnds::AtOnce));
            failures.extend(browser_plays_by_itself_failures(&evidence.states));
            end_summary(evidence)
        }
    };
    let summary = if failures.is_empty() {
        summary
    } else {
        format!("{summary}\n{}", evidence.page_log)
    };
    Verdict::from_failures(&failures, summary)
}

/// Окно и процесс плеера создались ровно по разу: новое окно на переходе — тот самый сбой.
fn window_failures(journal: &PlayerJournal) -> Vec<String> {
    let mut failures = Vec::new();
    if journal.window_created.len() != 1 {
        failures.push(format!(
            "the player window was created {} time(s), expected exactly 1: {:?}",
            journal.window_created.len(),
            journal.window_created
        ));
    }
    if journal.process_starts.len() != 1 {
        failures.push(format!(
            "the player process started {} time(s), expected exactly 1",
            journal.process_starts.len()
        ));
    }
    failures
}

/// Страница ушла именно туда, куда обещала строка: по одному второму документу нужного вида и серии,
/// и origin сменился только там, где сайт другой. Без этого «ничего не заменилось» могло значить
/// «страница вообще не ушла».
fn second_documents_failures(
    loads: &[DocumentLoad],
    expected: &[ExpectedDocument],
    origin_changes: bool,
) -> Vec<String> {
    let mut failures = Vec::new();
    let mut seen: Vec<(String, String)> = loads
        .iter()
        .filter(|load| load.second_load)
        .map(|load| (load.view.clone(), load.episode.clone()))
        .collect();
    let mut wanted: Vec<(String, String)> = expected
        .iter()
        .map(|document| (document.view.to_owned(), document.episode.to_owned()))
        .collect();
    seen.sort();
    wanted.sort();
    if seen != wanted {
        failures.push(format!(
            "the second document(s) were {seen:?}, expected {wanted:?} (view, episode)"
        ));
    }
    // Адрес вкладки судится по верхнему документу, а не по рамке внутри него.
    let top_origin = |second_load: bool| {
        loads
            .iter()
            .find(|load| load.second_load == second_load && load.view != FRAME_INNER_VIEW)
            .map(|load| load.origin.as_str())
    };
    match (top_origin(false), top_origin(true)) {
        (Some(first), Some(second)) if (first != second) != origin_changes => {
            failures.push(format!(
                "the tab origin went {first} -> {second}, expected it to {}",
                if origin_changes { "change" } else { "stay" }
            ));
        }
        (Some(_), Some(_)) => {}
        _ => failures.push("the page never reported both documents of the tab".to_owned()),
    }
    failures
}

/// Замена: один раз `ReplacedPrevious`, ровно два сеанса, полный экран сохранён, кадры растут,
/// браузер второй документ не декодирует.
fn replacement_failures(evidence: &ReloadEvidence) -> Vec<String> {
    let journal = &evidence.journal;
    let mut failures = Vec::new();
    let replaced = journal
        .session_opened
        .iter()
        .filter(|line| line.contains(REPLACED_PREVIOUS_GRANT))
        .count();
    if replaced != 1 {
        failures.push(format!(
            "expected exactly one `{REPLACED_PREVIOUS_GRANT}` session, got {replaced}: {:?}",
            journal.session_opened
        ));
    }
    if journal.session_opened.len() != 2 {
        failures.push(format!(
            "expected exactly 2 sessions (the first and its replacement), got {}: {:?}",
            journal.session_opened.len(),
            journal.session_opened
        ));
    }
    failures.extend(fullscreen_failures(&journal.fullscreen_changes));
    match &evidence.frame_growth {
        Some(Ok(gained)) if *gained >= MIN_FRAMES_AFTER_REPLACEMENT => {}
        Some(Ok(gained)) => failures.push(format!(
            "after the replacement the player presented only {gained} new frames, expected {MIN_FRAMES_AFTER_REPLACEMENT}"
        )),
        Some(Err(reason)) => failures.push(format!("no frames after the replacement: {reason}")),
        None => failures.push("the row never waited for frames after the replacement".to_owned()),
    }
    failures.extend(second_document_not_decoded_failures(&evidence.states));
    failures
}

/// Полный экран был включён (из запомненного) и ни разу не выключен.
pub(super) fn fullscreen_failures(changes: &[String]) -> Vec<String> {
    let values: Vec<Option<&str>> = changes
        .iter()
        .map(|line| field(line, "fullscreen"))
        .collect();
    let mut failures = Vec::new();
    if !values.contains(&Some("true")) {
        failures.push(format!("the window never entered fullscreen: {changes:?}"));
    }
    if values.contains(&Some("false")) {
        failures.push(format!("the window left fullscreen: {changes:?}"));
    }
    failures
}

/// Второй документ под тенью почти не декодирует: настоящих кадров не больше потолка `capture`.
pub(super) fn second_document_not_decoded_failures(states: &[DocumentState]) -> Vec<String> {
    let second: Vec<&DocumentState> = states.iter().filter(|state| state.second_load).collect();
    let (Some(first), Some(last)) = (second.first(), second.last()) else {
        return vec!["the second document never reported its video state".to_owned()];
    };
    if second.iter().any(|state| state.document != first.document) {
        return vec![
            "more than one second document reported: the page was opened twice".to_owned(),
        ];
    }
    if !last.probe_available {
        return vec!["the second document could not read the real frame counter".to_owned()];
    }
    // Одна строка в секунду: окно — число строк. Кадры считаются от загрузки документа.
    let window_s = second.len() as f64;
    if window_s < MIN_SECOND_DOCUMENT_WINDOW_S {
        return vec![format!(
            "the second document lived {window_s:.0} s, too short to judge its decoder (need {MIN_SECOND_DOCUMENT_WINDOW_S} s)"
        )];
    }
    let allowed = max_real_frames(window_s);
    if last.real_frames as f64 > allowed {
        return vec![format!(
            "the second document decoded {} frames in {window_s:.0} s (allowed {allowed:.0}): the browser plays it by itself",
            last.real_frames
        )];
    }
    Vec::new()
}

/// Другой сайт: видео второго документа играет сам браузер (не под тенью): настоящие кадры растут.
fn browser_plays_by_itself_failures(states: &[DocumentState]) -> Vec<String> {
    let Some(last) = states.iter().rev().find(|state| state.second_load) else {
        return vec!["the other-site document never reported its video state".to_owned()];
    };
    if !last.probe_available {
        return vec!["the other-site document could not read the real frame counter".to_owned()];
    }
    if last.real_frames < MIN_BROWSER_FRAMES || last.current_time_s <= 0.0 {
        return vec![format!(
            "the browser did not play the other-site video itself: {} real frames, clock {:.1} s (expected {MIN_BROWSER_FRAMES}+)",
            last.real_frames, last.current_time_s
        )];
    }
    Vec::new()
}

/// Когда старый сеанс обязан дать первый признак конца.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum WhenEnds {
    /// Не раньше срока ожидания экстеншена.
    AfterWait,
    /// Сразу, заведомо раньше срока ожидания.
    AtOnce,
}

/// Старый сеанс кончился без замены: нет `ReplacedPrevious`, один сеанс, нужные сроки и слова.
fn old_end_failures(evidence: &ReloadEvidence, when: WhenEnds) -> Vec<String> {
    let mut failures = Vec::new();
    let opened = &evidence.journal.session_opened;
    if opened.len() != 1
        || opened
            .iter()
            .any(|line| line.contains(REPLACED_PREVIOUS_GRANT))
    {
        failures.push(format!(
            "expected exactly one session and no `{REPLACED_PREVIOUS_GRANT}`, got {opened:?}"
        ));
    }
    let Some(end) = &evidence.end else {
        failures.push("the row never waited for the end of the old session".to_owned());
        return failures;
    };
    match &end.first_signal {
        Ok(signal) => failures.extend(end_timing_failures(signal, when)),
        Err(reason) => failures.push(reason.clone()),
    }
    match &end.notice {
        Ok(found) => failures.extend(tab_gone_notice_failures(found)),
        Err(reason) => failures.push(reason.clone()),
    }
    if let Err(reason) = &end.window_closed {
        failures.push(reason.clone());
    }
    failures
}

/// Срок первого признака конца относительно ухода страницы.
fn end_timing_failures(signal: &EndSignal, when: WhenEnds) -> Vec<String> {
    let elapsed = signal.after_navigation;
    match when {
        WhenEnds::AfterWait if elapsed < MIN_WAIT_BEFORE_END => vec![format!(
            "the old session ended {:.1} s after the page left, too early: the extension must wait {} s for a new video ({})",
            elapsed.as_secs_f64(),
            MIN_WAIT_BEFORE_END.as_secs(),
            signal.line.trim()
        )],
        WhenEnds::AtOnce if elapsed > MAX_IMMEDIATE_END => vec![format!(
            "the old session ended {:.1} s after the page left for another site, too late: it must not wait ({})",
            elapsed.as_secs_f64(),
            signal.line.trim()
        )],
        _ => Vec::new(),
    }
}

/// Вкладка ушла: причина `tab_gone`, доигрывание скачанного кончилось (`finished`).
pub(super) fn tab_gone_notice_failures(found: &EndNotice) -> Vec<String> {
    if found.cause == "tab_gone" && found.notice == "finished" {
        Vec::new()
    } else {
        vec![format!(
            "expected the source tab closed notice (notice=finished, cause=tab_gone), got: {}",
            found.line.trim()
        )]
    }
}

fn replacement_summary(evidence: &ReloadEvidence) -> String {
    let gained = match &evidence.frame_growth {
        Some(Ok(gained)) => format!("{gained} new frames after the replacement"),
        Some(Err(reason)) => reason.clone(),
        None => "no frame growth observed".to_owned(),
    };
    format!(
        "{} sessions, fullscreen changes {:?}, {gained}",
        evidence.journal.session_opened.len(),
        evidence.journal.fullscreen_changes.len()
    )
}

fn end_summary(evidence: &ReloadEvidence) -> String {
    match &evidence.end {
        Some(EndObservation {
            first_signal: Ok(signal),
            notice: Ok(found),
            ..
        }) => format!(
            "first end signal {:.1} s after the page left; {}",
            signal.after_navigation.as_secs_f64(),
            found.line.trim()
        ),
        Some(_) => "end observed only partly".to_owned(),
        None => "no end observed".to_owned(),
    }
}

#[cfg(test)]
mod tests;
