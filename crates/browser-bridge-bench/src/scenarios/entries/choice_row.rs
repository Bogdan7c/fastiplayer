//! Выбор другого видео в попапе (`banner-with-main`, решение 215).
//!
//! Цепь: клик по значку отдаёт главное видео → плеер его играет → окно плеера лежит поверх
//! браузера → робот возвращает браузер вперёд и кликает ещё раз → попап остаётся и говорит
//! `in_player` → настоящие Tab и Enter выбирают строку баннера → в том же окне плеера баннер,
//! а главное видео вернулось на страницу на паузе на той же секунде. Баннер доигрывает, страница
//! сама запускает главное, и сеанс переезжает обратно на него (решение 201): плеер грузит его заново.
//!
//! Доказательства по журналам, а не по нажатию: сервер видит, что файл баннера фон запросил
//! ТОЛЬКО после выбора (до выбора его качал лишь элемент страницы), плеер выдал два пропуска
//! в ОДНОМ процессе, кадры пошли и после замены, страница подтвердила возврат главного.

use std::time::{Duration, Instant};

use super::super::capture::decoys::{DECOY_PAGES, count_fetches};
use super::super::context::{ScenarioError, Stand};
use super::super::evidence::{
    FORBIDDEN_PLAYER_LINES, SESSION_GRANTED_TIMEOUT, format_browser_log, host_failures,
    page_failures, unexpected_player_lines,
};
use super::super::handoff_run::ACK_TIMEOUT;
use super::super::outcome::Verdict;
use super::super::run_tweaks::RunTweaks;
use super::Entry;
use super::common::{
    DEFAULT_QUERY, EarlyUnsupported, Opened, POPUP_SETTLE_TIMEOUT, ReadyRun, open_ready,
};
use super::robot::wait_for_settled_popup;
use crate::browsers::{BrowserKind, PopupSnapshot};
use crate::desktop_input::Key;
use crate::environment::BenchEnvironment;
use crate::observe::{PlayerLog, parse_loads, parse_summary, read_host_evidence};
use crate::player_control::PlayerControl;

/// Сколько лишних нажатий Tab допускаем сверх номера строки (документ попапа, ссылка).
const MAX_EXTRA_TABS: usize = 3;
/// Главное видео страницы начинает играть по действию зрителя (команда `hook:resume-main`).
const RESUME_MAIN_COMMAND: &str = "hook:resume-main";
/// Случай попапа, когда видео вкладки уже в плеере.
const IN_PLAYER_NOTICE: &str = "in_player";
/// Сколько кадров плеер обязан добавить после замены (две сводки по 10 с).
const MIN_FRAMES_AFTER_REPLACE: u64 = 60;
/// Сколько ждём рост кадров после замены.
const REPLACE_FRAMES_TIMEOUT: Duration = Duration::from_secs(60);
/// Строка журнала плеера, которая появляется один раз за ПРОЦЕСС плеера.
const PLAYER_PROCESS_MARK: &str = "=== fastiplayer browser session ===";
/// Строка журнала плеера: видео доиграло до конца.
const PLAYBACK_ENDED_MARK: &str = "Browser playback ended";
/// Сколько ждём конца баннера (файл на 12 с).
const BANNER_END_TIMEOUT: Duration = Duration::from_secs(30);

/// Когда страница запускает фильм после конца баннера. Оба варианта сперва ждут, пока СТРАНИЦА
/// сама увидит `ended` у баннера (так ведёт себя живой сайт и так уходит гонка с часами плеера).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) enum FilmStart {
    /// Сразу, как только баннер сообщил `ended`.
    RightAfterBannerEnd,
    /// Через секунду после `ended`: у фильма под тенью не должно быть старого `ended`.
    SecondAfterBannerEnd,
}

impl FilmStart {
    /// Правило в подписи строки таблицы (по нему же фильтрует `BRIDGE_BENCH_ENTRY_ROW`).
    pub(super) fn rule_name(self) -> &'static str {
        match self {
            Self::RightAfterBannerEnd => "choose-another-video",
            Self::SecondAfterBannerEnd => "choose-another-video-late-film",
        }
    }

    /// Команда страницы: имя команды и id элемента, который сейчас под тенью (баннер).
    fn page_command(self) -> String {
        let name = match self {
            Self::RightAfterBannerEnd => "expect-follows-into-player",
            Self::SecondAfterBannerEnd => "expect-follows-into-player-late",
        };
        format!("{name}:{SHADOWED_ELEMENT_ID}")
    }
}

/// Id баннера на странице `banner-with-main`: именно он стоит под тенью после выбора строки.
const SHADOWED_ELEMENT_ID: &str = "banner";

pub(super) fn run(
    stand: &Stand,
    environment: &BenchEnvironment,
    browser: BrowserKind,
    entry: Entry,
    page_id: &'static str,
    film_start: FilmStart,
) -> Result<Verdict, ScenarioError> {
    let run_started_at = Instant::now();
    let mut ready = match open_ready(
        stand,
        environment,
        browser,
        page_id,
        &RunTweaks::default(),
        DEFAULT_QUERY,
        EarlyUnsupported::OnlyWhereTheTableAllows,
    )? {
        Opened::Ready(ready) => *ready,
        Opened::Finished(verdict) => return Ok(verdict),
    };
    let mut failures: Vec<String> = Vec::new();

    // 1. Первый вход: главное видео в плеер, попап закрылся сам.
    ready.enter(entry)?;
    failures.extend(ready.wait_popup_closed());
    let first_hand_off_at = Instant::now();
    ready.run.tell_page("handed-off");
    // Главное стоит на паузе до действия зрителя: «нажимаем play» и ждём кадры плеера.
    ready.run.tell_page(RESUME_MAIN_COMMAND);
    if let Err(message) = ready.run.wait_ack(RESUME_MAIN_COMMAND, ACK_TIMEOUT) {
        failures.push(format!("the page did not resume the main video: {message}"));
    }
    match ready.wait_frames()? {
        Ok(_) => {}
        Err(message) => failures.push(format!("main video: {message}")),
    }

    // 2. Окно плеера поверх браузера. Робот возвращает браузер вперёд и входит ещё раз.
    let player_on_top = ready.robot.active_window_class()?;
    ready.enter(entry)?;
    let in_player = wait_for_settled_popup(ready.run.browser.as_mut(), POPUP_SETTLE_TIMEOUT);
    let banner_row = match &in_player {
        Ok(snapshot) => inspect_in_player_popup(snapshot, &mut failures),
        Err(message) => {
            failures.push(format!("second entry: {message}"));
            None
        }
    };

    // 3. Настоящие Tab и Enter выбирают баннер.
    let control = PlayerControl::on_bus(environment.bus_address()?);
    let position_before_choice = control
        .wait_until_controllable(SESSION_GRANTED_TIMEOUT)
        .and_then(|()| control.position_micros());
    let before_replacement = last_summary(&mut ready)?;
    let mut tab_note = String::new();
    let chosen_at = Instant::now();
    if let Some(row_index) = banner_row {
        let row_text = in_player
            .as_ref()
            .ok()
            .and_then(|snapshot| snapshot.selectable_rows.get(row_index).cloned())
            .unwrap_or_default();
        tab_note = choose_row(&mut ready, row_index, &row_text, &mut failures)?;
        ready.run.player_log.skip_history()?;
    }

    // 4. Улики замены: попап закрылся, главное вернулось на страницу, кадры идут от баннера.
    if banner_row.is_some() {
        // Решение 216: окно плеера уже было, поэтому попап НЕ закрывается, а говорит «переключись
        // на окно плеера»; само окно при этом не поднимается (активным остаётся браузер).
        failures.extend(expect_popup_stays_with_switch_notice(&mut ready));
        match ready.robot.active_window_class() {
            Ok(class_after) if class_after == player_on_top => failures.push(format!(
                "the player window took the focus after the choice ({class_after}); the popup must ask the viewer to switch"
            )),
            Ok(_) => {}
            Err(error) => failures.push(format!("cannot read the active window after the choice: {error}")),
        }
        // Сначала кадры: баннер короткий и доиграет, пока страница подтверждает возврат главного.
        failures.extend(frames_after_replacement(&mut ready, before_replacement));
        match position_before_choice {
            Ok(position_us) => confirm_main_returned(&ready, position_us, &mut failures),
            Err(error) => failures.push(format!("cannot read the player position: {error}")),
        }
        // 5. Баннер доиграл, страница сама запускает главное: окно плеера живо, сеанс едет за ним.
        failures.extend(confirm_main_follows(
            &mut ready,
            environment,
            BANNER_END_TIMEOUT,
            film_start,
        )?);
    }
    let report = ready.run.finish_page();
    let page_log = format_browser_log(&ready.run.take_page_log());
    let forbidden = ready
        .run
        .player_log
        .snapshot_forbidden(FORBIDDEN_PLAYER_LINES)?;
    let _ = ready.robot.take_screenshot("end-of-row");
    let exit = ready.run.browser.shutdown()?;
    let host = read_host_evidence(&exit.stderr_log)?;

    // 6. Приговор по журналам.
    failures.extend(host_failures(&host));
    if host.session_grants != 2 {
        failures.push(format!(
            "the player granted {} session(s), expected 2 (main, then the banner)",
            host.session_grants
        ));
    }
    failures.extend(page_failures(&report, page_id));
    failures.extend(
        unexpected_player_lines(&forbidden).map(|line| format!("forbidden player line: {line}")),
    );
    let player_starts = std::fs::read_to_string(environment.player_log())
        .map(|log| log.matches(PLAYER_PROCESS_MARK).count())
        .unwrap_or(0);
    if player_starts != 1 {
        failures.push(format!(
            "the player process started {player_starts} time(s): the replacement must reuse the same window"
        ));
    }
    let banner_fetches = banner_fetch_summary(
        stand,
        run_started_at,
        first_hand_off_at,
        chosen_at,
        &mut failures,
    );
    let summary = format!(
        "window on top before the second entry: {player_on_top}; {tab_note}; {banner_fetches}; player processes: {player_starts}"
    );
    Ok(if failures.is_empty() {
        Verdict::pass(summary)
    } else {
        Verdict::fail(format!("{}\n{summary}\n{page_log}", failures.join("; ")))
    })
}

/// Случай попапа после выбора другого видео при открытом окне плеера (решение 216).
const SWITCH_TO_PLAYER_NOTICE: &str = "switch_to_player";
/// Сколько ждём, пока попап покажет новый случай.
const SWITCH_NOTICE_TIMEOUT: Duration = Duration::from_secs(30);
/// Шаг опроса попапа.
const NOTICE_POLL: Duration = Duration::from_millis(250);

/// Попап обязан остаться открытым и показать `switch_to_player` (до выбора он показывал `in_player`,
/// поэтому ждём именно смену случая).
fn expect_popup_stays_with_switch_notice(ready: &mut ReadyRun<'_, '_>) -> Option<String> {
    let deadline = Instant::now() + SWITCH_NOTICE_TIMEOUT;
    let mut last;
    loop {
        match ready.run.browser.popup_snapshot() {
            Ok(Some(snapshot)) if snapshot.notice.as_deref() == Some(SWITCH_TO_PLAYER_NOTICE) => {
                return None;
            }
            Ok(Some(snapshot)) => last = format!("popup notice {:?}", snapshot.notice),
            Ok(None) => {
                return Some("the popup closed after the choice; with the player window already open it must stay and say 'switch to the player'".to_owned());
            }
            Err(error) => last = format!("popup probe: {error}"),
        }
        if Instant::now() >= deadline {
            return Some(format!(
                "the popup never showed '{SWITCH_TO_PLAYER_NOTICE}' ({last})"
            ));
        }
        std::thread::sleep(NOTICE_POLL);
    }
}

/// Последняя сводка плеера до замены: сколько кадров показано и сколько раз плеер загружал видео.
struct SummaryMark {
    presented_frames: u64,
    loads: u64,
}

/// Последняя сводка журнала плеера (`None`, если сводок ещё нет).
fn last_summary(ready: &mut ReadyRun<'_, '_>) -> Result<Option<SummaryMark>, ScenarioError> {
    let lines = ready.run.player_log.snapshot_summary_lines()?;
    Ok(lines.last().and_then(|line| {
        Some(SummaryMark {
            presented_frames: parse_summary(line)?.presented_frames,
            loads: parse_loads(line)?,
        })
    }))
}

/// Новые кадры после замены: сводка, в которой показано заметно больше, чем в последней сводке до
/// выбора, и счётчик загрузок вырос (плеер загрузил баннер заново). Баннер короткий и играет секунды,
/// поэтому ждём не «рост между двумя сводками», а одну сводку после замены.
fn frames_after_replacement(
    ready: &mut ReadyRun<'_, '_>,
    before: Option<SummaryMark>,
) -> Option<String> {
    let Some(before) = before else {
        return Some("no player summary before the choice to compare with".to_owned());
    };
    match ready.run.player_log.wait_for_presented_frames(
        before.presented_frames + MIN_FRAMES_AFTER_REPLACE,
        REPLACE_FRAMES_TIMEOUT,
    ) {
        Ok(after) if parse_loads(&after.summary_line).is_some_and(|loads| loads > before.loads) => {
            None
        }
        Ok(after) => Some(format!(
            "frames grew after the choice but the player did not load a new video: {}",
            after.summary_line
        )),
        Err(error) => Some(format!("no new frames after the replacement: {error}")),
    }
}

/// Попап обязан сказать `in_player`, показать «в плеере» невыбираемой строкой и предложить
/// баннер выбираемой. Возвращает номер строки баннера среди выбираемых (порядок Tab).
fn inspect_in_player_popup(snapshot: &PopupSnapshot, failures: &mut Vec<String>) -> Option<usize> {
    if snapshot.notice.as_deref() != Some(IN_PLAYER_NOTICE) {
        failures.push(format!(
            "the popup says {:?}, expected '{IN_PLAYER_NOTICE}'",
            snapshot.notice
        ));
    }
    if snapshot.static_rows.is_empty() {
        failures.push(
            "the video that is already in the player is not shown as a static row".to_owned(),
        );
    }
    let banner = banner_row_index(&snapshot.selectable_rows);
    if banner.is_none() {
        failures.push("the popup offers no other video to choose".to_owned());
    }
    banner
}

/// Номер строки, которую выбирает человек: первая из выбираемых. Баннер и превью играют один файл,
/// и их строки в попапе совпадают по тексту (заголовок вкладки, размер файла и длительность), так что
/// любая из них — «другое видео»; главное в плеере и выбираемой строкой не бывает (оно статичное).
fn banner_row_index(selectable_rows: &[String]) -> Option<usize> {
    if selectable_rows.is_empty() {
        None
    } else {
        Some(0)
    }
}

/// Tab до строки (кнопки `video-row` с нужным текстом) и Enter.
fn choose_row(
    ready: &mut ReadyRun<'_, '_>,
    row_index: usize,
    row_text: &str,
    failures: &mut Vec<String>,
) -> Result<String, ScenarioError> {
    let walk = ready.robot.press_tab_until(
        ready.run.browser.as_mut(),
        row_index + MAX_EXTRA_TABS,
        |snapshot| {
            snapshot.focused.as_ref().is_some_and(|focus| {
                focus.tag == "button" && focus.class.contains("video-row") && focus.text == row_text
            })
        },
    )?;
    if !walk.reached {
        failures.push(format!(
            "after {} Tab the focus never reached the row {row_index}: {:?}",
            walk.presses,
            walk.last_snapshot.map(|snapshot| snapshot.focused)
        ));
    }
    ready.robot.press(Key::Enter)?;
    Ok(format!("Tab x{} to the row", walk.presses))
}

/// Главное видео вернулось на страницу на паузе примерно на той же секунде: страница сама
/// проверяет это командой `expect-returned-paused` (те же правила, что при закрытии окна плеера).
/// Вторая половина обычной проверки возврата здесь неверна: окно плеера живо, и `play()` страницы
/// уводит видео в плеер, а не запускает его в браузере.
fn confirm_main_returned(ready: &ReadyRun<'_, '_>, position_us: i64, failures: &mut Vec<String>) {
    let seconds = position_us as f64 / 1_000_000.0;
    let command = format!("expect-returned-paused:{seconds:.2}");
    ready.run.tell_page(&command);
    if let Err(message) = ready.run.wait_ack(&command, ACK_TIMEOUT) {
        failures.push(format!(
            "the page did not confirm the return of the main video: {message}"
        ));
    }
}

/// Страница сама запускает вернувшееся главное видео после конца баннера: переезд сеанса открыт
/// только теперь. Страница подтверждает ход часов без декода, журнал плеера — новую загрузку и кадры.
fn confirm_main_follows(
    ready: &mut ReadyRun<'_, '_>,
    environment: &BenchEnvironment,
    banner_end_timeout: Duration,
    film_start: FilmStart,
) -> Result<Option<String>, ScenarioError> {
    // Отдельный читатель журнала: общий курсор мог уйти дальше строки конца, пока ждал сводку.
    let mut whole_log = PlayerLog::open(environment.player_log());
    if let Err(error) = whole_log.wait_for_line(PLAYBACK_ENDED_MARK, banner_end_timeout) {
        return Ok(Some(format!(
            "the banner never ended in the player: {error}"
        )));
    }
    let before_following = last_summary(ready)?;
    // Страница сама дожидается `ended` баннера (см. handoff-mode.js): строка журнала плеера приходит
    // раньше, чем тик часов доходит до фона, и без этого ожидания фон отвечал бы `release`.
    let command = film_start.page_command();
    ready.run.tell_page(&command);
    // Ответ страницы приходит после ожидания конца баннера, запуска фильма и наблюдения.
    if let Err(message) = ready.run.wait_ack(&command, ACK_TIMEOUT) {
        return Ok(Some(format!(
            "the page did not start the main video: {message}"
        )));
    }
    Ok(frames_after_replacement(ready, before_following))
}

/// Файл баннера: фон НЕ качал его до выбора и качал после. Возвращает строку сводки.
fn banner_fetch_summary(
    stand: &Stand,
    run_started_at: Instant,
    first_hand_off_at: Instant,
    chosen_at: Instant,
    failures: &mut Vec<String>,
) -> String {
    let Some(decoy) = DECOY_PAGES.first() else {
        failures.push("the decoy table has no page".to_owned());
        return "decoy table empty".to_owned();
    };
    let mut requests = stand.site.media_requests();
    requests.retain(|request| request.at >= run_started_at);
    let (before, after): (Vec<_>, Vec<_>) = requests
        .into_iter()
        .partition(|request| request.at < chosen_at);
    let before_counts = count_fetches(decoy.file_path, &before, first_hand_off_at);
    let after_counts = count_fetches(decoy.file_path, &after, chosen_at);
    if before_counts.element_requests == 0 {
        failures.push(
            "the page's own <video> never loaded the banner file: the ruler is blind".to_owned(),
        );
    }
    if before_counts.background_requests > 0 {
        failures.push(format!(
            "the background fetched the banner file {} time(s) BEFORE it was chosen",
            before_counts.background_requests
        ));
    }
    if after_counts.background_requests == 0 {
        failures
            .push("the background never fetched the banner file after it was chosen".to_owned());
    }
    format!(
        "banner file {}: background requests before the choice={}, after={}",
        decoy.file_path, before_counts.background_requests, after_counts.background_requests
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    fn rows(items: &[&str]) -> Vec<String> {
        items.iter().map(|item| (*item).to_owned()).collect()
    }

    #[test]
    fn film_start_variants_have_distinct_rules_and_address_the_banner() {
        assert_eq!(
            FilmStart::RightAfterBannerEnd.rule_name(),
            "choose-another-video"
        );
        assert_eq!(
            FilmStart::SecondAfterBannerEnd.rule_name(),
            "choose-another-video-late-film"
        );
        assert_eq!(
            FilmStart::RightAfterBannerEnd.page_command(),
            "expect-follows-into-player:banner"
        );
        assert_eq!(
            FilmStart::SecondAfterBannerEnd.page_command(),
            "expect-follows-into-player-late:banner"
        );
    }

    #[test]
    fn the_first_selectable_row_is_chosen_and_an_empty_list_has_none() {
        let selectable = rows(&["tab 640×360 · 0:12", "tab 640×360 · 0:12"]);
        assert_eq!(banner_row_index(&selectable), Some(0));
        assert_eq!(banner_row_index(&[]), None);
    }

    #[test]
    fn the_in_player_popup_without_a_banner_row_is_a_failure() {
        let snapshot = PopupSnapshot {
            notice: Some("in_player".to_owned()),
            selectable_rows: Vec::new(),
            static_rows: rows(&["main"]),
            has_consent_question: false,
            has_reload_button: false,
            focused: None,
        };
        let mut failures = Vec::new();
        assert_eq!(inspect_in_player_popup(&snapshot, &mut failures), None);
        assert_eq!(failures.len(), 1);
        let wrong_notice = PopupSnapshot {
            notice: Some("failed".to_owned()),
            ..snapshot
        };
        let mut failures = Vec::new();
        inspect_in_player_popup(&wrong_notice, &mut failures);
        assert!(
            failures
                .iter()
                .any(|failure| failure.contains("expected 'in_player'"))
        );
    }
}
