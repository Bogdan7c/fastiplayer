//! Общие звенья строк `auto-handoff`: открыть страницу с настоящей рукой, положить настройку режима 2
//! в экстеншен ДО загрузки страницы, кликнуть или навести мышь на цель, отметить и доложить окно
//! наблюдения страницы, дождаться передачи (пропуск посредника и кадры плеера), собрать итоговые улики.
//! Строки (`rows_*.rs`) только раскладывают эти звенья по своим правилам.

use std::collections::HashMap;
use std::time::{Duration, Instant};

use super::super::capture::{FRAMES_TIMEOUT, MIN_PRESENTED_FRAMES, judge_early_report};
use super::super::context::{ScenarioError, Stand};
use super::super::entries::return_hand::{HandOpened, PlaceholderView, ReturnHand, open_hand};
use super::super::evidence::{
    FORBIDDEN_PLAYER_LINES, SESSION_GRANTED_TIMEOUT, format_browser_log, page_failures,
    unexpected_player_lines,
};
use super::super::handoff_run::{ACK_TIMEOUT, HANDOFF_MODE_QUERY, HandoffRun, PageReadiness};
use super::super::outcome::Verdict;
use super::facts::{
    PLAY_CLICKED_LINE, StateFacts, WindowFacts, parse_state, parse_window, state_needle,
    window_needle,
};
use super::settings::{SettingsPreset, site_key_of_origin, write_settings};
use crate::browsers::BrowserKind;
use crate::desktop_input::Point;
use crate::environment::BenchEnvironment;
use crate::observe::{HostLogEvidence, read_host_evidence};
use crate::player_control::PlayerControl;
use crate::site::{PageReport, RunId, SiteError};

/// Сколько даём экстеншену подхватить записанную настройку (`storage.onChanged` → охрана страницы).
/// Страница после записи открывается заново, поэтому срок нужен только для самого хранилища.
pub(super) const SETTINGS_SETTLE: Duration = Duration::from_secs(2);
/// Сколько ждём заглушку после передачи: сигнал MAIN-мира и первый перемер приходят за доли секунды.
const PLACEHOLDER_SHOWN_TIMEOUT: Duration = Duration::from_secs(10);
/// Сколько ждём, что заглушки нет (передача кончилась, панель спрятана).
const PLACEHOLDER_GONE_TIMEOUT: Duration = Duration::from_secs(10);
/// Шаг опроса зонда слоя страницы.
const PLACEHOLDER_POLL: Duration = Duration::from_millis(250);
/// Сколько ждём, пока окно плеера станет управляемым.
const CONTROLLABLE_TIMEOUT: Duration = Duration::from_secs(30);
/// Шаг опроса журнала страницы и посредника.
const POLL_STEP: Duration = Duration::from_millis(150);
/// Сколько ждём строку журнала страницы, которая могла прийти позже подтверждения своей команды
/// (каждая строка — свой запрос страницы к стенду, порядок прихода не гарантирован).
const LATE_PAGE_LINE_TIMEOUT: Duration = Duration::from_secs(5);
/// На сколько должны вырасти кадры плеера между двумя сводками ПОСЛЕ новой передачи
/// (сводки раз в 10 с, 25 к/с: целый интервал даёт ~250; берём меньше четверти).
const MIN_FRAMES_GROWTH: u64 = 60;
/// Пауза между подходом мыши к краю экрана и к цели при наведении: браузер обязан увидеть выход и вход.
const HOVER_ENTER_PAUSE: Duration = Duration::from_millis(400);
/// Куда мышь уходит перед наведением: правый нижний угол видимой части страницы (доли ширины и высоты).
const FAR_CORNER_SHARE: f64 = 0.95;

/// Идущая строка: рука, пульт окна плеера и накопленные провалы и заметки.
pub(super) struct AutoSession<'a, 'env> {
    hand: ReturnHand<'a, 'env>,
    control: PlayerControl,
    pub(super) failures: Vec<String>,
    pub(super) notes: Vec<String>,
    /// Сколько докладов окон и снимков состояния команда уже запросила у каждой страницы (по прогону).
    report_counters: HashMap<String, ReportCounters>,
}

#[derive(Debug, Default, Clone, Copy)]
struct ReportCounters {
    windows: u32,
    states: u32,
}

/// Чем кончилось начало строки.
pub(super) enum Begun<'a, 'env> {
    /// Страница открыта (после настройки), рука на месте.
    Ready(Box<AutoSession<'a, 'env>>),
    /// До страницы с видео не дошло (`unsupported` там, где разрешено, или звено цепи не сработало).
    Finished(Verdict),
}

/// Открывает страницу, кладёт настройку режима 2 и открывает страницу ЗАНОВО: охрана страницы
/// включается на старте документа, и «человек включил режим, потом открыл сайт» — это именно так.
/// Строка без настройки (`UntouchedDefault`) ничего не пишет и не перезагружает страницу.
pub(super) fn begin<'a, 'env>(
    stand: &'a Stand,
    environment: &'env BenchEnvironment,
    browser: BrowserKind,
    page_id: &'static str,
    preset: SettingsPreset,
) -> Result<Begun<'a, 'env>, ScenarioError> {
    let mut hand = match open_hand(stand, environment, browser, page_id, HANDOFF_MODE_QUERY)? {
        HandOpened::Ready(hand) => *hand,
        HandOpened::Finished(verdict) => return Ok(Begun::Finished(verdict)),
    };
    // Служебная страница экстеншена: через неё пишется настройка и читается слой страницы.
    hand.watch_page_layer()?;
    let site_key = match site_key_of_origin(&stand.site.main_origin()) {
        Ok(key) => key,
        Err(problem) => return abort(hand, problem).map(Begun::Finished),
    };
    if let Some(setup) = preset.setup(&site_key) {
        if let Err(problem) = write_settings(hand.run().browser.as_mut(), &setup) {
            return abort(hand, problem).map(Begun::Finished);
        }
        std::thread::sleep(SETTINGS_SETTLE);
        hand.run().open_next_page(page_id, page_id)?;
        hand.run().activate_page_tab()?;
        match hand.run().wait_page_ready() {
            PageReadiness::Ready => {}
            PageReadiness::ReportedEarly(report) => {
                let verdict = judge_early_report(page_id, browser, &report);
                hand.into_run().browser.shutdown()?;
                return Ok(Begun::Finished(verdict));
            }
            PageReadiness::TimedOut => {
                return abort(
                    hand,
                    "the page never became ready after the settings".into(),
                )
                .map(Begun::Finished);
            }
        }
    }
    let control = PlayerControl::on_bus(environment.bus_address()?);
    Ok(Begun::Ready(Box::new(AutoSession {
        hand,
        control,
        failures: Vec::new(),
        notes: Vec::new(),
        report_counters: HashMap::new(),
    })))
}

/// Провал до начала строки: журнал страницы в текст, браузер закрыт.
fn abort(hand: ReturnHand<'_, '_>, message: String) -> Result<Verdict, ScenarioError> {
    let run = hand.into_run();
    let log = format_browser_log(&run.take_page_log());
    run.browser.shutdown()?;
    Ok(Verdict::fail(format!("{message}\n{log}")))
}

impl<'a> AutoSession<'a, '_> {
    /// Прогон текущей страницы.
    pub(super) fn run(&mut self) -> &mut HandoffRun<'a> {
        self.hand.run()
    }

    /// Прогон текущей страницы (её команды и журнал).
    pub(super) fn current_run(&self) -> RunId {
        self.hand.run_ref().run.clone()
    }

    /// Адрес текущей вкладки для зонда слоя: путь страницы и `?run=<прогон>`. Прогон в адресе нужен,
    /// когда открыты несколько вкладок одной страницы: по одному пути нашлась бы первая попавшаяся.
    pub(super) fn current_prefix(&self) -> String {
        let page = self.hand.run_ref();
        format!("{}?run={}", page.page_url_prefix(), page.run)
    }

    /// Записать провал шага (`Err`) и вернуть значение удавшегося шага.
    pub(super) fn record<T>(&mut self, result: Result<T, String>) -> Option<T> {
        match result {
            Ok(value) => Some(value),
            Err(problem) => {
                self.failures.push(problem);
                None
            }
        }
    }

    // --- рука ----------------------------------------------------------------------------------

    /// Точка экрана в центре цели зонда страницы `target` (свежая калибровка по прогону `run`).
    fn centre_of(&self, run: &RunId, target: &str) -> Result<Point, String> {
        let place = self.hand.locate(run).map_err(|error| error.to_string())?;
        let area = place
            .target_rect(target)
            .ok_or_else(|| format!("the page reported no '{target}' target to aim at"))?;
        Ok(place.screen_point_in(area, 0.5, 0.5))
    }

    /// Настоящий левый клик в центр цели `target` страницы прогона `run`.
    pub(super) fn click_target(&mut self, run: &RunId, target: &str) -> Result<(), String> {
        let point = self.centre_of(run, target)?;
        self.hand
            .click(point)
            .map_err(|error| format!("clicking '{target}': {error}"))
    }

    /// Настоящее наведение мыши на цель: мышь уходит в дальний угол страницы и приходит в центр цели,
    /// чтобы браузер увидел именно ВХОД в элемент (`mouseover`), а не стояние уже внутри него.
    pub(super) fn hover_target(&mut self, run: &RunId, target: &str) -> Result<(), String> {
        let place = self.hand.locate(run).map_err(|error| error.to_string())?;
        let area = place
            .target_rect(target)
            .ok_or_else(|| format!("the page reported no '{target}' target to aim at"))?;
        let viewport = place
            .target_rect("viewport")
            .ok_or("the page reported no viewport")?;
        let far = place.screen_point_in(viewport, FAR_CORNER_SHARE, FAR_CORNER_SHARE);
        let over = place.screen_point_in(area, 0.5, 0.5);
        self.hand
            .move_pointer(far)
            .map_err(|error| format!("moving the mouse away: {error}"))?;
        std::thread::sleep(HOVER_ENTER_PAUSE);
        self.hand
            .move_pointer(over)
            .map_err(|error| format!("hovering '{target}': {error}"))
    }

    /// Центр кнопки «Return» заглушки на странице прогона `run`: ждёт заглушку, калибрует экран.
    pub(super) fn return_button_point(
        &mut self,
        run: &RunId,
        prefix: &str,
    ) -> Result<Point, String> {
        let placeholder = self.wait_placeholder_shown(prefix).inspect_err(|_| {
            // Снимок экрана при провале: видно, есть ли заглушка вообще и что её закрывает.
            self.hand.screenshot("no-return-button");
        })?;
        let button = placeholder
            .return_button
            .ok_or("the placeholder has no Return button")?;
        let place = self.hand.locate(run).map_err(|error| error.to_string())?;
        Ok(place.screen_point_in(button, 0.5, 0.5))
    }

    /// Настоящий клик в точку экрана.
    pub(super) fn click_point(&self, point: Point) -> Result<(), String> {
        self.hand
            .click(point)
            .map_err(|error| format!("clicking at {point:?}: {error}"))
    }

    // --- команды странице -----------------------------------------------------------------------

    /// Команда странице прогона `run` и ожидание её НОВОГО подтверждения: одна и та же команда
    /// приходит несколько раз за прогон, а `ack <команда>` в журнале остаётся от прошлых раз.
    pub(super) fn command_and_wait(&self, run: &RunId, command: &str) -> Result<(), String> {
        let page = self.hand.run_ref();
        let needle = format!("ack {command}");
        let acks_before = page.log_lines_of(run, &needle).len();
        page.tell_page_of(run, command);
        let deadline = Instant::now() + ACK_TIMEOUT;
        loop {
            if page.log_lines_of(run, &needle).len() > acks_before {
                return Ok(());
            }
            if Instant::now() >= deadline {
                return Err(format!(
                    "the page did not acknowledge '{command}' within {ACK_TIMEOUT:?}"
                ));
            }
            std::thread::sleep(POLL_STEP);
        }
    }

    /// Начать окно наблюдения на странице прогона `run`.
    pub(super) fn mark_window(&self, run: &RunId) -> Result<(), String> {
        self.command_and_wait(run, "hook:mark-window")
    }

    /// Закрыть окно наблюдения и получить факты страницы прогона `run`.
    pub(super) fn report_window(&mut self, run: &RunId) -> Result<WindowFacts, String> {
        let counters = self.report_counters.entry(run.to_string()).or_default();
        counters.windows += 1;
        let seq = counters.windows;
        self.command_and_wait(run, "hook:report-window")?;
        let line = self.late_line(run, &window_needle(seq))?;
        parse_window(&line)
    }

    /// Снимок состояния видео страницы прогона `run`.
    pub(super) fn report_state(&mut self, run: &RunId) -> Result<StateFacts, String> {
        let counters = self.report_counters.entry(run.to_string()).or_default();
        counters.states += 1;
        let seq = counters.states;
        self.command_and_wait(run, "hook:report-state")?;
        let line = self.late_line(run, &state_needle(seq))?;
        parse_state(&line)
    }

    /// Строка журнала страницы `run`, которая могла прийти позже подтверждения команды.
    fn late_line(&self, run: &RunId, needle: &str) -> Result<String, String> {
        self.hand
            .run_ref()
            .wait_page_log_line_of(run, needle, LATE_PAGE_LINE_TIMEOUT)
            .map_err(|problem| format!("no '{needle}' line from the page: {problem}"))
    }

    /// Строки журнала страницы прогона `run` про клики по Play.
    pub(super) fn play_click_lines(&self, run: &RunId) -> Vec<String> {
        self.hand.run_ref().log_lines_of(run, PLAY_CLICKED_LINE)
    }

    /// Строки журнала страницы прогона `run`, содержащие `needle`.
    pub(super) fn page_lines(&self, run: &RunId, needle: &str) -> Vec<String> {
        self.hand.run_ref().log_lines_of(run, needle)
    }

    // --- заглушка -------------------------------------------------------------------------------

    /// Заглушка на вкладке `prefix` глазами зонда слоя экстеншена.
    pub(super) fn placeholder(&mut self, prefix: &str) -> Result<PlaceholderView, String> {
        self.hand.placeholder_on(prefix)
    }

    /// Ждёт заглушку: передача идёт, панель видна, кнопка «Return» на месте.
    pub(super) fn wait_placeholder_shown(
        &mut self,
        prefix: &str,
    ) -> Result<PlaceholderView, String> {
        let deadline = Instant::now() + PLACEHOLDER_SHOWN_TIMEOUT;
        loop {
            let last = self.hand.placeholder_on(prefix);
            if let Ok(view) = &last
                && view.handed_off
                && view.visible
                && view.return_button.is_some()
            {
                return Ok(*view);
            }
            if Instant::now() >= deadline {
                return Err(format!(
                    "no placeholder with a Return button within {PLACEHOLDER_SHOWN_TIMEOUT:?}; last probe: {last:?}"
                ));
            }
            std::thread::sleep(PLACEHOLDER_POLL);
        }
    }

    /// Ждёт, что заглушки нет: передача кончилась, панель спрятана.
    pub(super) fn wait_placeholder_gone(&mut self, prefix: &str) -> Result<(), String> {
        let deadline = Instant::now() + PLACEHOLDER_GONE_TIMEOUT;
        loop {
            let last = self.hand.placeholder_on(prefix);
            if let Ok(view) = &last
                && !view.handed_off
                && !view.visible
            {
                return Ok(());
            }
            if Instant::now() >= deadline {
                return Err(format!(
                    "the placeholder is still there {PLACEHOLDER_GONE_TIMEOUT:?} later: {last:?}"
                ));
            }
            std::thread::sleep(PLACEHOLDER_POLL);
        }
    }

    // --- плеер ----------------------------------------------------------------------------------

    /// Отметки посредника в stderr браузера сейчас.
    pub(super) fn host_now(&mut self) -> Result<HostLogEvidence, String> {
        read_host_evidence(&self.hand.run().stderr_log).map_err(|error| error.to_string())
    }

    /// Ждёт, пока посредник выдаст плееру `grants`-й по счёту пропуск на сеанс за весь прогон.
    fn wait_grants(&mut self, grants: u32) -> Result<(), String> {
        let deadline = Instant::now() + SESSION_GRANTED_TIMEOUT;
        loop {
            let seen = self.host_now()?.session_grants;
            if seen >= grants {
                return Ok(());
            }
            if Instant::now() >= deadline {
                return Err(format!(
                    "the host granted {seen} session(s) within {SESSION_GRANTED_TIMEOUT:?}, expected {grants}"
                ));
            }
            std::thread::sleep(POLL_STEP);
        }
    }

    /// Передача состоялась: `grants`-й пропуск посредника, кадры в рендере плеера, окно управляемо.
    /// Первый сеанс доказывается показанными кадрами (`presented_frames`), следующие — РОСТОМ кадров
    /// после пропуска: сводка плеера накопительная и от прошлого сеанса уже содержала бы «50 кадров».
    pub(super) fn await_handoff(&mut self, grants: u32) -> Result<(), String> {
        self.wait_grants(grants)?;
        let player_log = &mut self.hand.run().player_log;
        let proof = if grants == 1 {
            player_log
                .wait_for_presented_frames(MIN_PRESENTED_FRAMES, FRAMES_TIMEOUT)
                .map(|found| format!("player presented {} frames", found.presented_frames))
        } else {
            player_log
                .skip_history()
                .and_then(|()| player_log.wait_for_frame_growth(MIN_FRAMES_GROWTH, FRAMES_TIMEOUT))
                .map(|growth| {
                    format!(
                        "player frames grew by {} after session #{grants}",
                        growth.frames_gained()
                    )
                })
        }
        .map_err(|error| format!("session #{grants}: player did not present frames: {error}"))?;
        self.notes.push(proof);
        self.control
            .wait_until_controllable(CONTROLLABLE_TIMEOUT)
            .map_err(|error| format!("the player window never became controllable: {error}"))
    }

    /// Окно плеера закрылось за `timeout`; возвращает, сколько это заняло.
    pub(super) fn wait_window_gone(&self, timeout: Duration) -> Result<Duration, String> {
        let started = Instant::now();
        self.control
            .wait_until_gone(timeout)
            .map(|()| started.elapsed())
            .map_err(|error| format!("the player window did not close within {timeout:?}: {error}"))
    }

    // --- итог -----------------------------------------------------------------------------------

    /// Итог строки: отчёты страниц (текущей и `other_pages`), журнал плеера, посредник; браузер закрыт.
    pub(super) fn close(
        self,
        other_pages: &[(&RunId, &str)],
    ) -> Result<(RowEnd, Vec<String>, Vec<String>), ScenarioError> {
        let AutoSession {
            hand,
            failures,
            notes,
            ..
        } = self;
        let mut run = hand.into_run();
        let current_page = page_id_of(&run.page_url_prefix()).to_owned();
        let current_report = run.finish_page();
        let mut evidence = page_failures(&current_report, &current_page);
        for (other_run, page_id) in other_pages {
            evidence.extend(page_failures(&run.finish_page_of(other_run), page_id));
        }
        let page_log = format_browser_log(&run.take_page_log());
        let forbidden = run.player_log.snapshot_forbidden(FORBIDDEN_PLAYER_LINES)?;
        let exit = run.browser.shutdown()?;
        let host = read_host_evidence(&exit.stderr_log)?;
        evidence.extend(
            unexpected_player_lines(&forbidden)
                .map(|line| format!("forbidden player line: {line}")),
        );
        Ok((
            RowEnd {
                host,
                page_log,
                current_report,
                evidence_failures: evidence,
            },
            failures,
            notes,
        ))
    }
}

/// Идентификатор страницы из адреса `…/site/pages/<id>.html`.
fn page_id_of(prefix: &str) -> &str {
    prefix
        .rsplit('/')
        .next()
        .and_then(|file| file.strip_suffix(".html"))
        .unwrap_or(prefix)
}

/// Улики после закрытия браузера.
pub(super) struct RowEnd {
    pub(super) host: HostLogEvidence,
    pub(super) page_log: String,
    /// Итоговый отчёт текущей страницы (для порогов по метрикам).
    pub(super) current_report: Result<PageReport, SiteError>,
    /// Провалы общих улик: отчёты страниц, запрещённые строки плеера.
    pub(super) evidence_failures: Vec<String>,
}

/// Вердикт строки: провалы шагов + провалы улик; при провале к сводке — журнал страницы.
pub(super) fn verdict(end: &RowEnd, mut failures: Vec<String>, notes: &[String]) -> Verdict {
    failures.extend(end.evidence_failures.iter().cloned());
    let summary = notes.join("; ");
    if failures.is_empty() {
        Verdict::pass(summary)
    } else {
        Verdict::fail(format!(
            "{}\n{summary}\n{}",
            failures.join("; "),
            end.page_log
        ))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::scenarios::outcome::Outcome;

    fn end_with(evidence_failures: Vec<String>) -> RowEnd {
        RowEnd {
            host: HostLogEvidence::default(),
            page_log: "page log (1 lines)".to_owned(),
            current_report: Err(SiteError::InvalidRunId("no-report".to_owned())),
            evidence_failures,
        }
    }

    #[test]
    fn the_page_id_comes_from_the_page_address() {
        assert_eq!(
            page_id_of("http://127.0.0.1:7/site/pages/auto-film.html"),
            "auto-film"
        );
        assert_eq!(page_id_of("weird"), "weird");
    }

    #[test]
    fn a_failing_row_shows_its_problems_the_notes_and_the_page_log() {
        let end = end_with(vec!["page report is Fail".to_owned()]);
        let verdict = verdict(
            &end,
            vec!["no placeholder".to_owned()],
            &["note".to_owned()],
        );
        assert_eq!(verdict.outcome, Outcome::Fail);
        assert!(verdict.detail.contains("no placeholder"));
        assert!(verdict.detail.contains("page report is Fail"));
        assert!(verdict.detail.contains("note"));
        assert!(verdict.detail.contains("page log"));
    }

    #[test]
    fn a_row_without_problems_passes_with_its_notes() {
        let clean = end_with(Vec::new());
        assert_eq!(
            verdict(&clean, Vec::new(), &["a".to_owned(), "b".to_owned()]),
            Verdict::pass("a; b")
        );
    }
}
