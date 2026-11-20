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
//!  * окно не ждало данных у начала ролика до истечения срока (строка журнала плеера «у цели данных нет»):
//!    зритель в это время смотрит на прежний ролик, а новый потом встаёт с середины (находка живой проверки
//!    Instagram «нет мгновенного воспроизведения после прокрутки»);
//!  * в строке «страница качает дольше, чем помнит мост» докачка БЫЛА (страница её записала): без неё строка
//!    прошла бы вхолостую;
//!  * в отрицательных строках: раздражитель БЫЛ (превью заиграло, главный ролик ушёл с экрана и не остановлен),
//!    а поколение осталось одно, ролик на месте, кадры растут. Без доказательства раздражителя строка
//!    прошла бы вхолостую, поэтому его отсутствие — провал.

use super::super::super::evidence::{host_failures, page_failures, unexpected_player_lines};
use super::super::super::outcome::Verdict;
use super::back_judge::{self, BackRecord, BackStart};
use super::edge_judge::{self, EdgeRecord};
use super::history_back_judge::{self, HistoryBackRecord};
use super::switch_judge;
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
    /// Быстрая серия прокруток (16b3): после первой спокойной прокрутки страница пролистала ещё несколько
    /// роликов подряд. Окно кончает на ПОСЛЕДНЕМ ролике (ширины кадра всех роликов по порядку).
    EndsOnLastReel { reel_stream_widths: &'static [u64] },
    /// Уход из ленты в другой раздел (16b3): честный конец «Source tab closed», превью нового раздела не
    /// принято. `reel_stream_widths` — ролики, которые окно успело показать до ухода.
    EndsHonestly {
        reel_stream_widths: &'static [u64],
        window: edge_judge::WindowAfterLeave,
        /// Как скоро после смены адреса окно обязано кончить сеанс (решение 381: уход из Shorts).
        end_deadline: edge_judge::EndAfterAddressLeave,
    },
    /// Конец ленты без ухода (решение 356): окно ждёт, не кончает сеанс и не берёт новых роликов.
    /// `reel_stream_widths` — ролики, которые окно показало до конца ленты.
    WaitsAtFeedEnd { reel_stream_widths: &'static [u64] },
    /// Прокрутка НАЗАД (16b3b): после прокрутки вперёд страница вернулась к уже виденному ролику. Окно идёт
    /// за лентой в обе стороны (`reel_stream_widths` — `640, 512, 640`: первый ролик показан дважды), а
    /// секунду старта вернувшегося поколения требует `start`.
    ReturnsToEarlierReel {
        reel_stream_widths: &'static [u64],
        start: BackStart,
    },
    /// «Назад» в истории после ухода из ленты (16b3b, решения 375-376): первый сеанс кончился честно, окно
    /// стоит, а вернувшийся ролик подхвачен вторым сеансом в ТОМ ЖЕ окне.
    PicksUpAfterHistoryBack,
}

/// Можно ли окну показывать сообщение о конце сеанса в этой строке.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) enum EndNoticePolicy {
    /// Нельзя: сеанс обязан пережить всю ленту.
    Forbidden,
    /// Нужно (проверяется отдельно, `edge_judge`): строка про честный конец.
    Expected,
}

/// Можно ли новому поколению ждать данных у цели (строка плеера «у цели данных нет»).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum TargetWaitPolicy {
    /// Нельзя: ролик обязан заиграть сразу (все строки, кроме замера).
    Forbidden,
    /// Можно: ожидание — измеряемый результат строки.
    Allowed,
}

/// Судится ли рост кадров после переезда на ролик, к которому ведёт прокрутка.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) enum FrameGrowthCheck {
    /// Да: после переезда окно обязано играть новый ролик (все спокойные строки).
    Required,
    /// Нет: страница убрала этот элемент сразу после переезда (находка F2), и от окна ждут лишь, что оно
    /// дождётся следующего ролика; рост кадров судится на следующем ролике.
    NotRequired,
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
    /// Судится ли `frame_growth`.
    pub frame_growth_check: FrameGrowthCheck,
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
    /// Докачка страницы сверх памяти моста перед прокруткой: состоялась или причина, почему нет (`None` —
    /// строка её не просит).
    pub page_download: Option<Result<(), String>>,
    /// Улики края ленты (16b3): раздражитель страницы и что с ним стало (`None` — строка не про края).
    pub edge: Option<EdgeRecord>,
    /// Улики прокрутки назад (16b3b; `None` — строка не про возврат).
    pub back: Option<BackRecord>,
    /// Улики «Назад» в истории после ухода (16b3b; `None` — строка не про это).
    pub history_back: Option<HistoryBackRecord>,
    /// Строки журнала плеера «новое поколение не дождалось данных у цели» (их быть не должно).
    pub target_wait_lines: Vec<String>,
    /// Строки журнала плеера «смена медиа в окне» (решение 380): по ним судится «первый кадр без пустого».
    pub media_switch_lines: Vec<String>,
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
pub(super) fn without_consecutive_repeats(values: &[u64]) -> Vec<u64> {
    let mut result: Vec<u64> = Vec::new();
    for value in values {
        if result.last() != Some(value) {
            result.push(*value);
        }
    }
    result
}

/// Метрика отчёта страницы; нет метрики — это провал, а не нуль.
pub(super) fn metric(report: &PageReport, name: &str, failures: &mut Vec<String>) -> Option<f64> {
    let value = report.metrics.get(name).copied();
    if value.is_none() {
        failures.push(format!("the page report has no metric '{name}'"));
    }
    value
}

/// Проверка роста кадров: рост обязан быть и быть достаточным.
pub(super) fn growth_failure(what: &str, growth: &Result<FrameGrowth, String>) -> Option<String> {
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
    let end_notices = match expected {
        ExpectedWindow::EndsHonestly { .. } | ExpectedWindow::PicksUpAfterHistoryBack => {
            EndNoticePolicy::Expected
        }
        _ => EndNoticePolicy::Forbidden,
    };
    let target_wait = match expected {
        // Строка-замер (решение 369) не проваливается от ожидания данных у цели: ожидание — её результат.
        ExpectedWindow::ReturnsToEarlierReel {
            start: BackStart::Measured | BackStart::MeasuredFar,
            ..
        } => TargetWaitPolicy::Allowed,
        _ => TargetWaitPolicy::Forbidden,
    };
    let expected_grants = match expected {
        // Подхват после «Назад» — второй сеанс: посредник выдаёт пропуск дважды (окно при этом одно).
        ExpectedWindow::PicksUpAfterHistoryBack => 2_u32,
        _ => 1,
    };
    common_failures(
        evidence,
        end_notices,
        target_wait,
        expected_grants,
        &mut failures,
    );
    match expected {
        ExpectedWindow::FollowsFeed { reel_stream_widths } => {
            follow_failures(reel_stream_widths, evidence, &mut failures);
        }
        ExpectedWindow::StaysOnFirstReel { stream_width } => {
            stay_failures(stream_width, evidence, &mut failures);
        }
        ExpectedWindow::EndsOnLastReel { reel_stream_widths } => {
            edge_judge::last_reel_failures(reel_stream_widths, evidence, &mut failures);
        }
        ExpectedWindow::EndsHonestly {
            reel_stream_widths,
            window,
            end_deadline,
        } => {
            edge_judge::honest_end_failures(
                reel_stream_widths,
                window,
                end_deadline,
                evidence,
                &mut failures,
            );
        }
        ExpectedWindow::WaitsAtFeedEnd { reel_stream_widths } => {
            edge_judge::waits_failures(reel_stream_widths, evidence, &mut failures);
        }
        ExpectedWindow::ReturnsToEarlierReel {
            reel_stream_widths,
            start,
        } => {
            back_judge::return_failures(reel_stream_widths, start, evidence, &mut failures);
        }
        ExpectedWindow::PicksUpAfterHistoryBack => {
            history_back_judge::pickup_failures(evidence, &mut failures);
        }
    }
    media_switch_failures_for(expected, evidence, &mut failures);
    failures.extend(page_failures(&evidence.page_report, evidence.page_id));
    let summary = format!(
        "generations {:?}, stream widths shown {:?}{}",
        evidence.generations,
        without_consecutive_repeats(&evidence.shown_stream_widths),
        burst_gaps_note(evidence)
    );
    let summary = format!("{summary}{}", back_judge::measurement_note(evidence));
    // Журнал страницы прикладывается только к провалу: в успехе он шум.
    let summary = if failures.is_empty() {
        summary
    } else {
        format!("{summary}\n{}", evidence.page_log)
    };
    Verdict::from_failures(&failures, summary)
}

/// Правило решения 380 для строк, где окно меняет ролик (загружено больше одного поколения). Строка
/// «окно осталось на первом ролике» смен не имеет и не судится. Сколько переездов ждать — по числу
/// загруженных поколений без первого.
fn media_switch_failures_for(
    expected: ExpectedWindow,
    evidence: &FeedEvidence,
    failures: &mut Vec<String>,
) {
    if matches!(expected, ExpectedWindow::StaysOnFirstReel { .. }) {
        return;
    }
    let expected_switches = evidence.generations.len().saturating_sub(1);
    // Строка без переездов тоже не судится: смен в ней нет.
    if expected_switches == 0 {
        return;
    }
    switch_judge::media_switch_failures(&evidence.media_switch_lines, expected_switches, failures);
}

/// Промежутки между шагами быстрой серии, как их насчитала страница (для отчёта; пусто, если серии не было).
fn burst_gaps_note(evidence: &FeedEvidence) -> String {
    let Ok(report) = &evidence.page_report else {
        return String::new();
    };
    match (
        report.metrics.get("burst_gap_min_ms"),
        report.metrics.get("burst_gap_max_ms"),
    ) {
        (Some(min), Some(max)) => format!(", burst step gaps {min}..{max} ms"),
        _ => String::new(),
    }
}

/// Общее для всех строк: передача, один пропуск, ни одного сообщения о конце, отданный ролик показан.
fn common_failures(
    evidence: &FeedEvidence,
    end_notices: EndNoticePolicy,
    target_wait: TargetWaitPolicy,
    expected_grants: u32,
    failures: &mut Vec<String>,
) {
    match &evidence.hand_off {
        Ok(HandOffOutcome::Started) => {}
        Ok(other) => failures.push(format!(
            "hand_off_tab of reel 1: {other:?}, expected started"
        )),
        Err(error) => failures.push(format!("hand_off_tab of reel 1: {error}")),
    }
    failures.extend(host_failures(&evidence.host));
    if evidence.host.session_grants != expected_grants {
        failures.push(format!(
            "the player granted {} session(s) in the whole run, expected exactly {} (one window for the whole feed)",
            evidence.host.session_grants,
            if expected_grants == 1 { "one".to_owned() } else { expected_grants.to_string() }
        ));
    }
    if end_notices == EndNoticePolicy::Forbidden && !evidence.end_notice_lines.is_empty() {
        failures.push(format!(
            "the window showed a session end notice, the session did not survive the feed: {}",
            evidence.end_notice_lines.join(" | ")
        ));
    }
    if let Err(message) = &evidence.first_reel_frames {
        failures.push(format!("reel 1 never reached the player window: {message}"));
    }
    // Раздражитель края обязан дойти до страницы, иначе строка прошла бы вхолостую.
    if let Some(Err(message)) = evidence.edge.as_ref().map(|edge| &edge.stimulus) {
        failures.push(format!(
            "the page stimulus did not happen, so the row proves nothing: {message}"
        ));
    }
    if let Some(Err(message)) = &evidence.page_download {
        failures.push(format!(
            "the page did not download past the bridge memory, so the row proves nothing: {message}"
        ));
    }
    if target_wait == TargetWaitPolicy::Forbidden && !evidence.target_wait_lines.is_empty() {
        failures.push(format!(
            "the window waited out the whole deadline for data at the start of a reel instead of playing it at once: {}",
            evidence.target_wait_lines.join(" | ")
        ));
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
        scroll_record_failures(index, scroll, failures);
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

/// Нарушения одной прокрутки колесом (`index` с нуля): колесо дошло, страница запустила следующий ролик,
/// окно переехало и кадры растут.
pub(super) fn scroll_record_failures(
    index: usize,
    scroll: &ScrollRecord,
    failures: &mut Vec<String>,
) {
    let label = format!("scroll {} (reel {} -> {})", index + 1, index + 1, index + 2);
    labelled_scroll_failures(&label, scroll, failures);
}

/// То же, что [`scroll_record_failures`], но с названием прокрутки от вызывающего (прокрутка назад идёт
/// от ролика к РАННЕМУ ролику, нумерация «индекс + 1» ей не подходит).
pub(super) fn labelled_scroll_failures(
    label: &str,
    scroll: &ScrollRecord,
    failures: &mut Vec<String>,
) {
    // Отдельная причина: колесо вообще не дошло до страницы (стенд, а не продукт, не крутит).
    if scroll.trusted_wheel_events == 0 {
        failures.push(format!(
            "{label}: the real mouse wheel never reached the page (no 'wheel trusted=true' in the page log)"
        ));
        return;
    }
    if let Err(message) = &scroll.page_started_next_reel {
        failures.push(format!(
            "{label}: the page did not start the next reel: {message}"
        ));
        return;
    }
    if let Err(message) = &scroll.player_loaded_new_generation {
        failures.push(format!(
            "{label}: the window did not relocate to the next reel: {message}"
        ));
    }
    if scroll.frame_growth_check == FrameGrowthCheck::Required {
        failures.extend(growth_failure(label, &scroll.frame_growth));
    }
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
