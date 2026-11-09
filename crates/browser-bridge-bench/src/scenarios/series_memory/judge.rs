//! Чистые приговоры сценариев памяти выбора (14b2). Браузера, плеера и экстеншена здесь нет: на вход
//! идут уже собранные улики, поэтому каждое правило проверяется синтетикой.
//!
//! Судим по ЧАСТЯМ строки страницы (`cause`, `episode`, `dub`), а не по строке целиком: поле
//! `quality=` в ней зависит от порядка событий (экстеншен мог успеть выбрать качество раньше или
//! позже), и приговор не должен зависеть от этой случайности.

use std::time::Duration;

use super::super::evidence::{host_failures, page_failures, unexpected_player_lines};
use super::super::outcome::{Outcome, Verdict};
use super::session::SessionRecord;
use super::storage::{StoredMemory, StoredSeriesChoice};

/// Озвучка по умолчанию на старте серии и озвучка, которую выбирает зритель.
pub(super) const DEFAULT_DUB: &str = "dub-a";
pub(super) const PICKED_DUB: &str = "dub-b";
/// Подпись кнопки озвучки на странице: именно её экстеншен кладёт в память (`audio`).
pub(super) const PICKED_DUB_LABEL: &str = "Dub B";

/// Значение поля `имя=значение` в строке журнала страницы.
fn field<'a>(line: &'a str, name: &str) -> Option<&'a str> {
    let prefix = format!("{name}=");
    line.split_whitespace()
        .find_map(|token| token.strip_prefix(prefix.as_str()))
}

/// Сколько раз страница сменила озвучку (`cause=dub`) на первой серии, а какая стала озвучка.
pub(super) fn dub_switches(lines: &[String]) -> Vec<String> {
    lines
        .iter()
        .filter(|line| field(line, "cause") == Some("dub") && field(line, "episode") == Some("1"))
        .filter_map(|line| field(line, "dub").map(str::to_owned))
        .collect()
}

/// Сколько раз страница перешла на озвучку `dub_id` (причина — озвучка, серия первая).
pub(super) fn switches_to(lines: &[String], dub_id: &str) -> usize {
    dub_switches(lines)
        .iter()
        .filter(|switched| switched.as_str() == dub_id)
        .count()
}

/// Что общего у любого сеанса: пропуск посредника, отчёт страницы, запрещённые строки плеера,
/// провалы шагов сценария.
fn session_failures(
    label: &str,
    record: &SessionRecord,
    failed_steps: &[String],
    page: &str,
) -> Vec<String> {
    let mut failures: Vec<String> = failed_steps
        .iter()
        .map(|step| format!("{label}: {step}"))
        .collect();
    failures.extend(
        host_failures(&record.host)
            .into_iter()
            .map(|failure| format!("{label}: {failure}")),
    );
    failures.extend(
        page_failures(&record.page_report, page)
            .into_iter()
            .map(|failure| format!("{label}: {failure}")),
    );
    failures.extend(
        unexpected_player_lines(&record.forbidden_player_lines)
            .map(|line| format!("{label}: forbidden player line: {line}")),
    );
    failures
}

/// Приговор с журналами страниц всех сеансов: при провале они нужны, чтобы увидеть, что страница делала.
fn with_page_logs(mut verdict: Verdict, sessions: &[(&str, &SessionRecord)]) -> Verdict {
    if verdict.outcome == Outcome::Fail {
        for (label, record) in sessions {
            verdict
                .detail
                .push_str(&format!("\n--- {label} session ---\n{}", record.page_log));
        }
    }
    verdict
}

/// Запись для отчёта: сколько сериалов помнит память и что в записи.
fn describe_memory(memory: &StoredMemory) -> String {
    match memory {
        StoredMemory::Absent => "no rememberedChoices key".to_owned(),
        StoredMemory::Present(choices) => format!(
            "{} series: {:?}",
            choices.series.len(),
            choices.series.values().collect::<Vec<_>>()
        ),
    }
}

/// Запись должна быть ровно одна, под ожидаемым отпечатком, с озвучкой `audio`.
fn entry_failures(
    when: &str,
    memory: &StoredMemory,
    fingerprint: &str,
    audio: &str,
) -> Vec<String> {
    let mut failures = Vec::new();
    if memory.series_count() != 1 {
        failures.push(format!(
            "{when}: the memory holds {} series, expected exactly one ({})",
            memory.series_count(),
            describe_memory(memory)
        ));
    }
    match memory.entry(fingerprint) {
        None => failures.push(format!(
            "{when}: no entry under the fingerprint {fingerprint} ({})",
            describe_memory(memory)
        )),
        Some(entry) if entry.audio.as_deref() != Some(audio) => failures.push(format!(
            "{when}: the entry remembers audio {:?}, expected {audio:?}",
            entry.audio
        )),
        Some(_) => {}
    }
    failures
}

/// Улики сценария `series-memory-restart`: три сеанса на одном профиле.
pub(super) struct RestartEvidence {
    pub page: &'static str,
    /// Отпечаток пары «сайт + сериал», посчитанный стендом независимо от экстеншена.
    pub fingerprint: String,
    /// Сеанс 1: зритель выбрал Dub B на странице.
    pub picked: SessionRecord,
    pub picked_failed_steps: Vec<String>,
    /// Память после выбора, пока браузер ещё открыт.
    pub memory_after_pick: Option<StoredMemory>,
    /// Сеанс 2: тот же профиль, страница сама ничего не жмёт.
    pub restored: SessionRecord,
    pub restored_failed_steps: Vec<String>,
    /// Память после «забыть».
    pub memory_after_forget: Option<StoredMemory>,
    /// Сеанс 3: тот же профиль после «забыть».
    pub forgotten: SessionRecord,
    pub forgotten_failed_steps: Vec<String>,
    /// Память в конце сеанса 3 (старт по умолчанию не записывается).
    pub memory_in_forgotten_session: Option<StoredMemory>,
    /// Через сколько от начала сеанса 2 (запуск браузера, страница, передача) экстеншен сам нажал озвучку
    /// (`None` — не нажал). Отсчёт тот же, что у окна тишины сеанса 3, поэтому их можно сравнивать.
    pub restored_apply_delay: Option<Duration>,
    /// Сколько от начала сеанса 3 (тот же отсчёт, что у задержки сеанса 2) смотрели, не нажмёт ли экстеншен озвучку.
    pub quiet_window: Duration,
}

/// Приговор `series-memory-restart`.
pub(super) fn judge_restart(evidence: &RestartEvidence) -> Verdict {
    let mut failures = session_failures(
        "pick",
        &evidence.picked,
        &evidence.picked_failed_steps,
        evidence.page,
    );
    failures.extend(session_failures(
        "restored",
        &evidence.restored,
        &evidence.restored_failed_steps,
        evidence.page,
    ));
    failures.extend(session_failures(
        "forgotten",
        &evidence.forgotten,
        &evidence.forgotten_failed_steps,
        evidence.page,
    ));
    // 1. Выбор зрителя на странице: страница переключилась ровно один раз и память это запомнила.
    let picks = switches_to(&evidence.picked.generation_lines, PICKED_DUB);
    if picks != 1 {
        failures.push(format!(
            "pick: the page switched to {PICKED_DUB} {picks} times, expected once"
        ));
    }
    match &evidence.memory_after_pick {
        Some(memory) => failures.extend(entry_failures(
            "pick",
            memory,
            &evidence.fingerprint,
            PICKED_DUB_LABEL,
        )),
        None => failures.push("pick: the memory was not read".to_owned()),
    }
    // 2. После перезапуска страница САМА оказалась на Dub B: нажал экстеншен по памяти.
    let restores = switches_to(&evidence.restored.generation_lines, PICKED_DUB);
    if restores != 1 {
        failures.push(format!(
            "restored: after the restart the page switched to {PICKED_DUB} {restores} times, expected once (generation lines: {:?})",
            evidence.restored.generation_lines
        ));
    }
    // Контроль честен, только если в сеансе 2 память успела сработать внутри окна сеанса 3.
    match evidence.restored_apply_delay {
        Some(delay) if delay <= evidence.quiet_window => {}
        Some(delay) => failures.push(format!(
            "restored: the memory was applied {:.1} s after the hand-off, later than the {} s window of the control session; the control would prove nothing",
            delay.as_secs_f64(),
            evidence.quiet_window.as_secs()
        )),
        None => failures.push("restored: the memory was never applied".to_owned()),
    }
    // 3. «Забыть» стирает всё.
    match &evidence.memory_after_forget {
        Some(memory) if memory.series_count() == 0 => {}
        Some(memory) => failures.push(format!(
            "forget: the memory still holds {} series ({})",
            memory.series_count(),
            describe_memory(memory)
        )),
        None => failures.push("forget: the memory was not read".to_owned()),
    }
    // 4. Контроль без памяти: за окно наблюдения озвучку никто не меняет, страница остаётся на старте.
    let unexpected = dub_switches(&evidence.forgotten.generation_lines);
    if !unexpected.is_empty() {
        failures.push(format!(
            "forgotten: the page switched dub {unexpected:?} although nothing is remembered (window {} s, default {DEFAULT_DUB})",
            evidence.quiet_window.as_secs()
        ));
    }
    match &evidence.memory_in_forgotten_session {
        Some(memory) if memory.series_count() == 0 => {}
        Some(memory) => failures.push(format!(
            "forgotten: the episode start default was written to the memory ({})",
            describe_memory(memory)
        )),
        None => failures.push("forgotten: the memory was not read".to_owned()),
    }
    let verdict = Verdict::from_failures(
        &failures,
        format!(
            "a dub picked on the page was remembered under {} (audio {PICKED_DUB_LABEL}); after a browser restart the page switched to {PICKED_DUB} by itself; after forget nothing was remembered and the page stayed on {DEFAULT_DUB} for {} s (the memory had needed {} s to apply); frames grew in every session",
            evidence.fingerprint,
            evidence.quiet_window.as_secs(),
            evidence.restored_apply_delay.map_or_else(
                || "?".to_owned(),
                |delay| format!("{:.1}", delay.as_secs_f64())
            )
        ),
    );
    with_page_logs(
        verdict,
        &[
            ("pick", &evidence.picked),
            ("restored", &evidence.restored),
            ("forgotten", &evidence.forgotten),
        ],
    )
}

/// Улики сценария `series-memory-incognito`.
pub(super) struct IncognitoEvidence {
    pub page: &'static str,
    pub fingerprint: String,
    /// Вкладка стенда в приватном сеансе видна экстеншену как приватная.
    pub stand_tab_is_private: [Option<bool>; 2],
    /// Сеанс (а): выбор зрителя на странице в приватном окне.
    pub picked: SessionRecord,
    pub picked_failed_steps: Vec<String>,
    /// Память после выбора и паузы: записи быть не должно.
    pub memory_after_pick: Option<StoredMemory>,
    /// Запись, подсаженная стендом перед сеансом (б).
    pub seeded: StoredSeriesChoice,
    /// Сеанс (б): страница сама ничего не жмёт.
    pub restored: SessionRecord,
    pub restored_failed_steps: Vec<String>,
    /// Память после сеанса (б): подсаженная запись не тронута.
    pub memory_after_restore: Option<StoredMemory>,
}

/// Приговор `series-memory-incognito`.
pub(super) fn judge_incognito(evidence: &IncognitoEvidence) -> Verdict {
    let mut failures = session_failures(
        "private-pick",
        &evidence.picked,
        &evidence.picked_failed_steps,
        evidence.page,
    );
    failures.extend(session_failures(
        "private-restore",
        &evidence.restored,
        &evidence.restored_failed_steps,
        evidence.page,
    ));
    for (index, is_private) in evidence.stand_tab_is_private.iter().enumerate() {
        match is_private {
            Some(true) => {}
            other => failures.push(format!(
                "session {}: the extension does not see the stand tab as private ({other:?}); the run would not prove anything",
                index + 1
            )),
        }
    }
    // (а) Выбор в приватном окне: страница переключилась, а память осталась пустой (даже без `used_at`).
    let picks = switches_to(&evidence.picked.generation_lines, PICKED_DUB);
    if picks != 1 {
        failures.push(format!(
            "private-pick: the page switched to {PICKED_DUB} {picks} times, expected once"
        ));
    }
    match &evidence.memory_after_pick {
        Some(memory) if memory.series_count() == 0 => {}
        Some(memory) => failures.push(format!(
            "private-pick: the memory was written in a private window ({})",
            describe_memory(memory)
        )),
        None => failures.push("private-pick: the memory was not read".to_owned()),
    }
    // (б) Чтение в приватном окне: память применилась, а запись осталась такой, какой её подсадили.
    let restores = switches_to(&evidence.restored.generation_lines, PICKED_DUB);
    if restores != 1 {
        failures.push(format!(
            "private-restore: the page switched to {PICKED_DUB} {restores} times, expected once (generation lines: {:?})",
            evidence.restored.generation_lines
        ));
    }
    match &evidence.memory_after_restore {
        Some(memory) => match memory.entry(&evidence.fingerprint) {
            Some(entry) if memory.series_count() == 1 && *entry == evidence.seeded => {}
            _ => failures.push(format!(
                "private-restore: the seeded entry {:?} changed in a private window ({})",
                evidence.seeded,
                describe_memory(memory)
            )),
        },
        None => failures.push("private-restore: the memory was not read".to_owned()),
    }
    let verdict = Verdict::from_failures(
        &failures,
        format!(
            "private window: a dub picked on the page switched the page but wrote nothing to the memory; a seeded entry ({PICKED_DUB_LABEL}, used_at {}) was applied and left untouched",
            evidence.seeded.used_at
        ),
    );
    with_page_logs(
        verdict,
        &[
            ("private-pick", &evidence.picked),
            ("private-restore", &evidence.restored),
        ],
    )
}

#[cfg(test)]
mod tests {
    use super::super::super::outcome::Outcome;
    use super::super::storage::{BENCH_SERIES_KEY, choice_fingerprint, parse_memory};
    use super::*;
    use crate::observe::HostLogEvidence;
    use crate::site::{PageReport, PageStatus, RunId};

    fn lines(items: &[&str]) -> Vec<String> {
        items.iter().map(|line| (*line).to_owned()).collect()
    }

    fn record(generation_lines: &[&str]) -> SessionRecord {
        SessionRecord {
            generation_lines: lines(generation_lines),
            host: HostLogEvidence {
                caller_accepted: true,
                hello_accepted: true,
                session_granted: true,
                session_grants: 1,
            },
            page_report: Ok(PageReport {
                run: RunId::new("r-1").expect("run id"),
                scenario: "series-variants".into(),
                status: PageStatus::Pass,
                checks: Vec::new(),
                user_agent: String::new(),
                elapsed_ms: 0,
                metrics: Default::default(),
            }),
            forbidden_player_lines: Vec::new(),
            page_log: String::new(),
        }
    }

    fn fingerprint() -> String {
        choice_fingerprint("127.0.0.1", BENCH_SERIES_KEY)
    }

    fn memory_with(audio: &str, used_at: u64) -> StoredMemory {
        parse_memory(&format!(
            r#"{{"version":1,"series":{{"{}":{{"audio":"{audio}","used_at":{used_at}}}}}}}"#,
            fingerprint()
        ))
        .expect("memory")
    }

    fn empty_memory() -> StoredMemory {
        parse_memory(r#"{"version":1,"series":{}}"#).expect("memory")
    }

    const START: &str = "generation cause=start episode=1 dub=dub-a quality=video-360 from=0";
    const PICK: &str = "generation cause=dub episode=1 dub=dub-b quality=video-360 from=0";

    #[test]
    fn dub_switches_are_read_by_parts_not_by_the_whole_line() {
        // Поле `quality=` разное, озвучка и причина те же: считаются обе строки.
        let both = lines(&[
            START,
            PICK,
            "series-variants: generation cause=dub episode=1 dub=dub-b quality=video-720 from=3",
            "generation cause=dub episode=2 dub=dub-b quality=video-720 from=0",
            "generation cause=quality episode=1 dub=dub-b quality=video-720 from=3",
        ]);
        assert_eq!(switches_to(&both, PICKED_DUB), 2);
        assert_eq!(switches_to(&both, DEFAULT_DUB), 0);
        assert_eq!(dub_switches(&lines(&[START])), Vec::<String>::new());
    }

    fn good_restart() -> RestartEvidence {
        RestartEvidence {
            page: "series-variants",
            fingerprint: fingerprint(),
            picked: record(&[START, PICK]),
            picked_failed_steps: Vec::new(),
            memory_after_pick: Some(memory_with(PICKED_DUB_LABEL, 5)),
            restored: record(&[START, PICK]),
            restored_failed_steps: Vec::new(),
            memory_after_forget: Some(empty_memory()),
            forgotten: record(&[START]),
            forgotten_failed_steps: Vec::new(),
            memory_in_forgotten_session: Some(StoredMemory::Absent),
            restored_apply_delay: Some(Duration::from_secs(4)),
            quiet_window: Duration::from_secs(15),
        }
    }

    #[test]
    fn a_remembered_restored_and_forgotten_choice_passes() {
        let verdict = judge_restart(&good_restart());
        assert_eq!(verdict.outcome, Outcome::Pass, "{verdict:?}");
    }

    fn failure_text(evidence: &RestartEvidence) -> String {
        let verdict = judge_restart(evidence);
        assert_eq!(verdict.outcome, Outcome::Fail);
        verdict.detail
    }

    #[test]
    fn a_choice_that_was_not_applied_after_the_restart_fails() {
        let mut evidence = good_restart();
        evidence.restored = record(&[START]);
        assert!(failure_text(&evidence).contains("restored: after the restart"));
    }

    #[test]
    fn a_memory_of_the_wrong_series_or_label_fails() {
        let mut evidence = good_restart();
        evidence.memory_after_pick = Some(memory_with("Dub A", 5));
        assert!(failure_text(&evidence).contains("remembers audio"));
        evidence.memory_after_pick = Some(empty_memory());
        assert!(failure_text(&evidence).contains("expected exactly one"));
        evidence.memory_after_pick = None;
        assert!(failure_text(&evidence).contains("memory was not read"));
    }

    #[test]
    fn a_memory_that_survives_forget_or_switches_the_dub_afterwards_fails() {
        let mut evidence = good_restart();
        evidence.memory_after_forget = Some(memory_with(PICKED_DUB_LABEL, 5));
        assert!(failure_text(&evidence).contains("forget: the memory still holds 1"));
        let mut evidence = good_restart();
        evidence.forgotten = record(&[START, PICK]);
        assert!(failure_text(&evidence).contains("nothing is remembered"));
        let mut evidence = good_restart();
        evidence.memory_in_forgotten_session = Some(memory_with(DEFAULT_DUB, 1));
        assert!(failure_text(&evidence).contains("episode start default was written"));
    }

    #[test]
    fn an_apply_slower_than_the_control_window_makes_the_control_vacuous() {
        let mut evidence = good_restart();
        evidence.restored_apply_delay = Some(Duration::from_secs(20));
        assert!(failure_text(&evidence).contains("the control would prove nothing"));
        evidence.restored_apply_delay = None;
        assert!(failure_text(&evidence).contains("never applied"));
    }

    #[test]
    fn a_failed_session_step_fails_the_verdict_with_its_session_name() {
        let mut evidence = good_restart();
        evidence.restored_failed_steps = vec!["frames-after-restore: no growth".to_owned()];
        assert!(failure_text(&evidence).contains("restored: frames-after-restore: no growth"));
    }

    fn good_incognito() -> IncognitoEvidence {
        IncognitoEvidence {
            page: "series-variants",
            fingerprint: fingerprint(),
            stand_tab_is_private: [Some(true), Some(true)],
            picked: record(&[START, PICK]),
            picked_failed_steps: Vec::new(),
            memory_after_pick: Some(StoredMemory::Absent),
            seeded: StoredSeriesChoice {
                audio: Some(PICKED_DUB_LABEL.to_owned()),
                quality: None,
                subtitles: None,
                used_at: 777,
            },
            restored: record(&[START, PICK]),
            restored_failed_steps: Vec::new(),
            memory_after_restore: Some(memory_with(PICKED_DUB_LABEL, 777)),
        }
    }

    #[test]
    fn a_private_window_that_reads_but_never_writes_passes() {
        let verdict = judge_incognito(&good_incognito());
        assert_eq!(verdict.outcome, Outcome::Pass, "{verdict:?}");
    }

    #[test]
    fn a_write_or_a_touched_used_at_in_a_private_window_fails() {
        let mut evidence = good_incognito();
        evidence.memory_after_pick = Some(memory_with(PICKED_DUB_LABEL, 9));
        let verdict = judge_incognito(&evidence);
        assert!(verdict.detail.contains("written in a private window"));
        let mut evidence = good_incognito();
        evidence.memory_after_restore = Some(memory_with(PICKED_DUB_LABEL, 778));
        let verdict = judge_incognito(&evidence);
        assert!(
            verdict.detail.contains("changed in a private window"),
            "{verdict:?}"
        );
    }

    #[test]
    fn a_run_in_a_non_private_tab_proves_nothing_and_fails() {
        let mut evidence = good_incognito();
        evidence.stand_tab_is_private = [Some(true), Some(false)];
        assert!(judge_incognito(&evidence).detail.contains("session 2"));
        evidence.stand_tab_is_private = [None, Some(true)];
        assert!(judge_incognito(&evidence).detail.contains("session 1"));
    }

    #[test]
    fn a_seeded_memory_that_was_not_applied_in_a_private_window_fails() {
        let mut evidence = good_incognito();
        evidence.restored = record(&[START]);
        assert!(
            judge_incognito(&evidence)
                .detail
                .contains("private-restore: the page switched")
        );
    }
}
