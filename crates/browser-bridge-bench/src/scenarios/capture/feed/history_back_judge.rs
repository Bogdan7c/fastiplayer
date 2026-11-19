//! Чистый приговор строки «Назад» в истории после ухода из ленты (сессия 16b3b, решения 375-376). Ввода-вывода
//! нет, каждое нарушение проверяется юнит-тестом (`history_back_judge/tests.rs`).
//!
//! Живое наблюдение (16b3, TikTok): «Назад» в истории после ухода в «Смотреть» возвращает прежний элемент с ТЕМ
//! ЖЕ MediaSource, и он играет с начала. Уход кончил сеанс (окно стоит на паузе, решение 370), фон держит запись
//! «откуда ушли» и подхватывает вернувшийся ролик новым сеансом в ТОМ ЖЕ окне. Что считается результатом:
//!  * окно создано ровно один раз за строку (процесс тот же), сеансов открыто два, второй с `grant=Started`;
//!  * до подхвата окно ничего нового не взяло: поколения первого сеанса `1, 2` (превью раздела не принято);
//!  * после подхвата окно показало ровно ОДНО поколение, шириной ролика 2 (512), с начала (`start_ms` меньше
//!    секунды), кадры растут; превью (иная ширина, лишнее поколение) не принято;
//!  * честный конец до подхвата: `notice=waiting_for_viewer`, `cause=tab_gone`, окно отвечает;
//!  * страница не получила `AbortError` от `play()`, которого сама не вызывала (решение 371).

use super::edge_judge::WindowAfterLeave;
use super::edge_judge::{EndRecord, tab_gone_notice_failure};
use super::judge::{FeedEvidence, growth_failure, metric};
use crate::observe::{
    FrameGrowth, ShownGeneration, parse_stream_generation_loaded, parse_stream_generation_shown,
    parse_stream_surface_width,
};

/// Метка журнала плеера: окно создано.
pub(super) const WINDOW_CREATED_MARKER: &str = "Browser window created";
/// Метка журнала плеера: сеанс открыт (с видом выданного пропуска `grant=`).
pub(super) const SESSION_OPENED_MARKER: &str = "Browser session opened";
/// Ширина кадра ролика 2: ролик, на котором был уход, его окно обязано показать после подхвата.
const PICKED_UP_STREAM_WIDTH: u64 = 512;
/// Подхваченный ролик идёт с начала: `start_ms` раньше этой секунды, мс.
const MAX_RESTART_START_MS: u64 = 1000;

/// Улики строки: что сделала страница и что произошло с окном и сеансами.
#[derive(Debug)]
pub(super) struct HistoryBackRecord {
    /// Страница ушла (метки ухода), вернулась по `popstate` и позвала `play()`, или причина, почему нет.
    pub stimulus: Result<(), String>,
    /// Как кончился первый сеанс (уход из ленты).
    pub end: EndRecord,
    /// Строки журнала плеера по порядку: окно создано, сеанс открыт, поколение загружено/показано, пул
    /// поверхностей перестроен.
    pub player_lines: Vec<String>,
    /// Рост кадров после подхвата или причина, почему не дождались.
    pub frame_growth: Result<FrameGrowth, String>,
    /// Строки журнала страницы `play-rejected …`.
    pub play_rejections: Vec<String>,
}

/// Что журнал плеера говорит о сеансах: строки до подхвата и после него.
struct SessionsView<'a> {
    window_created: usize,
    sessions_opened: usize,
    second_session_grant_started: bool,
    before_pickup: &'a [String],
    after_pickup: &'a [String],
}

fn sessions_view(lines: &[String]) -> SessionsView<'_> {
    let opened: Vec<usize> = lines
        .iter()
        .enumerate()
        .filter(|(_, line)| line.contains(SESSION_OPENED_MARKER))
        .map(|(index, _)| index)
        .collect();
    let second = opened.get(1).copied();
    SessionsView {
        window_created: lines
            .iter()
            .filter(|line| line.contains(WINDOW_CREATED_MARKER))
            .count(),
        sessions_opened: opened.len(),
        second_session_grant_started: second
            .is_some_and(|index| lines[index].contains("grant=Started")),
        before_pickup: &lines[..second.unwrap_or(lines.len())],
        after_pickup: second.map_or(&[], |index| &lines[index + 1..]),
    }
}

/// Приговор строки.
pub(super) fn pickup_failures(evidence: &FeedEvidence, failures: &mut Vec<String>) {
    let Some(record) = &evidence.history_back else {
        failures.push("the row recorded no history-back evidence at all".to_owned());
        return;
    };
    if let Err(message) = &record.stimulus {
        failures.push(format!(
            "the page did not leave the feed and come back through the history, so the row proves nothing: {message}"
        ));
    }
    failures.extend(
        record
            .play_rejections
            .iter()
            .filter(|line| line.contains("AbortError"))
            .map(|line| {
                format!("the page got an AbortError from play() it did not cause itself: {line}")
            }),
    );
    end_failures(&record.end, failures);
    let view = sessions_view(&record.player_lines);
    session_failures(&view, failures);
    before_pickup_failures(&view, failures);
    after_pickup_failures(&view, failures);
    failures.extend(growth_failure("after the pickup", &record.frame_growth));
    if let Ok(report) = &evidence.page_report
        && let Some(returned) = metric(report, "reel_back_started", failures)
        && returned < 1.0
    {
        failures.push("the page never saw the returned reel playing".to_owned());
    }
}

/// Первый сеанс кончился честно: «Source tab closed», окно стоит и отвечает.
fn end_failures(end: &EndRecord, failures: &mut Vec<String>) {
    match &end.notice {
        Ok(found) => failures.extend(tab_gone_notice_failure(
            found,
            WindowAfterLeave::StandsPaused,
        )),
        Err(message) => failures.push(format!(
            "the session did not end honestly (no 'Source tab closed' notice): {message}"
        )),
    }
    if let Err(message) = &end.window_after_end {
        failures.push(format!("the player window after the end: {message}"));
    }
}

/// Одно окно, два сеанса, второй — `grant=Started`.
fn session_failures(view: &SessionsView<'_>, failures: &mut Vec<String>) {
    if view.window_created != 1 {
        failures.push(format!(
            "the player created {} window(s), expected exactly one (the pickup reuses the window of the ended session)",
            view.window_created
        ));
    }
    if view.sessions_opened != 2 {
        failures.push(format!(
            "the player opened {} session(s), expected two: the original one and the pickup after the history back",
            view.sessions_opened
        ));
    } else if !view.second_session_grant_started {
        failures.push("the pickup session was not opened with grant=Started".to_owned());
    }
}

/// До подхвата окно не взяло ничего нового: поколения первого сеанса `1, 2` (превью не принято).
fn before_pickup_failures(view: &SessionsView<'_>, failures: &mut Vec<String>) {
    let generations: Vec<u64> = view
        .before_pickup
        .iter()
        .filter_map(|line| parse_stream_generation_loaded(line))
        .collect();
    if generations != [1, 2] {
        failures.push(format!(
            "the first session loaded generations {generations:?}, expected [1, 2]: the preview of the other section must not be taken for a reel"
        ));
    }
}

/// После подхвата: одно показанное поколение, ширина ролика 2, старт с начала, превью не принято.
fn after_pickup_failures(view: &SessionsView<'_>, failures: &mut Vec<String>) {
    let shown: Vec<ShownGeneration> = view
        .after_pickup
        .iter()
        .filter_map(|line| parse_stream_generation_shown(line))
        .collect();
    match shown.as_slice() {
        [] => failures.push(
            "the window showed nothing after the pickup (no 'Browser stream generation shown' line after the second session)"
                .to_owned(),
        ),
        [only] if only.start_ms >= MAX_RESTART_START_MS => failures.push(format!(
            "the returned element must play from the beginning, but the pickup started at {} ms (limit {MAX_RESTART_START_MS} ms)",
            only.start_ms
        )),
        [_] => {}
        several => failures.push(format!(
            "the window showed {} generations after the pickup, expected one: the preview of the other section was taken too",
            several.len()
        )),
    }
    let widths: Vec<u64> = view
        .after_pickup
        .iter()
        .filter_map(|line| parse_stream_surface_width(line))
        .collect();
    if widths.iter().any(|width| *width != PICKED_UP_STREAM_WIDTH) {
        failures.push(format!(
            "the window showed stream widths {widths:?} after the pickup, expected only {PICKED_UP_STREAM_WIDTH} (reel 2)"
        ));
    }
}

#[cfg(test)]
mod tests;
