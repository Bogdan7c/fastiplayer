//! Чистые приговоры сценария `episodes` (сессия 16a): на вход только собранные улики, ни ввода-вывода.
//!
//! Улики — строки журнала плеера (окно, полный экран, заголовок, слова зрителю), сообщение о конце
//! сеанса, отчёт страницы и посредника. Разбор строк тоже здесь, чтобы формат журнала плеера
//! (договор со стендом) был виден в одном месте и закреплён тестами.

use std::time::Duration;

use super::replay_guard::previous_episode_replay_failures;
use crate::observe::{EndNotice, HostLogEvidence};
use crate::scenarios::evidence::{host_failures, page_failures, unexpected_player_lines};
use crate::scenarios::outcome::Verdict;
use crate::site::{PageReport, SiteError};

/// Начало слов зрителю: окно пишет так и «ждём следующую серию», и «надпись снята».
pub(super) const VIEWER_NOTICE_MARKER: &str = "Browser window tells the viewer";
/// Окно создано (одна строка на окно; вторая значила бы, что окно пересоздавали).
pub(super) const WINDOW_CREATED_MARKER: &str = "Browser window created";
/// Окно сменило полный экран.
pub(super) const FULLSCREEN_MARKER: &str = "Browser window fullscreen changed";
/// Окно получило новый заголовок страницы.
pub(super) const PAGE_TITLE_MARKER: &str = "Browser page title changed";
/// Начало процесса плеера (одна строка на процесс).
pub(super) const PLAYER_PROCESS_MARKER: &str = "=== fastiplayer browser session ===";
/// Сообщение о конце сеанса (любое): у серий с паузой до конца прогона его быть не должно.
pub(super) const END_NOTICE_MARKER: &str = "Browser session end notice shown";
/// Доигрывание буфера: у честных концов без доигрывания его нет.
pub(super) const DRAIN_FINISHED_MARKER: &str = "Playback drain finished";

/// Окно написало «ждём следующую серию».
const WAITING_TEXT: &str = "waiting for the next episode";
/// Окно сняло надпись ожидания.
const WAITING_CLEARED_TEXT: &str = "the waiting notice is cleared";
/// Причина снятия: показано новое поколение (серия 2 пришла).
const CLEARED_BY_NEW_GENERATION: &str = "generation_shown";

/// Страница честно закончила видео «забрала» (код 4009): для одностраничника — неверный итог.
const PAGE_TOOK_VIDEO_BACK: &str = "page_took_video_back";

/// Срок ожидания экстеншена после конца серии (`NEXT_EPISODE_WAIT_MS`), с.
const NEXT_EPISODE_WAIT: Duration = Duration::from_secs(60);
/// Конец сеанса не раньше, чем за столько до срока (таймер экстеншена не бывает быстрее).
const NEXT_EPISODE_EARLIEST: Duration = Duration::from_secs(55);
/// Конец сеанса не позже, чем к этому сроку (запас на опрос стенда и окно).
const NEXT_EPISODE_LATEST: Duration = Duration::from_secs(75);

/// Видна ли вкладка страницы в момент конца серии.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) enum PageTab {
    /// Вкладка обычная: стенд её не прятал.
    Left,
    /// Стенд открыл поверх пустую вкладку: страница в фоне, её таймеры тикают реже.
    Hidden,
}

/// Что строка обязана доказать.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) enum Expectation {
    /// Серия 2 приходит после паузы: окно ждёт и дожидается, остаётся в полном экране.
    DelayedNext(PageTab),
    /// Серии 2 нет: через ~60 с «The page didn't start the next episode».
    NoNextEpisode,
    /// Одностраничник ушёл со страницы просмотра: «вкладка закрыта», а не «страница забрала».
    SpaAway,
    /// Серия 2 тем же элементом сразу, в порядке PlayerJS (16a3): как `DelayedNext`, и серия 1 не
    /// перезапускается до показа серии 2.
    InstantNext,
}

/// Строки журнала плеера по смыслу, в порядке появления в журнале.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub(super) struct PlayerJournal {
    /// «Browser window created».
    pub(super) window_created: Vec<String>,
    /// Начала процесса плеера.
    pub(super) process_starts: Vec<String>,
    /// «Browser window fullscreen changed fullscreen=…».
    pub(super) fullscreen_changes: Vec<String>,
    /// «Browser window tells the viewer …» (ожидание и снятие надписи по порядку).
    pub(super) viewer_notices: Vec<String>,
    /// «Browser page title changed title_bytes=… revision=…».
    pub(super) page_titles: Vec<String>,
    /// Конец показанного, старт звука и загрузка поколения — по порядку ([`EPISODE_CHANGE_MARKERS`]).
    pub(super) episode_change: Vec<String>,
}

/// Конец сеанса, который стенд дождался после действия, и через сколько.
pub(super) type ObservedEnd = Result<(EndNotice, Duration), String>;

/// Всё, что собрал прогон строки: вход приговора.
#[derive(Debug)]
pub(super) struct EpisodeEvidence {
    /// Шаги прогона, которые не удались (передача, кадры, ожидание страницы): по одному тексту на шаг.
    pub(super) failed_steps: Vec<String>,
    pub(super) journal: PlayerJournal,
    /// Сообщения о конце сеанса, замеченные ДО того, как стенд сам свернул страницу и браузер.
    pub(super) end_notices_while_running: Vec<String>,
    /// Конец сеанса для строк, которые его ждут (`None` — строка его не ждала).
    pub(super) end: Option<ObservedEnd>,
    /// Окно закрылось само (для строк с концом); `None` — не ждали.
    pub(super) window_closed: Option<Result<(), String>>,
    /// Строки страницы «серия кончилась» (с видимостью вкладки).
    pub(super) page_ended_lines: Vec<String>,
    /// Строки «Playback drain finished».
    pub(super) drain_lines: Vec<String>,
    pub(super) host: HostLogEvidence,
    pub(super) page_report: Result<PageReport, SiteError>,
    pub(super) forbidden_player_lines: Vec<String>,
    /// Хвост журнала страницы: главная улика при провале.
    pub(super) page_log: String,
}

/// Достаёт `name=<слово>` из строки журнала; левая граница — пробел (как у остальных разборов стенда).
pub(super) fn field_of(line: &str, name: &str) -> Option<String> {
    let pattern = format!("{name}=");
    let (start, _) = line
        .match_indices(&pattern)
        .find(|(index, _)| *index == 0 || line.as_bytes()[index - 1] == b' ')?;
    let word: String = line[start + pattern.len()..]
        .chars()
        .take_while(|symbol| !symbol.is_whitespace())
        .collect();
    (!word.is_empty()).then_some(word)
}

/// Приговор строки.
pub(super) fn judge(
    page_id: &str,
    expectation: Expectation,
    evidence: &EpisodeEvidence,
) -> Verdict {
    let mut failures: Vec<String> = evidence.failed_steps.clone();
    failures.extend(host_failures(&evidence.host));
    failures.extend(page_failures(&evidence.page_report, page_id));
    failures.extend(
        unexpected_player_lines(&evidence.forbidden_player_lines)
            .map(|line| format!("forbidden player line: {line}")),
    );
    failures.extend(window_failures(&evidence.journal));
    let summary = match expectation {
        Expectation::DelayedNext(tab) => {
            failures.extend(delayed_next_failures(tab, evidence));
            delayed_next_summary(&evidence.journal)
        }
        Expectation::InstantNext => {
            failures.extend(instant_next_failures(evidence));
            failures.extend(previous_episode_replay_failures(
                &evidence.journal.episode_change,
            ));
            delayed_next_summary(&evidence.journal)
        }
        Expectation::NoNextEpisode => {
            failures.extend(no_next_episode_failures(evidence));
            end_summary(evidence)
        }
        Expectation::SpaAway => {
            failures.extend(spa_away_failures(evidence));
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

/// Окно и процесс плеера создались ровно по разу: пересоздание окна на смене серии — тот самый сбой.
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

/// Серия 2 сразу (16a3): окно то же, полный экран и заголовок дошли, сеанс не кончался. Надписи
/// «ждём следующую серию» тут может и не быть: серия 2 приходит раньше, чем её стоит показывать.
fn instant_next_failures(evidence: &EpisodeEvidence) -> Vec<String> {
    let journal = &evidence.journal;
    let mut failures = fullscreen_failures(&journal.fullscreen_changes);
    failures.extend(page_title_failures(&journal.page_titles));
    if !evidence.end_notices_while_running.is_empty() {
        failures.push(format!(
            "the session ended while the next episode was coming: {}",
            evidence.end_notices_while_running.join(" | ")
        ));
    }
    failures
}

fn delayed_next_failures(tab: PageTab, evidence: &EpisodeEvidence) -> Vec<String> {
    let journal = &evidence.journal;
    let mut failures = Vec::new();
    failures.extend(fullscreen_failures(&journal.fullscreen_changes));
    failures.extend(waiting_then_new_generation_failures(
        &journal.viewer_notices,
    ));
    failures.extend(page_title_failures(&journal.page_titles));
    if !evidence.end_notices_while_running.is_empty() {
        failures.push(format!(
            "the session ended while the next episode was still coming: {}",
            evidence.end_notices_while_running.join(" | ")
        ));
    }
    if tab == PageTab::Hidden
        && !evidence
            .page_ended_lines
            .iter()
            .any(|line| line.contains("visibility=hidden"))
    {
        failures.push(format!(
            "the page tab was not hidden when the episode ended: {:?}",
            evidence.page_ended_lines
        ));
    }
    failures
}

fn delayed_next_summary(journal: &PlayerJournal) -> String {
    format!(
        "windows {}, fullscreen changes {}, viewer notices {}, title changes {}",
        journal.window_created.len(),
        journal.fullscreen_changes.len(),
        journal.viewer_notices.len(),
        journal.page_titles.len()
    )
}

/// Полный экран был включён (из запомненного) и ни разу не выключен.
pub(super) fn fullscreen_failures(changes: &[String]) -> Vec<String> {
    let values: Vec<Option<String>> = changes
        .iter()
        .map(|line| field_of(line, "fullscreen"))
        .collect();
    let mut failures = Vec::new();
    if !values.iter().any(|value| value.as_deref() == Some("true")) {
        failures.push(format!("the window never entered fullscreen: {changes:?}"));
    }
    if values.iter().any(|value| value.as_deref() == Some("false")) {
        failures.push(format!("the window left fullscreen: {changes:?}"));
    }
    failures
}

/// Сначала «ждём следующую серию», потом надпись снята ИМЕННО появлением нового поколения.
pub(super) fn waiting_then_new_generation_failures(notices: &[String]) -> Vec<String> {
    let Some(waiting_at) = notices.iter().position(|line| line.contains(WAITING_TEXT)) else {
        return vec![format!(
            "the window never said it waits for the next episode: {notices:?}"
        )];
    };
    let cleared = notices[waiting_at + 1..]
        .iter()
        .find(|line| line.contains(WAITING_CLEARED_TEXT));
    match cleared {
        None => vec![format!(
            "the waiting notice was never cleared after it appeared: {notices:?}"
        )],
        Some(line) if field_of(line, "reason").as_deref() == Some(CLEARED_BY_NEW_GENERATION) => {
            Vec::new()
        }
        Some(line) => vec![format!(
            "the waiting notice was cleared for the wrong reason (want {CLEARED_BY_NEW_GENERATION}): {line}"
        )],
    }
}

/// Хотя бы одна смена заголовка дошла до окна не первой ревизией (первая — заголовок до передачи).
pub(super) fn page_title_failures(titles: &[String]) -> Vec<String> {
    let reached_later_revision = titles.iter().any(|line| {
        let revision = field_of(line, "revision").and_then(|value| value.parse::<u64>().ok());
        let bytes = field_of(line, "title_bytes").and_then(|value| value.parse::<u64>().ok());
        revision.is_some_and(|revision| revision >= 1) && bytes.is_some_and(|bytes| bytes > 0)
    });
    if reached_later_revision {
        Vec::new()
    } else {
        vec![format!(
            "no page title change with revision >= 1 reached the window: {titles:?}"
        )]
    }
}

fn no_next_episode_failures(evidence: &EpisodeEvidence) -> Vec<String> {
    let mut failures = Vec::new();
    if !evidence
        .journal
        .viewer_notices
        .iter()
        .any(|line| line.contains(WAITING_TEXT))
    {
        failures.push(format!(
            "the window never said it waits for the next episode: {:?}",
            evidence.journal.viewer_notices
        ));
    }
    match &evidence.end {
        Some(Ok((found, elapsed))) => {
            failures.extend(end_field_failures(
                found,
                "next_episode_not_started",
                "next_episode_not_started",
                "none",
            ));
            failures.extend(timing_failures(
                *elapsed,
                NEXT_EPISODE_EARLIEST,
                NEXT_EPISODE_LATEST,
            ));
        }
        Some(Err(message)) => failures.push(message.clone()),
        None => failures.push("the row never waited for the end of the session".to_owned()),
    }
    failures.extend(window_closed_failures(evidence));
    if !evidence.drain_lines.is_empty() {
        failures.push(format!(
            "the player drained the buffer ('{DRAIN_FINISHED_MARKER}') after an honest end: {}",
            evidence.drain_lines.join(" | ")
        ));
    }
    failures
}

fn spa_away_failures(evidence: &EpisodeEvidence) -> Vec<String> {
    let mut failures = Vec::new();
    match &evidence.end {
        Some(Ok((found, _))) => failures.extend(tab_gone_notice_failures(found)),
        Some(Err(message)) => failures.push(message.clone()),
        None => failures.push("the row never waited for the end of the session".to_owned()),
    }
    failures.extend(window_closed_failures(evidence));
    // 16a3: окно досматривает прежний ролик — заголовок ушедшей страницы до него доходить не должен.
    if !evidence.journal.page_titles.is_empty() {
        failures.push(format!(
            "the window took the title of the page the site left for: {:?}",
            evidence.journal.page_titles
        ));
    }
    failures
}

/// Вкладка ушла со страницы: причина `tab_gone`, а НЕ «страница забрала видео» и не «не начала серию».
pub(super) fn tab_gone_notice_failures(found: &EndNotice) -> Vec<String> {
    if found.notice == PAGE_TOOK_VIDEO_BACK || found.cause == PAGE_TOOK_VIDEO_BACK {
        return vec![format!(
            "a single-page site left the watch page but the player said the page took the video back: {}",
            found.line.trim()
        )];
    }
    // Пока доигрывается скачанное, окно молчит; после — `finished`, при паузе — `waiting_for_viewer`.
    let says_source_tab_closed = found.cause == "tab_gone"
        && matches!(found.notice.as_str(), "finished" | "waiting_for_viewer");
    if says_source_tab_closed {
        Vec::new()
    } else {
        vec![format!(
            "expected the source tab closed notice (cause=tab_gone, notice=finished|waiting_for_viewer), got: {}",
            found.line.trim()
        )]
    }
}

/// Поля сообщения совпали с ожидаемыми, каждое названо отдельно.
fn end_field_failures(found: &EndNotice, notice: &str, cause: &str, end: &str) -> Vec<String> {
    [
        ("notice", notice, found.notice.as_str()),
        ("cause", cause, found.cause.as_str()),
        ("end", end, found.end.as_str()),
    ]
    .into_iter()
    .filter(|(_, wanted, actual)| wanted != actual)
    .map(|(field, wanted, actual)| {
        format!(
            "end notice {field}={actual}, expected {wanted}: {}",
            found.line.trim()
        )
    })
    .collect()
}

/// Сообщение пришло в окно сроков от метки страницы.
pub(super) fn timing_failures(
    elapsed: Duration,
    earliest: Duration,
    latest: Duration,
) -> Vec<String> {
    if elapsed >= earliest && elapsed <= latest {
        return Vec::new();
    }
    let kind = if elapsed < earliest {
        format!(
            "too early (the product must wait the full {} s)",
            NEXT_EPISODE_WAIT.as_secs()
        )
    } else {
        "too late".to_owned()
    };
    vec![format!(
        "end notice came {:.1} s after the source was removed, {kind}; allowed {:.0}-{:.0} s",
        elapsed.as_secs_f64(),
        earliest.as_secs_f64(),
        latest.as_secs_f64()
    )]
}

fn window_closed_failures(evidence: &EpisodeEvidence) -> Vec<String> {
    match &evidence.window_closed {
        Some(Err(message)) => vec![message.clone()],
        Some(Ok(())) => Vec::new(),
        None => vec!["the row never waited for the window to close".to_owned()],
    }
}

fn end_summary(evidence: &EpisodeEvidence) -> String {
    match &evidence.end {
        Some(Ok((found, elapsed))) => {
            format!("{} after {:.1} s", found.line.trim(), elapsed.as_secs_f64())
        }
        Some(Err(message)) => message.clone(),
        None => "no end observed".to_owned(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::scenarios::outcome::Outcome;
    use crate::site::{PageStatus, RunId};

    const WAITING: &str = "INFO Browser window tells the viewer: waiting for the next episode";
    const CLEARED_BY_GENERATION: &str = "INFO Browser window tells the viewer: the waiting notice is cleared reason=generation_shown";
    const CLEARED_BY_END: &str =
        "INFO Browser window tells the viewer: the waiting notice is cleared reason=session_ended";

    fn lines(items: &[&str]) -> Vec<String> {
        items.iter().map(|item| (*item).to_owned()).collect()
    }

    fn notice(notice: &str, cause: &str, end: &str) -> EndNotice {
        EndNotice {
            notice: notice.into(),
            cause: cause.into(),
            end: end.into(),
            line: format!(
                "Browser session end notice shown notice={notice} cause={cause} end={end}"
            ),
        }
    }

    fn healthy_journal() -> PlayerJournal {
        PlayerJournal {
            window_created: lines(&["Browser window created"]),
            process_starts: lines(&[PLAYER_PROCESS_MARKER]),
            fullscreen_changes: lines(&["Browser window fullscreen changed fullscreen=true"]),
            viewer_notices: lines(&[WAITING, CLEARED_BY_GENERATION]),
            page_titles: lines(&["Browser page title changed title_bytes=22 revision=1"]),
            episode_change: lines(&[
                "Audio stream started",
                "Browser playback ended presented_frames=700",
                "Browser stream generation loaded generation=2",
                "Audio stream started",
            ]),
        }
    }

    fn pass_report(id: &str) -> PageReport {
        PageReport {
            run: RunId::new("r-1").expect("run id"),
            scenario: id.into(),
            status: PageStatus::Pass,
            checks: Vec::new(),
            user_agent: String::new(),
            elapsed_ms: 0,
            metrics: Default::default(),
        }
    }

    fn healthy_host() -> HostLogEvidence {
        HostLogEvidence {
            caller_accepted: true,
            hello_accepted: true,
            session_granted: true,
            ..Default::default()
        }
    }

    fn evidence(journal: PlayerJournal) -> EpisodeEvidence {
        EpisodeEvidence {
            failed_steps: Vec::new(),
            journal,
            end_notices_while_running: Vec::new(),
            end: None,
            window_closed: None,
            page_ended_lines: Vec::new(),
            drain_lines: Vec::new(),
            host: healthy_host(),
            page_report: Ok(pass_report("episode-gap")),
            forbidden_player_lines: Vec::new(),
            page_log: "page log".to_owned(),
        }
    }

    #[test]
    fn a_field_is_read_only_at_a_word_boundary() {
        let line = "Browser page title changed title_bytes=22 revision=3";
        assert_eq!(field_of(line, "revision").as_deref(), Some("3"));
        assert_eq!(field_of(line, "title_bytes").as_deref(), Some("22"));
        // `bytes=` внутри `title_bytes=` — не отдельное поле.
        assert_eq!(field_of(line, "bytes"), None);
        assert_eq!(field_of("fullscreen=", "fullscreen"), None);
    }

    #[test]
    fn fullscreen_must_be_entered_and_never_left() {
        let on = "Browser window fullscreen changed fullscreen=true";
        let off = "Browser window fullscreen changed fullscreen=false";
        assert!(fullscreen_failures(&lines(&[on])).is_empty());
        assert_eq!(fullscreen_failures(&[]).len(), 1);
        let left = fullscreen_failures(&lines(&[on, off]));
        assert_eq!(left.len(), 1);
        assert!(left[0].contains("left fullscreen"));
        // Только выход: и «не входил», и «вышел».
        assert_eq!(fullscreen_failures(&lines(&[off])).len(), 2);
    }

    #[test]
    fn the_waiting_notice_must_be_cleared_by_the_new_generation_after_it_appeared() {
        assert!(
            waiting_then_new_generation_failures(&lines(&[WAITING, CLEARED_BY_GENERATION]))
                .is_empty()
        );
        assert!(waiting_then_new_generation_failures(&lines(&[CLEARED_BY_GENERATION])).len() == 1);
        let only_waiting = waiting_then_new_generation_failures(&lines(&[WAITING]));
        assert!(only_waiting[0].contains("never cleared"));
        let wrong = waiting_then_new_generation_failures(&lines(&[WAITING, CLEARED_BY_END]));
        assert!(wrong[0].contains("wrong reason"));
        // Снятие ДО ожидания (прошлая серия) не засчитывается.
        assert_eq!(
            waiting_then_new_generation_failures(&lines(&[CLEARED_BY_GENERATION, WAITING])).len(),
            1
        );
    }

    #[test]
    fn a_title_change_counts_only_from_the_second_revision_with_text() {
        let first = "Browser page title changed title_bytes=22 revision=0";
        let later = "Browser page title changed title_bytes=22 revision=1";
        let empty = "Browser page title changed title_bytes=0 revision=2";
        assert_eq!(page_title_failures(&lines(&[first])).len(), 1);
        assert_eq!(page_title_failures(&lines(&[empty])).len(), 1);
        assert!(page_title_failures(&lines(&[first, later])).is_empty());
        assert_eq!(page_title_failures(&[]).len(), 1);
    }

    #[test]
    fn timing_is_judged_against_both_edges() {
        let early = timing_failures(
            Duration::from_secs(54),
            NEXT_EPISODE_EARLIEST,
            NEXT_EPISODE_LATEST,
        );
        assert!(early[0].contains("too early"));
        let late = timing_failures(
            Duration::from_secs(76),
            NEXT_EPISODE_EARLIEST,
            NEXT_EPISODE_LATEST,
        );
        assert!(late[0].contains("too late"));
        assert!(
            timing_failures(
                Duration::from_secs(61),
                NEXT_EPISODE_EARLIEST,
                NEXT_EPISODE_LATEST
            )
            .is_empty()
        );
        // Края окна входят в допуск.
        assert!(
            timing_failures(
                NEXT_EPISODE_EARLIEST,
                NEXT_EPISODE_EARLIEST,
                NEXT_EPISODE_LATEST
            )
            .is_empty()
        );
        assert!(
            timing_failures(
                NEXT_EPISODE_LATEST,
                NEXT_EPISODE_EARLIEST,
                NEXT_EPISODE_LATEST
            )
            .is_empty()
        );
    }

    #[test]
    fn a_spa_that_left_gets_tab_closed_never_page_took_video_back() {
        assert!(
            tab_gone_notice_failures(&notice("finished", "tab_gone", "played_to_end")).is_empty()
        );
        assert!(
            tab_gone_notice_failures(&notice("waiting_for_viewer", "tab_gone", "none")).is_empty()
        );
        let wrong = tab_gone_notice_failures(&notice(
            "page_took_video_back",
            "page_took_video_back",
            "none",
        ));
        assert!(wrong[0].contains("took the video back"));
        let other = tab_gone_notice_failures(&notice(
            "next_episode_not_started",
            "next_episode_not_started",
            "none",
        ));
        assert!(other[0].contains("expected the source tab closed notice"));
    }

    #[test]
    fn a_healthy_delayed_next_passes_and_names_every_broken_promise_when_it_breaks() {
        let good = judge(
            "episode-gap",
            Expectation::DelayedNext(PageTab::Left),
            &evidence(healthy_journal()),
        );
        assert_eq!(good.outcome, Outcome::Pass, "{}", good.detail);

        let mut broken_journal = healthy_journal();
        broken_journal.window_created =
            lines(&["Browser window created", "Browser window created"]);
        broken_journal.fullscreen_changes =
            lines(&["Browser window fullscreen changed fullscreen=false"]);
        broken_journal.viewer_notices = lines(&[WAITING, CLEARED_BY_END]);
        broken_journal.page_titles = Vec::new();
        let mut broken = evidence(broken_journal);
        broken.end_notices_while_running = lines(&[
            "Browser session end notice shown notice=page_took_video_back cause=page_took_video_back end=none",
        ]);
        let verdict = judge(
            "episode-gap",
            Expectation::DelayedNext(PageTab::Left),
            &broken,
        );
        assert_eq!(verdict.outcome, Outcome::Fail);
        for fragment in [
            "created 2 time(s)",
            "never entered fullscreen",
            "left fullscreen",
            "wrong reason",
            "no page title change",
            "session ended while",
        ] {
            assert!(
                verdict.detail.contains(fragment),
                "missing '{fragment}' in {}",
                verdict.detail
            );
        }
    }

    #[test]
    fn the_instant_next_row_fails_when_episode_1_plays_again_before_episode_2() {
        let verdict = judge(
            "episode-gap",
            Expectation::InstantNext,
            &evidence(healthy_journal()),
        );
        assert_eq!(verdict.outcome, Outcome::Pass, "{}", verdict.detail);
        let mut replayed = healthy_journal();
        replayed.episode_change = lines(&[
            "Browser playback ended presented_frames=700",
            "Audio stream started",
            "Browser stream generation loaded generation=2",
        ]);
        let verdict = judge("episode-gap", Expectation::InstantNext, &evidence(replayed));
        assert_eq!(verdict.outcome, Outcome::Fail);
        assert!(
            verdict.detail.contains("played again"),
            "{}",
            verdict.detail
        );
    }

    #[test]
    fn the_hidden_tab_row_needs_proof_that_the_tab_was_hidden() {
        let mut shown = evidence(healthy_journal());
        shown.page_ended_lines = lines(&["episode-1-ended visibility=visible"]);
        let verdict = judge(
            "episode-gap",
            Expectation::DelayedNext(PageTab::Hidden),
            &shown,
        );
        assert_eq!(verdict.outcome, Outcome::Fail);
        assert!(verdict.detail.contains("was not hidden"));
        let mut hidden = evidence(healthy_journal());
        hidden.page_ended_lines = lines(&["episode-1-ended visibility=hidden"]);
        let verdict = judge(
            "episode-gap",
            Expectation::DelayedNext(PageTab::Hidden),
            &hidden,
        );
        assert_eq!(verdict.outcome, Outcome::Pass, "{}", verdict.detail);
    }

    #[test]
    fn no_next_episode_needs_the_right_notice_in_time_a_closed_window_and_no_drain() {
        let mut journal = healthy_journal();
        journal.viewer_notices = lines(&[WAITING, CLEARED_BY_END]);
        let mut good = evidence(journal.clone());
        good.end = Some(Ok((
            notice(
                "next_episode_not_started",
                "next_episode_not_started",
                "none",
            ),
            Duration::from_secs(61),
        )));
        good.window_closed = Some(Ok(()));
        assert_eq!(
            judge("episode-gap", Expectation::NoNextEpisode, &good).outcome,
            Outcome::Pass
        );

        let mut bad = evidence(journal);
        // 4009 вместо 4011, слишком рано, окно не закрылось, доигрывание было.
        bad.end = Some(Ok((
            notice("page_took_video_back", "page_took_video_back", "none"),
            Duration::from_secs(6),
        )));
        bad.window_closed = Some(Err("the player window did not close".to_owned()));
        bad.drain_lines = lines(&["Playback drain finished"]);
        let verdict = judge("episode-gap", Expectation::NoNextEpisode, &bad);
        assert_eq!(verdict.outcome, Outcome::Fail);
        for fragment in [
            "notice=page_took_video_back, expected next_episode_not_started",
            "too early",
            "did not close",
            "drained the buffer",
        ] {
            assert!(
                verdict.detail.contains(fragment),
                "missing '{fragment}' in {}",
                verdict.detail
            );
        }
    }

    #[test]
    fn spa_away_fails_when_the_window_took_the_title_of_the_page_left_for() {
        let mut journal = healthy_journal();
        journal.page_titles.clear();
        let mut gone = evidence(journal);
        gone.end = Some(Ok((
            notice("finished", "tab_gone", "none"),
            Duration::from_secs(20),
        )));
        gone.window_closed = Some(Ok(()));
        let verdict = judge("episode-gap", Expectation::SpaAway, &gone);
        assert_eq!(verdict.outcome, Outcome::Pass, "{}", verdict.detail);
        gone.journal.page_titles = lines(&["Browser page title changed title_bytes=4 revision=1"]);
        let verdict = judge("episode-gap", Expectation::SpaAway, &gone);
        assert_eq!(verdict.outcome, Outcome::Fail);
        assert!(
            verdict.detail.contains("title of the page the site left"),
            "{}",
            verdict.detail
        );
    }

    #[test]
    fn spa_away_fails_when_the_end_never_came() {
        let mut silent = evidence(healthy_journal());
        silent.end = Some(Err("no end notice from the player".to_owned()));
        silent.window_closed = Some(Ok(()));
        let verdict = judge("episode-gap", Expectation::SpaAway, &silent);
        assert_eq!(verdict.outcome, Outcome::Fail);
        assert!(verdict.detail.contains("no end notice"));
    }
}
