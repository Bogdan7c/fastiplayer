//! Чистый приговор `hls-live-sleep` (сессия 16d4): по шагам робота, журналу сервера стенда, журналу
//! плеера и отчёту страницы. Браузера и плеера здесь нет — проверяется тестами на синтетике.
//!
//! Что доказывается:
//! 1. **Прогон годен** — передача вкладки, первые кадры плеера и хотя бы один удачный плейлист фона
//!    случились ДО «сна» сервера. Иначе строка проверяла бы не «сон посреди эфира», а «старт во сне».
//! 2. **Ожидание сети (решение 334)** — фон экстеншена получил на плейлист 503 несколько раз за не
//!    меньше чем [`MIN_OUTAGE_SPAN`] (плеер повторял, а не сдался на первом), после сна плейлист снова
//!    скачался, в журнале плеера — «network recovered».
//! 3. **Склейка окна (решение 333)** — после сна фон качал сегменты НОВОГО окна (номера, которых не
//!    было ни в одном плейлисте до сна), в журнале плеера — строка склейки, после неё кадров не меньше
//!    [`MIN_FRAMES_AFTER_REJOIN`], откатов позиции нет.
//! 4. **Поколение не отвергнуто** — ни одной строки отказа потока, запрещённых строк, провалов
//!    посредника и страницы.
//!
//! «Журнал фона» здесь — журнал запросов сервера стенда: запросы фона экстеншена — это запросы
//! скрипта (`Sec-Fetch-Dest` не `video`/`audio`) к маршруту эфира, со статусом ответа.

use std::time::{Duration, Instant};

use super::journal::{SleepJournal, read_sleep_journal};
use crate::observe::HostLogEvidence;
use crate::scenarios::evidence::{host_failures, page_failures, unexpected_player_lines};
use crate::scenarios::outcome::Verdict;
use crate::site::{
    HLS_LIVE_PLAYLIST_NAME, HLS_LIVE_SLEEP_ASLEEP_STATUS, HLS_LIVE_SLEEP_ROUTE_PREFIX,
    HLS_LIVE_SLEEP_SCHEDULE, HLS_LIVE_SLEEP_SEGMENT_PERIOD, MediaRequest, MediaRequester,
    PageReport, SiteError, first_segment_unseen_before_sleep,
};

/// Страница сценария (она же `scenario` в её отчёте).
pub(super) const HLS_LIVE_SLEEP_PAGE: &str = "hls-live-sleep";
/// Сколько кадров плеер обязан показать после склейки: ~4 с эфира по 24 кадра/с. Сводки идут раз в
/// 10 с, сценарий ждёт после склейки две сводки, так что запас большой.
pub(super) const MIN_FRAMES_AFTER_REJOIN: u64 = 100;
/// Сколько 503 должен получить фон: одна — случайность, две и больше — повторы плеера.
const MIN_ASLEEP_ANSWERS: usize = 2;
/// Наименьший разброс между первым и последним 503 фона. Сон длится 20 с, паузы повторов плеера
/// 0,5 → 8 с: последний повтор во сне приходится на ~15 с. 10 с доказывает, что плеер ждал долго,
/// а не уложился в быстрые повторы транспорта (~0,3 с), которые были и до 16d3.
pub(super) const MIN_OUTAGE_SPAN: Duration = Duration::from_secs(10);

/// Результат одного шага робота.
pub(super) struct StepEvidence {
    pub(super) name: &'static str,
    pub(super) result: Result<(), String>,
}

/// Всё, что собрал прогон.
pub(super) struct SleepEvidence {
    pub(super) steps: Vec<StepEvidence>,
    /// Запросы к серверу стенда за время прогона (фильтр по маршруту делает судья).
    pub(super) requests: Vec<MediaRequest>,
    /// Когда робот отдал вкладку плееру.
    pub(super) handed_off_at: Instant,
    /// Когда робот увидел первые кадры плеера (`None` — не увидел).
    pub(super) first_frames_at: Option<Instant>,
    /// Отобранные строки журнала плеера в порядке записи (см. `journal_needles`).
    pub(super) journal: Vec<String>,
    pub(super) host: HostLogEvidence,
    pub(super) page_report: Result<PageReport, SiteError>,
    pub(super) forbidden_player_lines: Vec<String>,
    pub(super) page_log: String,
}

/// Что сервер стенда видел от фона экстеншена.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(super) struct ServerFacts {
    /// Первый и последний ответ 503 фону на плейлист (`None` — ни одного).
    pub asleep_answers: Option<AsleepAnswers>,
    /// Был удачный плейлист фона ДО первого 503.
    pub playlist_ok_before_sleep: bool,
    /// Был удачный плейлист фона ПОСЛЕ последнего 503.
    pub playlist_ok_after_wake: bool,
    /// Сколько удачных сегментов НОВОГО окна фон скачал после последнего 503.
    pub new_window_segments_after_wake: usize,
}

/// Ответы 503 фону на плейлист.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) struct AsleepAnswers {
    pub count: usize,
    pub first_at: Instant,
    pub last_at: Instant,
}

impl AsleepAnswers {
    /// Сколько времени между первым и последним 503.
    fn span(self) -> Duration {
        self.last_at.saturating_duration_since(self.first_at)
    }
}

/// Номер сегмента по имени `seg-NNNNN.ts` в конце пути (`None` — это не сегмент эфира).
fn segment_index(path: &str) -> Option<usize> {
    let name = path.rsplit('/').next()?;
    name.strip_prefix("seg-")?.strip_suffix(".ts")?.parse().ok()
}

/// Разбирает журнал сервера: только запросы фона (скрипта) к маршруту «засыпающего» эфира.
pub(super) fn read_server_facts(requests: &[MediaRequest]) -> ServerFacts {
    let background: Vec<&MediaRequest> = requests
        .iter()
        .filter(|request| request.path.starts_with(HLS_LIVE_SLEEP_ROUTE_PREFIX))
        .filter(|request| request.requester() == MediaRequester::Script)
        .collect();
    let is_playlist = |request: &&&MediaRequest| request.path.ends_with(HLS_LIVE_PLAYLIST_NAME);
    let asleep: Vec<Instant> = background
        .iter()
        .filter(is_playlist)
        .filter(|request| request.status == HLS_LIVE_SLEEP_ASLEEP_STATUS)
        .map(|request| request.at)
        .collect();
    let asleep_answers = match (asleep.iter().min(), asleep.iter().max()) {
        (Some(&first_at), Some(&last_at)) => Some(AsleepAnswers {
            count: asleep.len(),
            first_at,
            last_at,
        }),
        _ => None,
    };
    let ok_playlist_at = |accept: &dyn Fn(Instant) -> bool| {
        background
            .iter()
            .filter(is_playlist)
            .any(|request| request.status == 200 && accept(request.at))
    };
    let threshold =
        first_segment_unseen_before_sleep(HLS_LIVE_SLEEP_SCHEDULE, HLS_LIVE_SLEEP_SEGMENT_PERIOD);
    let (playlist_ok_before_sleep, playlist_ok_after_wake, new_window_segments_after_wake) =
        match asleep_answers {
            None => (false, false, 0),
            Some(answers) => (
                ok_playlist_at(&|at| at < answers.first_at),
                ok_playlist_at(&|at| at > answers.last_at),
                background
                    .iter()
                    .filter(|request| request.at > answers.last_at)
                    .filter(|request| matches!(request.status, 200 | 206))
                    .filter(|request| segment_index(&request.path).is_some_and(|i| i >= threshold))
                    .count(),
            ),
        };
    ServerFacts {
        asleep_answers,
        playlist_ok_before_sleep,
        playlist_ok_after_wake,
        new_window_segments_after_wake,
    }
}

/// Приговор: провалы собираются все сразу, чтобы разбор шёл по одному прогону.
pub(super) fn judge_hls_live_sleep(evidence: &SleepEvidence) -> Verdict {
    let mut failures: Vec<String> = evidence
        .steps
        .iter()
        .filter_map(|step| {
            step.result
                .as_ref()
                .err()
                .map(|message| format!("step '{}': {message}", step.name))
        })
        .collect();
    let server = read_server_facts(&evidence.requests);
    let journal = read_sleep_journal(&evidence.journal);
    failures.extend(validity_failures(evidence, &server));
    failures.extend(network_wait_failures(&server, &journal));
    failures.extend(rejoin_failures(&server, &journal));
    failures.extend(
        journal
            .rejections
            .iter()
            .map(|line| format!("the player rejected the stream: {line}")),
    );
    failures.extend(host_failures(&evidence.host));
    failures.extend(page_failures(&evidence.page_report, HLS_LIVE_SLEEP_PAGE));
    failures.extend(
        unexpected_player_lines(&evidence.forbidden_player_lines)
            .map(|line| format!("forbidden player line: {line}")),
    );
    let summary = describe(&server, &journal);
    let summary = if failures.is_empty() {
        summary
    } else {
        format!("{summary}\n{}", evidence.page_log)
    };
    Verdict::from_failures(&failures, summary)
}

/// Годен ли прогон: всё, что должно быть до сна, случилось до первого 503.
fn validity_failures(evidence: &SleepEvidence, server: &ServerFacts) -> Vec<String> {
    let Some(answers) = server.asleep_answers else {
        return vec![
            "the background never got a 503 on the playlist: the server sleep was not observed"
                .to_owned(),
        ];
    };
    let mut failures = Vec::new();
    if evidence.handed_off_at >= answers.first_at {
        failures.push(
            "the stand is not valid: the tab was handed off only after the server fell asleep"
                .to_owned(),
        );
    }
    match evidence.first_frames_at {
        Some(at) if at < answers.first_at => {}
        Some(_) => failures.push(
            "the stand is not valid: the player showed its first frames only after the server fell asleep"
                .to_owned(),
        ),
        // Шаг кадров уже записал свой провал.
        None => {}
    }
    if !server.playlist_ok_before_sleep {
        failures.push(
            "the stand is not valid: the background got no playlist before the server fell asleep"
                .to_owned(),
        );
    }
    failures
}

/// Ожидание сети: долгие повторы во сне, плейлист после сна, строка «сеть вернулась».
fn network_wait_failures(server: &ServerFacts, journal: &SleepJournal) -> Vec<String> {
    let mut failures = Vec::new();
    if let Some(answers) = server.asleep_answers {
        if answers.count < MIN_ASLEEP_ANSWERS || answers.span() < MIN_OUTAGE_SPAN {
            failures.push(format!(
                "the player did not wait for the network: {} 503 answer(s) over {:?}, need {MIN_ASLEEP_ANSWERS} over at least {MIN_OUTAGE_SPAN:?}",
                answers.count,
                answers.span()
            ));
        }
        if !server.playlist_ok_after_wake {
            failures
                .push("the background never got the playlist after the server woke up".to_owned());
        }
    }
    if !journal.network_recovered {
        failures.push(
            "the player log has no 'HLS live network recovered' line: the outage was not waited out"
                .to_owned(),
        );
    }
    failures
}

/// Склейка: сегменты нового окна, строка склейки, кадры после неё, нет откатов позиции.
fn rejoin_failures(server: &ServerFacts, journal: &SleepJournal) -> Vec<String> {
    let mut failures = Vec::new();
    if server.asleep_answers.is_some() && server.new_window_segments_after_wake == 0 {
        failures.push(
            "after the wake the background fetched no segment of the new window (none it could have seen before the sleep)"
                .to_owned(),
        );
    }
    match journal.rejoin {
        None => failures.push(
            "the player log has no 'rejoined near the live edge' line: the lost window was not rejoined"
                .to_owned(),
        ),
        Some(facts) if facts.frames_gained() < MIN_FRAMES_AFTER_REJOIN => failures.push(format!(
            "the player presented only {} frame(s) after the rejoin (before: {}, after: {:?}), need {MIN_FRAMES_AFTER_REJOIN}",
            facts.frames_gained(),
            facts.frames_before,
            facts.frames_after
        )),
        Some(_) => {}
    }
    if journal.max_position_regressions > 0 {
        failures.push(format!(
            "the playback position went back {} time(s): the rejoin must not move the clock back",
            journal.max_position_regressions
        ));
    }
    failures
}

/// Строка итога: что видели сервер и плеер.
fn describe(server: &ServerFacts, journal: &SleepJournal) -> String {
    let outage = server.asleep_answers.map_or_else(
        || "no 503".to_owned(),
        |answers| format!("{} x 503 over {:?}", answers.count, answers.span()),
    );
    let rejoin = journal.rejoin.map_or_else(
        || "no rejoin".to_owned(),
        |facts| format!("rejoin, +{} frames after it", facts.frames_gained()),
    );
    format!(
        "background: {outage}, new-window segments after wake: {}; player: {rejoin}, network recovered: {}",
        server.new_window_segments_after_wake, journal.network_recovered
    )
}
