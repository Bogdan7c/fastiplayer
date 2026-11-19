//! Чистый приговор краёв ленты (сессия 16b3): быстрая серия прокруток, уход из ленты в другой раздел и
//! конец ленты без ухода. Ввода-вывода нет, каждое нарушение проверяется юнит-тестом (`edge_judge/tests.rs`).
//!
//! Что считается результатом, а не вызовом:
//!  * серия: страница ПРОШЛА серию (дошла до последнего ролика, промежуточные ролики получили `play()` и
//!    вскоре `pause()`), окно в итоге показывает ПОСЛЕДНИЙ ролик, ширины кадра шли только вперёд, номера
//!    поколений идут подряд без дыр, кадры на последнем ролике растут;
//!  * уход из ленты: раздражитель был целиком (адрес, удаление/снятие источника, играющее видимое превью),
//!    окно кончило сеанс сообщением «Source tab closed» (`notice=finished cause=tab_gone`) и закрылось, а
//!    превью другого раздела окно НЕ приняло: новых поколений и новых ширин кадра нет;
//!  * конец ленты: страница ушла в раздел без видео и остановила последний ролик, а окно за всё ожидание не
//!    показало сообщения о конце, не взяло нового ролика и осталось на месте.

use super::judge::{
    FeedEvidence, growth_failure, metric, scroll_record_failures, without_consecutive_repeats,
};
use crate::observe::{EndNotice, FrameGrowth};

/// Сколько роликов в серии должны быть промежуточными (получили `play()` и вскоре `pause()`): серия из
/// трёх прокруток после спокойной даёт два промежуточных ролика, последний остаётся играть.
pub(super) const MIN_BURST_INTERMEDIATES: f64 = 2.0;

/// Как кончился сеанс по наблюдению робота (строки «уход из ленты»).
#[derive(Debug)]
pub(super) struct EndRecord {
    /// Сообщение о конце сеанса из журнала плеера или причина, почему его нет.
    pub notice: Result<EndNotice, String>,
    /// Окно плеера закрылось (ушло с шины) или причина, почему нет.
    pub window_closed: Result<(), String>,
}

/// Улики края ленты: что сделала страница и что стало с окном.
#[derive(Debug)]
pub(super) struct EdgeRecord {
    /// Раздражитель страницы состоялся (метки журнала страницы) или причина, почему нет.
    pub stimulus: Result<(), String>,
    /// Серия: окно дошло до показа последнего ролика (по журналу плеера) или причина, почему нет.
    pub window_reached_last_reel: Option<Result<(), String>>,
    /// Серия: рост кадров плеера на последнем ролике.
    pub frame_growth: Option<Result<FrameGrowth, String>>,
    /// Уход из ленты: как кончился сеанс.
    pub end: Option<EndRecord>,
    /// Уход из ленты и конец ленты: поколения, загруженные до начала края (после них новых быть не должно).
    pub generations_before_edge: Option<Vec<u64>>,
    /// Конец ленты: окно после всего ожидания на месте и отвечает.
    pub window_still_present: Option<Result<(), String>>,
}

/// Поколения шли строго вперёд с единицы (повтор или откат смертелен для плеера, `GenerationOutOfOrder`).
/// Дыры в номерах не нарушение: страница с двумя MediaSource на шаг (Shorts) даёт мосту два номера на ролик,
/// и окно вправе пропустить поколение, которое страница тут же сменила.
fn increasing_generations_failure(generations: &[u64]) -> Option<String> {
    let starts_at_one = generations.first() == Some(&1);
    let increasing = generations.windows(2).all(|pair| pair[0] < pair[1]);
    (!(starts_at_one && increasing)).then(|| {
        format!("the player loaded generations {generations:?}, expected strictly increasing numbers from 1")
    })
}

/// После начала края (уход из ленты, конец ленты) окно не загрузило НИ ОДНОГО нового поколения: превью
/// другого раздела не принято за следующий ролик ленты. Снимок поколений сделан роботом перед краем.
fn no_new_generation_failure(edge: &EdgeRecord, generations: &[u64]) -> Option<String> {
    let Some(before) = &edge.generations_before_edge else {
        return Some("the row did not record the generations loaded before the edge".to_owned());
    };
    let new: Vec<u64> = generations
        .iter()
        .copied()
        .filter(|generation| !before.contains(generation))
        .collect();
    (!new.is_empty()).then(|| {
        format!(
            "the player loaded generation(s) {new:?} after the edge began: a stream that is not the next reel of the feed was taken into the window"
        )
    })
}

/// Окно показало ровно эти ширины кадра по порядку, ничего другого.
fn exact_widths_failure(reel_stream_widths: &[u64], evidence: &FeedEvidence) -> Option<String> {
    let shown = without_consecutive_repeats(&evidence.shown_stream_widths);
    (shown != reel_stream_widths).then(|| {
        format!(
            "the window showed stream widths {shown:?}, expected exactly {reel_stream_widths:?}"
        )
    })
}

/// Нарушения «спокойных» прокруток, которые предшествовали краю (каждая как в строках «окно идёт за лентой»).
fn calm_scroll_failures(
    evidence: &FeedEvidence,
    expected_scrolls: usize,
    failures: &mut Vec<String>,
) {
    if evidence.scrolls.len() != expected_scrolls {
        failures.push(format!(
            "the robot made {} calm scroll(s) before the edge, the row needs {expected_scrolls}",
            evidence.scrolls.len()
        ));
    }
    for (index, scroll) in evidence.scrolls.iter().enumerate() {
        scroll_record_failures(index, scroll, failures);
    }
}

/// Запись края обязана быть: без неё строка ничего не доказывает.
fn edge_of<'a>(evidence: &'a FeedEvidence, failures: &mut Vec<String>) -> Option<&'a EdgeRecord> {
    let edge = evidence.edge.as_ref();
    if edge.is_none() {
        failures.push("the row recorded no edge evidence at all".to_owned());
    }
    edge
}

/// Быстрая серия: окно кончает на последнем ролике.
pub(super) fn last_reel_failures(
    reel_stream_widths: &[u64],
    evidence: &FeedEvidence,
    failures: &mut Vec<String>,
) {
    // Одна спокойная прокрутка (ролик 1 -> 2), дальше серия.
    calm_scroll_failures(evidence, 1, failures);
    let Some(edge) = edge_of(evidence, failures) else {
        return;
    };
    let last_reel = reel_stream_widths.len();
    if let Ok(report) = &evidence.page_report {
        if metric(report, "final_reel", failures).is_some_and(|reel| reel as usize != last_reel) {
            failures.push(format!(
                "the page ended the burst on another reel than the last (reel {last_reel}), so the row proves nothing"
            ));
        }
        if metric(report, "burst_intermediates", failures)
            .is_some_and(|reels| reels < MIN_BURST_INTERMEDIATES)
        {
            failures.push(format!(
                "the page passed fewer than {MIN_BURST_INTERMEDIATES} intermediate reels in a burst, so the row proves nothing"
            ));
        }
    }
    if let Some(Err(message)) = &edge.window_reached_last_reel {
        failures.push(format!(
            "the window never ended on the last reel (width {}): {message}",
            reel_stream_widths.last().copied().unwrap_or_default()
        ));
    }
    failures.extend(increasing_generations_failure(&evidence.generations));
    // Минимум: ролик 1, ролик 2 и итоговый ролик серии.
    if evidence.generations.len() < 3 {
        failures.push(format!(
            "the player loaded only {} generation(s), the calm scroll and the burst need at least 3",
            evidence.generations.len()
        ));
    }
    failures.extend(forward_only_widths_failure(reel_stream_widths, evidence));
    match &edge.frame_growth {
        Some(growth) => failures.extend(growth_failure("on the last reel", growth)),
        None => failures.push("no frame growth was measured on the last reel".to_owned()),
    }
}

/// Ширины кадра шли только вперёд по ленте (назад и повтор ролика — провал), а итог — последний ролик.
fn forward_only_widths_failure(
    reel_stream_widths: &[u64],
    evidence: &FeedEvidence,
) -> Option<String> {
    let shown = without_consecutive_repeats(&evidence.shown_stream_widths);
    let mut previous_position: Option<usize> = None;
    for width in &shown {
        let Some(position) = reel_stream_widths.iter().position(|known| known == width) else {
            return Some(format!(
                "the window showed stream width {width}, which is no reel of the feed {reel_stream_widths:?}"
            ));
        };
        if previous_position.is_some_and(|before| position <= before) {
            return Some(format!(
                "the window went back or repeated a reel: widths shown {shown:?}, feed order {reel_stream_widths:?}"
            ));
        }
        previous_position = Some(position);
    }
    if shown.last() != reel_stream_widths.last() {
        return Some(format!(
            "the window ended on stream width {:?}, expected the last reel's {:?} (widths shown {shown:?})",
            shown.last(),
            reel_stream_widths.last()
        ));
    }
    None
}

/// Сообщение о конце: «Source tab closed» (`notice=finished cause=tab_gone`).
fn tab_gone_notice_failure(found: &EndNotice) -> Option<String> {
    (!(found.cause == "tab_gone" && found.notice == "finished")).then(|| {
        format!(
            "expected the source tab closed notice (notice=finished, cause=tab_gone), got: {}",
            found.line
        )
    })
}

/// Уход из ленты: честный конец, превью другого раздела не принято.
pub(super) fn honest_end_failures(
    reel_stream_widths: &[u64],
    evidence: &FeedEvidence,
    failures: &mut Vec<String>,
) {
    calm_scroll_failures(
        evidence,
        reel_stream_widths.len().saturating_sub(1),
        failures,
    );
    let Some(edge) = edge_of(evidence, failures) else {
        return;
    };
    match &edge.end {
        None => failures.push("the row did not observe how the session ended".to_owned()),
        Some(end) => {
            match &end.notice {
                Ok(found) => failures.extend(tab_gone_notice_failure(found)),
                Err(message) => failures.push(format!(
                    "the session did not end honestly (no 'Source tab closed' notice): {message}"
                )),
            }
            if let Err(message) = &end.window_closed {
                failures.push(format!("the player window stayed after the end: {message}"));
            }
        }
    }
    failures.extend(no_new_generation_failure(edge, &evidence.generations));
    failures.extend(exact_widths_failure(reel_stream_widths, evidence));
}

/// Конец ленты без ухода: окно ждёт и ничего не делает.
pub(super) fn waits_failures(
    reel_stream_widths: &[u64],
    evidence: &FeedEvidence,
    failures: &mut Vec<String>,
) {
    calm_scroll_failures(
        evidence,
        reel_stream_widths.len().saturating_sub(1),
        failures,
    );
    let Some(edge) = edge_of(evidence, failures) else {
        return;
    };
    match &edge.window_still_present {
        Some(Ok(())) => {}
        Some(Err(message)) => failures.push(format!(
            "the window is gone or unresponsive after waiting at the feed end: {message}"
        )),
        None => failures.push("the row never checked that the window is still there".to_owned()),
    }
    failures.extend(no_new_generation_failure(edge, &evidence.generations));
    failures.extend(exact_widths_failure(reel_stream_widths, evidence));
}

#[cfg(test)]
mod tests;
