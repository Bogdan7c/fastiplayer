//! Чистый приговор строк «окно идёт за лентой коротких видео» (сессия 16b2, решения 354–360): по собранным
//! уликам решает, переехало ли окно плеера на следующий ролик, когда человек пролистал ленту НАСТОЯЩИМ
//! колесом мыши, или (в отрицательных строках) осталось на своём ролике. Ввода-вывода здесь нет, поэтому
//! каждое нарушение проверяется юнит-тестом.
//!
//! Что считается результатом, а не вызовом (правило тестов проекта):
//!  * `hand_off_tab` отдал ролик 1 (`started`), посредник выдал ровно ОДИН пропуск (одно окно на всю ленту);
//!  * колесо ДОШЛО до страницы (`wheel trusted=true`), и страница сама запустила следующий ролик;
//!  * плеер загрузил новое поколение потока на каждом переезде (строка журнала приходит сразу), и номера
//!    поколений идут подряд `1, 2, 3`: ни лишней, ни пропущенной загрузки;
//!  * окно показало ролик НУЖНОГО размера кадра (ширина 640 -> 512 -> 384: это три разных ролика);
//!  * кадры плеера растут после каждого переезда, окно не показывало зрителю сообщение о конце сеанса;
//!  * в отрицательных строках: раздражитель БЫЛ (превью заиграло, главный ролик ушёл с экрана и не остановлен),
//!    а поколение осталось одно, ролик на месте, кадры растут. Без доказательства раздражителя строка
//!    прошла бы вхолостую, поэтому его отсутствие — провал.

use super::super::super::evidence::{host_failures, page_failures, unexpected_player_lines};
use super::super::super::outcome::Verdict;
use crate::browsers::HandOffOutcome;
use crate::observe::{FrameGrowth, HostLogEvidence};
use crate::site::{PageReport, SiteError};

/// На сколько должен вырасти показ кадров плеера между двумя сводками после переезда или раздражителя:
/// 60 кадров — 2,4 с видео при 25 к/с, «играет», а не «один кадр».
pub(super) const MIN_FRAMES_GAINED: u64 = 60;

/// Что обязано случиться с окном плеера.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) enum ExpectedWindow {
    /// Окно идёт за лентой: после каждой прокрутки оно показывает следующий ролик. Ширины кадра роликов по
    /// порядку (первый — отданный; прокруток на одну меньше, чем роликов).
    FollowsFeed { reel_stream_widths: &'static [u64] },
    /// Окно остаётся на отданном ролике (ширина кадра его), новых загрузок нет.
    StaysOnFirstReel { stream_width: u64 },
}

/// Итог одной прокрутки колесом (по данным робота).
#[derive(Debug)]
pub(super) struct ScrollRecord {
    /// Сколько настоящих событий колеса (`wheel trusted=true`) страница получила за эту прокрутку.
    pub trusted_wheel_events: usize,
    /// Страница сама запустила следующий ролик (`reel-started`) или причина, почему нет.
    pub page_started_next_reel: Result<(), String>,
    /// Плеер загрузил новое поколение потока или причина, почему нет.
    pub player_loaded_new_generation: Result<(), String>,
    /// Рост показа кадров между двумя сводками ПОСЛЕ переезда.
    pub frame_growth: Result<FrameGrowth, String>,
}

/// Улики отрицательной строки: раздражитель был, а окно осталось.
#[derive(Debug)]
pub(super) struct StayRecord {
    /// Раздражитель состоялся (страница это записала) или причина, почему нет.
    pub stimulus: Result<(), String>,
    /// Рост показа кадров между двумя сводками ПОСЛЕ раздражителя (за это время сводка успела бы
    /// показать лишнюю загрузку).
    pub frame_growth: Result<FrameGrowth, String>,
}

/// Всё, что собрал прогон: вход чистого приговора.
pub(super) struct FeedEvidence {
    /// Идентификатор страницы (отчёт должен быть от неё).
    pub page_id: &'static str,
    /// Исход передачи ролика 1.
    pub hand_off: Result<HandOffOutcome, String>,
    /// Отметки посредника за весь прогон.
    pub host: HostLogEvidence,
    /// Кадры плеера дошли до показа на отданном ролике.
    pub first_reel_frames: Result<(), String>,
    /// Прокрутки колесом по порядку (в строках «окно идёт за лентой»).
    pub scrolls: Vec<ScrollRecord>,
    /// Отрицательная строка: раздражитель и его последствия.
    pub stay: Option<StayRecord>,
    /// Номера поколений из строк `Browser stream generation loaded` по порядку.
    pub generations: Vec<u64>,
    /// Ширины кадра из строк «пул поверхностей перестроен под поток» по порядку (какой ролик показывало окно).
    pub shown_stream_widths: Vec<u64>,
    /// Строки «окно показало сообщение о конце сеанса» (в этих строках их быть не должно).
    pub end_notice_lines: Vec<String>,
    /// Запрещённые строки журнала плеера (с шумом).
    pub forbidden_player_lines: Vec<String>,
    /// Отчёт страницы или ошибка ожидания.
    pub page_report: Result<PageReport, SiteError>,
    /// Хвост журнала страницы (прикладывается при провале).
    pub page_log: String,
}

/// Убирает подряд идущие повторы: плеер пишет строку качества дважды (вес потока «неизвестен», потом
/// известен), а ролик при этом один.
fn without_consecutive_repeats(values: &[u64]) -> Vec<u64> {
    let mut result: Vec<u64> = Vec::new();
    for value in values {
        if result.last() != Some(value) {
            result.push(*value);
        }
    }
    result
}

/// Метрика отчёта страницы; нет метрики — это провал, а не нуль.
fn metric(report: &PageReport, name: &str, failures: &mut Vec<String>) -> Option<f64> {
    let value = report.metrics.get(name).copied();
    if value.is_none() {
        failures.push(format!("the page report has no metric '{name}'"));
    }
    value
}

/// Проверка роста кадров: рост обязан быть и быть достаточным.
fn growth_failure(what: &str, growth: &Result<FrameGrowth, String>) -> Option<String> {
    match growth {
        Ok(growth) if growth.frames_gained() >= MIN_FRAMES_GAINED => None,
        Ok(growth) => Some(format!(
            "{what}: player frames grew by only {} (need {MIN_FRAMES_GAINED})",
            growth.frames_gained()
        )),
        Err(message) => Some(format!("{what}: player frames did not grow: {message}")),
    }
}

/// Чистое решение «окно плеера повело себя как надо за лентой».
pub(super) fn judge_feed(expected: ExpectedWindow, evidence: &FeedEvidence) -> Verdict {
    let mut failures: Vec<String> = Vec::new();
    common_failures(evidence, &mut failures);
    match expected {
        ExpectedWindow::FollowsFeed { reel_stream_widths } => {
            follow_failures(reel_stream_widths, evidence, &mut failures);
        }
        ExpectedWindow::StaysOnFirstReel { stream_width } => {
            stay_failures(stream_width, evidence, &mut failures);
        }
    }
    failures.extend(page_failures(&evidence.page_report, evidence.page_id));
    let summary = format!(
        "generations {:?}, stream widths shown {:?}",
        evidence.generations,
        without_consecutive_repeats(&evidence.shown_stream_widths)
    );
    // Журнал страницы прикладывается только к провалу: в успехе он шум.
    let summary = if failures.is_empty() {
        summary
    } else {
        format!("{summary}\n{}", evidence.page_log)
    };
    Verdict::from_failures(&failures, summary)
}

/// Общее для всех строк: передача, один пропуск, ни одного сообщения о конце, отданный ролик показан.
fn common_failures(evidence: &FeedEvidence, failures: &mut Vec<String>) {
    match &evidence.hand_off {
        Ok(HandOffOutcome::Started) => {}
        Ok(other) => failures.push(format!(
            "hand_off_tab of reel 1: {other:?}, expected started"
        )),
        Err(error) => failures.push(format!("hand_off_tab of reel 1: {error}")),
    }
    failures.extend(host_failures(&evidence.host));
    if evidence.host.session_grants != 1 {
        failures.push(format!(
            "the player granted {} session(s) in the whole run, expected exactly one (one window for the whole feed)",
            evidence.host.session_grants
        ));
    }
    if !evidence.end_notice_lines.is_empty() {
        failures.push(format!(
            "the window showed a session end notice, the session did not survive the feed: {}",
            evidence.end_notice_lines.join(" | ")
        ));
    }
    if let Err(message) = &evidence.first_reel_frames {
        failures.push(format!("reel 1 never reached the player window: {message}"));
    }
    failures.extend(
        unexpected_player_lines(&evidence.forbidden_player_lines)
            .map(|line| format!("forbidden player line: {line}")),
    );
}

/// Строки «окно идёт за лентой».
fn follow_failures(
    reel_stream_widths: &[u64],
    evidence: &FeedEvidence,
    failures: &mut Vec<String>,
) {
    let expected_scrolls = reel_stream_widths.len().saturating_sub(1);
    if evidence.scrolls.len() != expected_scrolls {
        failures.push(format!(
            "the robot made {} scroll(s), the row needs {expected_scrolls}",
            evidence.scrolls.len()
        ));
    }
    for (index, scroll) in evidence.scrolls.iter().enumerate() {
        let label = format!("scroll {} (reel {} -> {})", index + 1, index + 1, index + 2);
        // Отдельная причина: колесо вообще не дошло до страницы (стенд, а не продукт, не крутит).
        if scroll.trusted_wheel_events == 0 {
            failures.push(format!(
                "{label}: the real mouse wheel never reached the page (no 'wheel trusted=true' in the page log)"
            ));
            continue;
        }
        if let Err(message) = &scroll.page_started_next_reel {
            failures.push(format!(
                "{label}: the page did not start the next reel: {message}"
            ));
            continue;
        }
        if let Err(message) = &scroll.player_loaded_new_generation {
            failures.push(format!(
                "{label}: the window did not relocate to the next reel: {message}"
            ));
        }
        failures.extend(growth_failure(&label, &scroll.frame_growth));
    }
    let expected_generations: Vec<u64> = (1..=reel_stream_widths.len() as u64).collect();
    if evidence.generations != expected_generations {
        failures.push(format!(
            "the player loaded generations {:?}, expected {expected_generations:?} (one load per reel, no extra and none missing)",
            evidence.generations
        ));
    }
    let shown = without_consecutive_repeats(&evidence.shown_stream_widths);
    if shown != reel_stream_widths {
        failures.push(format!(
            "the window showed stream widths {shown:?}, expected {reel_stream_widths:?} (reel 1, reel 2, ...)"
        ));
    }
    page_metric_failures(evidence, reel_stream_widths.len(), failures);
}

/// Метрики страницы строк «окно идёт за лентой»: она сама запустила столько роликов, сколько надо.
fn page_metric_failures(evidence: &FeedEvidence, reels: usize, failures: &mut Vec<String>) {
    let Ok(report) = &evidence.page_report else {
        return;
    };
    if let Some(started) = metric(report, "reels_started", failures)
        && (started - reels as f64).abs() > f64::EPSILON
    {
        failures.push(format!(
            "the page started {started} reel(s), expected {reels}"
        ));
    }
    if metric(report, "wheel_events_trusted", failures).is_some_and(|events| events < 1.0) {
        failures.push("the page saw no trusted wheel events".to_owned());
    }
}

/// Отрицательные строки: раздражитель был, а окно осталось на своём ролике.
fn stay_failures(stream_width: u64, evidence: &FeedEvidence, failures: &mut Vec<String>) {
    let Some(stay) = &evidence.stay else {
        failures.push("the row recorded no stimulus at all".to_owned());
        return;
    };
    if let Err(message) = &stay.stimulus {
        failures.push(format!(
            "the stimulus did not happen, so the row proves nothing: {message}"
        ));
    }
    failures.extend(growth_failure("after the stimulus", &stay.frame_growth));
    if evidence.generations != [1] {
        failures.push(format!(
            "the player loaded generations {:?}, expected only [1]: the window relocated although the page did not leave our video",
            evidence.generations
        ));
    }
    let shown = without_consecutive_repeats(&evidence.shown_stream_widths);
    if shown != [stream_width] {
        failures.push(format!(
            "the window showed stream widths {shown:?}, expected only [{stream_width}]"
        ));
    }
    if let Ok(report) = &evidence.page_report {
        if metric(report, "main_paused", failures).is_some_and(|paused| paused > 0.0) {
            failures.push(
                "the page sees our video paused: the row is not the 'page did not stop it' case"
                    .to_owned(),
            );
        }
        if metric(report, "preview_playing", failures).is_some_and(|playing| playing < 1.0) {
            failures.push(
                "the preview was not playing at the end: the stimulus ended early".to_owned(),
            );
        }
    }
}

#[cfg(test)]
mod tests;
