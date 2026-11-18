//! Чистый приговор строки `series-memory-autoplay` (16e): «озвучка та же после автоперехода».
//! Браузера, плеера и экстеншена здесь нет — только собранные улики, поэтому каждое правило
//! проверяется синтетикой в тестах ниже.

use std::time::Duration;

use super::super::outcome::Verdict;
use super::judge::{
    DEFAULT_DUB, PICKED_DUB, PICKED_DUB_LABEL, entry_failures, field, session_failures,
    with_page_logs,
};
use super::session::SessionRecord;
use super::storage::StoredMemory;

/// Запас, на который позиция серии 2 может обогнать прожитое с её старта время: часы плеера,
/// опрос MPRIS и чтение журнала страницы идут не в одну миллисекунду.
const POSITION_SLACK: Duration = Duration::from_secs(3);

/// Сколько окон браузерного режима плеер создал за жизнь окружения (строки журнала плеера).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) struct WindowCount(pub usize);

/// Где стояла серия 2 после того, как экстеншен вернул ей озвучку.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) struct EpisodeTwoStart {
    /// Позиция плеера (MPRIS), мкс.
    pub position_micros: i64,
    /// Сколько прошло с момента, когда стенд увидел смену озвучки серии 2, до чтения позиции.
    pub since_dub_switch: Duration,
}

/// Улики одного сеанса строки.
pub(super) struct AutoplaySession {
    pub record: SessionRecord,
    pub failed_steps: Vec<String>,
    /// Память после выбора зрителя (только у сеанса «память»).
    pub memory_after_pick: Option<StoredMemory>,
    /// Окна до конца серии 1 и в конце сеанса: автопереход не должен создавать новое окно.
    pub windows_before_end: Option<WindowCount>,
    pub windows_at_end: Option<WindowCount>,
    /// Через сколько после автоперехода экстеншен нажал озвучку серии 2 (только «память»).
    pub dub_applied_after: Option<Duration>,
    /// Позиция серии 2 в выбранной озвучке (только «память»).
    pub episode_two_start: Option<EpisodeTwoStart>,
}

/// Улики всей строки: контроль и память на одном профиле.
pub(super) struct AutoplayEvidence {
    pub page: &'static str,
    /// Отпечаток «сайт + сериал», посчитанный стендом независимо от экстеншена.
    pub fingerprint: String,
    pub control: AutoplaySession,
    pub remembered: AutoplaySession,
    /// Окно, за которое экстеншен с памятью обязан нажать озвучку и за которое без памяти не нажал.
    pub quiet_window: Duration,
}

/// Строка страницы о поколении в разобранном виде: причина, серия, озвучка.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct GenerationLine<'a> {
    cause: &'a str,
    episode: &'a str,
    dub: &'a str,
}

/// Разбирает строки `generation cause=… episode=… dub=…` (строки без этих полей пропускаются).
fn generation_lines(lines: &[String]) -> Vec<GenerationLine<'_>> {
    lines
        .iter()
        .filter_map(|line| {
            Some(GenerationLine {
                cause: field(line, "cause")?,
                episode: field(line, "episode")?,
                dub: field(line, "dub")?,
            })
        })
        .collect()
}

/// Индексы строк с данной причиной и серией.
fn positions_of(lines: &[GenerationLine<'_>], cause: &str, episode: &str) -> Vec<usize> {
    lines
        .iter()
        .enumerate()
        .filter(|(_, line)| line.cause == cause && line.episode == episode)
        .map(|(index, _)| index)
        .collect()
}

/// Автопереход страницы: ровно один, на серию 2, в озвучке по умолчанию. Возвращает его индекс.
/// Без этого строка не доказывала бы ничего: страница, сама державшая Dub B, прошла бы и без памяти.
fn autoplay_failures(
    label: &str,
    lines: &[GenerationLine<'_>],
    failures: &mut Vec<String>,
) -> Option<usize> {
    let autoplays = positions_of(lines, "autoplay", "2");
    let [index] = autoplays.as_slice() else {
        failures.push(format!(
            "{label}: the page autoplayed episode 2 {} times, expected once ({lines:?})",
            autoplays.len()
        ));
        return None;
    };
    if lines[*index].dub != DEFAULT_DUB {
        failures.push(format!(
            "{label}: the page autoplayed episode 2 in {}, expected the site default {DEFAULT_DUB}; the row would prove nothing",
            lines[*index].dub
        ));
    }
    Some(*index)
}

/// Автопереход не создаёт новое окно: сколько было до конца серии 1, столько и в конце сеанса.
fn window_failures(label: &str, session: &AutoplaySession, failures: &mut Vec<String>) {
    match (session.windows_before_end, session.windows_at_end) {
        (Some(before), Some(after)) if before == after && before.0 > 0 => {}
        (Some(before), Some(after)) => failures.push(format!(
            "{label}: the player created windows {before:?} before the end of episode 1 and {after:?} at the end of the session; expected the same window"
        )),
        _ => failures.push(format!("{label}: the player windows were not counted")),
    }
}

/// Контроль: автопереход в Dub A и ни одной смены озвучки за весь сеанс.
fn control_failures(session: &AutoplaySession, failures: &mut Vec<String>) {
    let lines = generation_lines(&session.record.generation_lines);
    autoplay_failures("control", &lines, failures);
    let switches: Vec<_> = lines.iter().filter(|line| line.cause == "dub").collect();
    if !switches.is_empty() {
        failures.push(format!(
            "control: the page switched dub {switches:?} although nothing is remembered"
        ));
    }
    window_failures("control", session, failures);
}

/// Память: выбор зрителя в серии 1 → автопереход в Dub A → экстеншен ставит Dub B серии 2, вовремя,
/// с начала серии, в том же окне.
fn remembered_failures(evidence: &AutoplayEvidence, failures: &mut Vec<String>) {
    let session = &evidence.remembered;
    let lines = generation_lines(&session.record.generation_lines);
    let picks = positions_of(&lines, "dub", "1");
    if picks.len() != 1 || picks.iter().any(|&index| lines[index].dub != PICKED_DUB) {
        failures.push(format!(
            "remembered: expected exactly one viewer pick of {PICKED_DUB} in episode 1 ({lines:?})"
        ));
    }
    match &session.memory_after_pick {
        Some(memory) => failures.extend(entry_failures(
            "remembered",
            memory,
            &evidence.fingerprint,
            PICKED_DUB_LABEL,
        )),
        None => failures.push("remembered: the memory was not read after the pick".to_owned()),
    }
    let autoplay = autoplay_failures("remembered", &lines, failures);
    let restores = positions_of(&lines, "dub", "2");
    match (autoplay, restores.as_slice()) {
        (Some(autoplay), [restore]) if *restore > autoplay && lines[*restore].dub == PICKED_DUB => {}
        _ => failures.push(format!(
            "remembered: expected exactly one switch of episode 2 to {PICKED_DUB} after the autoplay ({lines:?})"
        )),
    }
    // Контроль честен, только если память сработала внутри того же окна тишины.
    match session.dub_applied_after {
        Some(delay) if delay <= evidence.quiet_window => {}
        Some(delay) => failures.push(format!(
            "remembered: the dub was restored {:.1} s after the autoplay, later than the {} s control window",
            delay.as_secs_f64(),
            evidence.quiet_window.as_secs()
        )),
        None => failures.push("remembered: the dub was never restored on episode 2".to_owned()),
    }
    failures.extend(episode_two_start_failures(session.episode_two_start));
    window_failures("remembered", session, failures);
}

/// Серия 2 в выбранной озвучке идёт с начала: позиция не больше прожитого с её старта (и запаса).
/// Ошибка «вернулись на секунду конца серии 1» дала бы позицию у самого конца ролика.
fn episode_two_start_failures(start: Option<EpisodeTwoStart>) -> Vec<String> {
    let Some(start) = start else {
        return vec!["remembered: the episode 2 position was not read".to_owned()];
    };
    let limit = start.since_dub_switch + POSITION_SLACK;
    let position = Duration::from_micros(u64::try_from(start.position_micros).unwrap_or(u64::MAX));
    if position <= limit {
        return Vec::new();
    }
    vec![format!(
        "remembered: episode 2 is at {:.1} s only {:.1} s after its dub switch; it did not start from the beginning",
        position.as_secs_f64(),
        start.since_dub_switch.as_secs_f64()
    )]
}

/// Приговор `series-memory-autoplay`.
pub(super) fn judge_autoplay(evidence: &AutoplayEvidence) -> Verdict {
    let mut failures = session_failures(
        "control",
        &evidence.control.record,
        &evidence.control.failed_steps,
        evidence.page,
    );
    failures.extend(session_failures(
        "remembered",
        &evidence.remembered.record,
        &evidence.remembered.failed_steps,
        evidence.page,
    ));
    control_failures(&evidence.control, &mut failures);
    remembered_failures(evidence, &mut failures);
    let verdict = Verdict::from_failures(
        &failures,
        format!(
            "without memory the autoplayed episode 2 stayed on {DEFAULT_DUB} for {} s; with {PICKED_DUB_LABEL} remembered the extension switched the autoplayed episode 2 from {DEFAULT_DUB} to {PICKED_DUB} in {} s, from the start, in the same player window",
            evidence.quiet_window.as_secs(),
            evidence.remembered.dub_applied_after.map_or_else(
                || "?".to_owned(),
                |delay| format!("{:.1}", delay.as_secs_f64())
            )
        ),
    );
    with_page_logs(
        verdict,
        &[
            ("control", &evidence.control.record),
            ("remembered", &evidence.remembered.record),
        ],
    )
}

#[cfg(test)]
#[path = "autoplay_judge_tests.rs"]
mod tests;
