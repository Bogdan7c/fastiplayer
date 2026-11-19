//! Чистый приговор строк «прокрутка ленты НАЗАД» (сессия 16b3b). Ввода-вывода нет, каждое нарушение
//! проверяется юнит-тестом (`back_judge/tests.rs`).
//!
//! Живое наблюдение (16b3): назад почти всегда новый элемент с новым MediaSource, с нуля; если элемент
//! Instagram ещё жив (назад сразу после вперёд), тот же элемент продолжает с секунды, где страница поставила
//! его на паузу. Окно плеера обязано пойти за лентой и назад: тот же сеанс, ролик 1 показан во второй раз
//! (ширины кадра `640, 512, 640`, поколения `1, 2, 3`), кадры растут. Что считается результатом, а не вызовом:
//!  * поколение 3 ВПЕРВЫЕ показано окном (строка плеера `Browser stream generation shown`), и по её `start_ms`
//!    видно, с какой секунды идёт ролик: у живого элемента — около секунды паузы страницы и заметно больше
//!    нуля, у нового MediaSource — меньше секунды;
//!  * раздражитель строки — «страница ПОЗВАЛА `play()` при возврате» (`reel-back-play-called`), а не «ролик
//!    заиграл»: пока окно держит часы элемента, `play()` его не оживляет, и «заиграл» — уже исход;
//!  * страница не получает `AbortError` от `play()`, которого сама не вызывала (решение 371);
//!  * строка-замер (решение 369): давление памяти сверх потолка моста; ожидание данных у цели НЕ провал, а
//!    результат (`immediate | waited | played_in_browser | never`); провал только «never». Исход
//!    `played_in_browser` (решение 378): окно нового поколения не загрузило, сеанс жив, окно отвечает, а страница
//!    видит, что ролик играет сам (`reel-back-started` без тени). Дальний замер (`MeasuredFar`) допускает только
//!    `immediate` и `played_in_browser`.

use super::judge::{
    FeedEvidence, ScrollRecord, growth_failure, labelled_scroll_failures, metric,
    scroll_record_failures, without_consecutive_repeats,
};
use crate::observe::{ShownGeneration, parse_target_wait_ms};

/// С какой секунды обязано идти вернувшееся поколение (и что считается исходом).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) enum BackStart {
    /// Живой элемент (Instagram): около секунды, на которой страница остановила ролик, и заметно больше нуля.
    NearPausedSecond,
    /// Новый MediaSource ролика: с начала, меньше `MAX_RESTART_START_MS`.
    FromTheBeginning,
    /// Замер (решение 369): секунда не судится, она записывается в отчёт; не проваливает всё, кроме `never`.
    Measured,
    /// Дальний замер (решение 378): назад на два ролика под давлением памяти. Проходят только `immediate` и
    /// `played_in_browser`; `waited` без предела и `never` проваливают.
    MeasuredFar,
}

impl BackStart {
    /// Замер: исход записывается в отчёт, а не судится строгими числами.
    fn is_measurement(self) -> bool {
        matches!(self, Self::Measured | Self::MeasuredFar)
    }
}

/// Ролик 1 играл до ухода вперёд не меньше этого, мс: иначе «секунда паузы» неотличима от нуля.
const MIN_PAUSED_AT_MS: u64 = 2500;
/// Живой элемент: насколько `start_ms` может отличаться от секунды паузы страницы, мс. Между паузой и
/// показом проходят `play()` страницы (80-400 мс) и подготовка окна.
const MAX_START_DRIFT_MS: u64 = 1500;
/// Живой элемент: «заметно больше нуля», мс.
const MIN_RESUMED_START_MS: u64 = 1000;
/// Новый MediaSource: поколение начинается «с нуля», то есть раньше этой секунды, мс.
const MAX_RESTART_START_MS: u64 = 1000;
/// Ширина кадра ролика 1: окно «на ролике 1», когда оно показывает её последней.
const FIRST_REEL_WIDTH: u64 = 640;

/// Улики прокрутки назад: что сделала страница и что показало окно.
#[derive(Debug)]
pub(super) struct BackRecord {
    /// Страница позвала `play()` вернувшегося ролика так, как требует строка (метка `reel-back-play-called`
    /// нужного вида), и нагрузка памятью состоялась, если её просила строка; иначе причина.
    pub stimulus: Result<(), String>,
    /// Сколько роликов прошла лента вперёд до возврата (ширины кадра `reel_stream_widths` без возврата).
    pub forward_reels: usize,
    /// Секунда, на которой страница остановила ролик 1 при прокрутке вперёд (`reel-paused n=1 at_ms=`).
    pub paused_at_ms: Option<u64>,
    /// Прокрутка колесом ВВЕРХ к ролику 1.
    pub scroll_back: ScrollRecord,
    /// Поколения, которые окно показало, по порядку журнала плеера.
    pub shown: Vec<ShownGeneration>,
    /// Страница видит, что вернувшийся ролик ИГРАЕТ (`reel-back-started`: часы элемента пошли).
    pub page_saw_playing: bool,
    /// Окно плеера отвечает после возврата (исход `played_in_browser` его требует) или причина, почему нет.
    pub window_responds: Result<(), String>,
    /// Строки журнала страницы `play-rejected …` за весь прогон: `AbortError` в них — провал.
    pub play_rejections: Vec<String>,
}

/// Чем кончился возврат окна к ролику 1 (для замера решений 369 и 378).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) enum BackOutcome {
    /// Ролик показан и играет, ожидания данных не было.
    Immediate,
    /// Ролик показан и играет, но окно сначала ждало данных у цели.
    Waited,
    /// Решение 378: окно ролик не взяло, сеанс жив, окно отвечает, страница играет ролик сама.
    PlayedInBrowser,
    /// Окно за срок не показало ролик 1 с растущими кадрами и страница сама его тоже не играет.
    Never,
}

impl BackOutcome {
    fn name(self) -> &'static str {
        match self {
            Self::Immediate => "immediate",
            Self::Waited => "waited",
            Self::PlayedInBrowser => "played_in_browser",
            Self::Never => "never",
        }
    }
}

/// Поколение, которое должно быть показано при возврате на одну прокрутку назад: следом за «ролик 1, ролик 2».
const RETURNING_GENERATION: u64 = 3;

/// Самое долгое ожидание данных у цели по строкам плеера, мс (0 — ожиданий не было).
fn waited_ms(evidence: &FeedEvidence) -> u64 {
    evidence
        .target_wait_lines
        .iter()
        .filter_map(|line| parse_target_wait_ms(line))
        .max()
        .unwrap_or(0)
}

/// Последнее показанное окном поколение (`None` — окно ничего не показало).
fn last_shown(back: &BackRecord) -> Option<&ShownGeneration> {
    back.shown.last()
}

/// Окно показывает ролик 1 ПОСЛЕ возврата: ширины кадра по порядку длиннее пути вперёд и кончаются на ролике 1.
fn window_is_back_on_first_reel(evidence: &FeedEvidence, back: &BackRecord) -> bool {
    let shown = without_consecutive_repeats(&evidence.shown_stream_widths);
    shown.len() > back.forward_reels && shown.last() == Some(&FIRST_REEL_WIDTH)
}

/// Исход возврата. `never`, пока ни окно, ни страница ролик не играют.
fn outcome(evidence: &FeedEvidence, back: &BackRecord) -> BackOutcome {
    let window_took_reel = window_is_back_on_first_reel(evidence, back);
    let window_plays = window_took_reel
        && last_shown(back).is_some()
        && growth_failure("back", &back.scroll_back.frame_growth).is_none();
    if window_plays {
        return if waited_ms(evidence) == 0 {
            BackOutcome::Immediate
        } else {
            BackOutcome::Waited
        };
    }
    // Окно ролик не взяло (если взяло, но кадры стоят — это `never`). Это решение 378, только если ничего не сломано: сеанс жив (проверяет общий
    // приговор: сообщения о конце нет), окно отвечает, а страница видит, что ролик играет без тени.
    if !window_took_reel && back.page_saw_playing && back.window_responds.is_ok() {
        return BackOutcome::PlayedInBrowser;
    }
    BackOutcome::Never
}

/// Приписка к отчёту строк возврата: числа замера решений 369 и 378 (пусто, если строка не про возврат).
pub(super) fn measurement_note(evidence: &FeedEvidence) -> String {
    let Some(back) = &evidence.back else {
        return String::new();
    };
    let start = if window_is_back_on_first_reel(evidence, back) {
        last_shown(back).map_or_else(|| "none".to_owned(), |shown| shown.start_ms.to_string())
    } else {
        "none".to_owned()
    };
    format!(
        "\nback_start_ms={start} back_waited_ms={} outcome={}",
        waited_ms(evidence),
        outcome(evidence, back).name()
    )
}

/// Приговор строки «назад»: страница вернулась к ролику, окно пошло за ней.
pub(super) fn return_failures(
    reel_stream_widths: &[u64],
    start: BackStart,
    evidence: &FeedEvidence,
    failures: &mut Vec<String>,
) {
    let Some(back) = &evidence.back else {
        failures.push("the row recorded no back-scroll evidence at all".to_owned());
        return;
    };
    if let Err(message) = &back.stimulus {
        failures.push(format!(
            "the page did not call play() of the earlier reel on the way back, so the row proves nothing: {message}"
        ));
    }
    failures.extend(play_rejection_failures(back));
    let forward_scrolls = reel_stream_widths.len().saturating_sub(2);
    if evidence.scrolls.len() != forward_scrolls {
        failures.push(format!(
            "the robot made {} forward scroll(s) before going back, the row needs {forward_scrolls}",
            evidence.scrolls.len()
        ));
    }
    for (index, scroll) in evidence.scrolls.iter().enumerate() {
        scroll_record_failures(index, scroll, failures);
    }
    if start.is_measurement() {
        measurement_failures(reel_stream_widths, start, evidence, back, failures);
    } else {
        strict_return_failures(reel_stream_widths, start, evidence, back, failures);
    }
}

/// Страница не должна получать `AbortError` от `play()`, которого сама не вызывала (решение 371).
fn play_rejection_failures(back: &BackRecord) -> Vec<String> {
    back.play_rejections
        .iter()
        .filter(|line| line.contains("AbortError"))
        .map(|line| {
            format!("the page got an AbortError from play() it did not cause itself: {line}")
        })
        .collect()
}

/// Строки без замера: окно обязано взять вернувшийся ролик, секунда его старта судится.
fn strict_return_failures(
    reel_stream_widths: &[u64],
    start: BackStart,
    evidence: &FeedEvidence,
    back: &BackRecord,
    failures: &mut Vec<String>,
) {
    labelled_scroll_failures("scroll back (reel 2 -> 1)", &back.scroll_back, failures);
    generation_and_width_failures(reel_stream_widths, evidence, failures);
    page_failures_of_back(
        reel_stream_widths,
        evidence,
        PageReturns::ExactlyOnce,
        failures,
    );
    match last_shown(back).filter(|shown| shown.generation == RETURNING_GENERATION) {
        None => failures.push(format!(
            "the window never showed generation {RETURNING_GENERATION} (no 'Browser stream generation shown generation={RETURNING_GENERATION}' line)"
        )),
        Some(shown) => match start {
            BackStart::NearPausedSecond => {
                live_start_failures(shown.start_ms, back.paused_at_ms, failures);
            }
            _ => {
                if shown.start_ms >= MAX_RESTART_START_MS {
                    failures.push(format!(
                        "a new MediaSource must start from the beginning, but generation {RETURNING_GENERATION} started at {} ms (limit {MAX_RESTART_START_MS} ms)",
                        shown.start_ms
                    ));
                }
            }
        },
    }
}

/// Строки-замеры (решения 369 и 378): исход записывается в отчёт; провал — `never` (и `waited` у дальнего замера).
fn measurement_failures(
    reel_stream_widths: &[u64],
    start: BackStart,
    evidence: &FeedEvidence,
    back: &BackRecord,
    failures: &mut Vec<String>,
) {
    let outcome = outcome(evidence, back);
    match outcome {
        BackOutcome::Never => failures.push(
            "outcome never: neither the window nor the page plays reel 1 again within the deadline"
                .to_owned(),
        ),
        BackOutcome::Waited if start == BackStart::MeasuredFar => failures.push(
            "outcome waited: the window waited for data at the target instead of playing the reel at once or leaving it to the page (decision 378)"
                .to_owned(),
        ),
        BackOutcome::PlayedInBrowser => {
            if let Err(message) = &back.window_responds {
                failures.push(format!("the window does not respond: {message}"));
            }
        }
        BackOutcome::Immediate | BackOutcome::Waited => {
            // Окно ролик взяло: как в строгих строках, но секунду старта не судим. У дальнего замера
            // промежуточный ролик мог дать лишнее поколение, поэтому порядок поколений там не строгий.
            if start == BackStart::Measured {
                generation_and_width_failures(reel_stream_widths, evidence, failures);
            }
        }
    }
    let returns = if outcome == BackOutcome::PlayedInBrowser || start == BackStart::MeasuredFar {
        PageReturns::AtLeastOnce
    } else {
        PageReturns::ExactlyOnce
    };
    page_failures_of_back(reel_stream_widths, evidence, returns, failures);
}

/// Поколения идут подряд (`1, 2, 3`), окно показало ширины `640, 512, 640` и ничего другого.
fn generation_and_width_failures(
    reel_stream_widths: &[u64],
    evidence: &FeedEvidence,
    failures: &mut Vec<String>,
) {
    let expected_generations: Vec<u64> = (1..=reel_stream_widths.len() as u64).collect();
    if evidence.generations != expected_generations {
        failures.push(format!(
            "the player loaded generations {:?}, expected {expected_generations:?} (the return is a new generation of the same session)",
            evidence.generations
        ));
    }
    let shown = without_consecutive_repeats(&evidence.shown_stream_widths);
    if shown != reel_stream_widths {
        failures.push(format!(
            "the window showed stream widths {shown:?}, expected {reel_stream_widths:?} (reel 1, reel 2, reel 1 again)"
        ));
    }
}

/// Сколько раз страница обязана была вернуться к ролику: строка с одним возвратом — ровно раз, а у замера
/// с двумя шагами назад или без часов у элемента (исход `played_in_browser`) число не строгое.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum PageReturns {
    ExactlyOnce,
    AtLeastOnce,
}

/// Отчёт страницы: она запустила ролики вперёд, вернулась и видела настоящее колесо.
fn page_failures_of_back(
    reel_stream_widths: &[u64],
    evidence: &FeedEvidence,
    returns: PageReturns,
    failures: &mut Vec<String>,
) {
    let Ok(report) = &evidence.page_report else {
        return;
    };
    let forward_starts = reel_stream_widths.len().saturating_sub(1) as f64;
    if let Some(started) = metric(report, "reels_started", failures)
        && (started - forward_starts).abs() > f64::EPSILON
    {
        failures.push(format!(
            "the page started {started} reel(s) forward, expected {forward_starts}"
        ));
    }
    // Метрика `reel_back_started` — заиграл ли ролик у страницы; у `played_in_browser` и без часов её может не быть.
    if returns == PageReturns::ExactlyOnce
        && let Some(back_started) = metric(report, "reel_back_started", failures)
        && (back_started - 1.0).abs() > f64::EPSILON
    {
        failures.push(format!(
            "the page returned to a reel {back_started} time(s), expected once"
        ));
    }
    if metric(report, "wheel_events_trusted", failures).is_some_and(|events| events < 1.0) {
        failures.push("the page saw no trusted wheel events".to_owned());
    }
}

/// Живой элемент продолжает со своей секунды: `start_ms` около секунды паузы страницы и заметно больше нуля.
fn live_start_failures(start_ms: u64, paused_at_ms: Option<u64>, failures: &mut Vec<String>) {
    let Some(paused_at_ms) = paused_at_ms else {
        failures.push(
            "the page never logged the second it paused reel 1 at ('reel-paused n=1 at_ms='), so the start second proves nothing"
                .to_owned(),
        );
        return;
    };
    if paused_at_ms < MIN_PAUSED_AT_MS {
        failures.push(format!(
            "reel 1 played only {paused_at_ms} ms before the page paused it (need {MIN_PAUSED_AT_MS} ms), the row cannot tell a resume from a restart"
        ));
    }
    if start_ms < MIN_RESUMED_START_MS {
        failures.push(format!(
            "the live element must continue from where the page paused it, but generation started at {start_ms} ms (page paused at {paused_at_ms} ms)"
        ));
    } else if start_ms.abs_diff(paused_at_ms) > MAX_START_DRIFT_MS {
        failures.push(format!(
            "generation started at {start_ms} ms, far from the {paused_at_ms} ms where the page paused the live element (limit {MAX_START_DRIFT_MS} ms)"
        ));
    }
}

/// Секунда паузы ролика из строк журнала страницы `reel-paused n=<номер> at_ms=<мс>`; берётся последняя строка.
pub(super) fn parse_paused_at_ms(lines: &[String]) -> Option<u64> {
    lines.iter().rev().find_map(|line| {
        let (_, tail) = line.rsplit_once("at_ms=")?;
        let digits: String = tail.chars().take_while(char::is_ascii_digit).collect();
        digits.parse().ok()
    })
}

#[cfg(test)]
mod tests;
