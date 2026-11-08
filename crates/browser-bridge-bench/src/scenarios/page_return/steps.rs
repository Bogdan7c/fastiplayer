//! Общие звенья строк `page-return`: передать вкладку и доказать, что сеанс идёт, найти заглушку и
//! кнопку «Return» на экране, отметить секунду страницы, собрать факты возврата и итоговые улики.
//! Строки (`rows_*.rs`) только раскладывают эти звенья по своим правилам.

use std::time::{Duration, Instant};

use super::facts::{
    PLAYER_END_NEEDLES, PlayerEndFacts, RETURN_FACTS_LINE, RETURN_MARK_LINE, ReturnFacts,
    parse_return_facts, parse_return_mark,
};
use crate::browsers::{BrowserKind, HandOffOutcome};
use crate::desktop_input::Point;
use crate::environment::BenchEnvironment;
use crate::observe::{HostLogEvidence, read_host_evidence};
use crate::player_control::PlayerControl;
use crate::scenarios::context::{ScenarioError, Stand};
use crate::scenarios::entries::return_hand::{
    ClientBox, HandOpened, PlaceholderView, ReturnHand, open_hand,
};
use crate::scenarios::evidence::{
    FORBIDDEN_PLAYER_LINES, format_browser_log, host_failures, page_failures,
    unexpected_player_lines,
};
use crate::scenarios::handoff_run::{ACK_TIMEOUT, HANDOFF_MODE_QUERY, HandoffRun};
use crate::scenarios::outcome::Verdict;
use crate::scenarios::page_ends::start_of_session;
use crate::site::{PageReport, RunId, SiteError};

/// Страница большинства строк: рабочий MSE-плеер сайта с хуками (`site/pages/return-kit.html`).
pub(super) const RETURN_KIT_PAGE: &str = "return-kit";
/// Сколько ждём заглушку после передачи: сигнал MAIN-мира и первый перемер (`PLACEHOLDER_LAYOUT_RECHECK_MS`
/// = 250 мс) приходят за доли секунды; запас — на медленную машину и опрос служебной страницы.
const PLACEHOLDER_SHOWN_TIMEOUT: Duration = Duration::from_secs(10);
/// Сколько ждём, что заглушка ушла после конца сеанса (тень снимается за доли секунды).
pub(super) const PLACEHOLDER_GONE_TIMEOUT: Duration = Duration::from_secs(10);
/// Шаг опроса зонда слоя страницы.
const PLACEHOLDER_POLL: Duration = Duration::from_millis(250);
/// Окно после «Return» обязано закрыться сразу (решение 236). Процессный тест плеера даёт 4 с от
/// закрытия связи до выхода; здесь ещё клик, путь экстеншена и опрос шины — 6 с с запасом, а
/// доигрывание скачанного (~10 с буфера страницы) в этот срок не влезло бы.
pub(super) const RETURN_WINDOW_GONE_TIMEOUT: Duration = Duration::from_secs(6);
/// Сколько окно может закрываться после `Quit` (прощание со страницей, выход процесса).
pub(super) const CLOSE_WINDOW_GONE_TIMEOUT: Duration = Duration::from_secs(20);

/// Сколько ждём строку журнала страницы, которая могла прийти позже подтверждения своей команды
/// (каждая строка — свой запрос страницы к стенду, порядок прихода не гарантирован).
const LATE_PAGE_LINE_TIMEOUT: Duration = Duration::from_secs(5);

/// Идущий сеанс строки: рука, пульт окна плеера и накопленные провалы и заметки.
pub(super) struct Session<'a, 'env> {
    pub(super) hand: ReturnHand<'a, 'env>,
    pub(super) control: PlayerControl,
    pub(super) failures: Vec<String>,
    pub(super) notes: Vec<String>,
}

/// Чем кончилось начало строки.
pub(super) enum Begun<'a, 'env> {
    /// Сеанс идёт: кадры в плеере, окно управляемо, слой экстеншена читается.
    Running(Box<Session<'a, 'env>>),
    /// До сеанса не дошло (страница `unsupported` там, где разрешено, или звено цепи не сработало).
    Finished(Verdict),
}

/// Хвост адреса страницы второй вкладки замены: своя секунда старта.
pub(super) fn query_starting_at(start_s: u32) -> String {
    format!("{HANDOFF_MODE_QUERY}&start_s={start_s}")
}

/// Открывает страницу с рукой, отдаёт вкладку роботом (решение 175) и доказывает сеанс: пропуск
/// посредника, кадры в плеере, окно управляемо. Затем открывает служебную страницу для зонда слоя.
pub(super) fn begin<'a, 'env>(
    stand: &'a Stand,
    environment: &'env BenchEnvironment,
    browser: BrowserKind,
    page_id: &'static str,
    query: &str,
) -> Result<Begun<'a, 'env>, ScenarioError> {
    let mut hand = match open_hand(stand, environment, browser, page_id, query)? {
        HandOpened::Ready(hand) => *hand,
        HandOpened::Finished(verdict) => return Ok(Begun::Finished(verdict)),
    };
    match hand.run().hand_off() {
        Ok(HandOffOutcome::Started) => {}
        other => return abort(hand, format!("hand_off_tab: {other:?}")).map(Begun::Finished),
    }
    hand.run().tell_page("handed-off");
    let control = PlayerControl::on_bus(environment.bus_address()?);
    if let Err(message) = start_of_session(hand.run(), &control)? {
        return abort(hand, message).map(Begun::Finished);
    }
    hand.watch_page_layer()?;
    Ok(Begun::Running(Box::new(Session {
        hand,
        control,
        failures: Vec::new(),
        notes: Vec::new(),
    })))
}

/// Провал до начала сеанса: журнал страницы в текст, браузер закрыт.
fn abort(hand: ReturnHand<'_, '_>, message: String) -> Result<Verdict, ScenarioError> {
    let run = hand.into_run();
    let log = format_browser_log(&run.take_page_log());
    run.browser.shutdown()?;
    Ok(Verdict::fail(format!("{message}\n{log}")))
}

/// Куда кликать «Return»: заглушка, точка кнопки на экране и прямоугольник видео на миг калибровки.
pub(super) struct ReturnAim {
    pub(super) placeholder: PlaceholderView,
    pub(super) video: Option<ClientBox>,
    pub(super) button_point: Point,
}

impl<'a> Session<'a, '_> {
    /// Прогон текущей страницы.
    pub(super) fn run(&mut self) -> &mut HandoffRun<'a> {
        self.hand.run()
    }

    /// Прогон текущей страницы (её команды и журнал).
    pub(super) fn current_run(&self) -> RunId {
        self.hand.run_ref().run.clone()
    }

    /// Адрес текущей вкладки для зонда слоя: путь страницы и `?run=<прогон>`. Прогон в адресе нужен,
    /// когда открыты две вкладки одной страницы (замена): по одному пути нашлась бы первая попавшаяся.
    pub(super) fn current_prefix(&self) -> String {
        let page = self.hand.run_ref();
        format!("{}?run={}", page.page_url_prefix(), page.run)
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
                    "the placeholder is still there {PLACEHOLDER_GONE_TIMEOUT:?} after the session ended: {last:?}"
                ));
            }
            std::thread::sleep(PLACEHOLDER_POLL);
        }
    }

    /// Заглушка на вкладке `prefix` и где на экране её кнопка (калибровка по зонду прогона `run`).
    pub(super) fn aim_at_return(&mut self, run: &RunId, prefix: &str) -> Result<ReturnAim, String> {
        let placeholder = self.wait_placeholder_shown(prefix).inspect_err(|_| {
            // Снимок экрана при провале: видно, есть ли заглушка вообще и что её закрывает.
            self.hand.screenshot("no-return-button");
        })?;
        let place = self.hand.locate(run).map_err(|error| error.to_string())?;
        let button = placeholder
            .return_button
            .ok_or("the placeholder has no Return button")?;
        Ok(ReturnAim {
            placeholder,
            video: place.video_rect(),
            button_point: place.screen_point_in(button, 0.5, 0.5),
        })
    }

    /// Отметка «прямо перед действием» на странице прогона `run`: её секунда.
    pub(super) fn mark_return(&self, run: &RunId) -> Result<f64, String> {
        let page = self.hand.run_ref();
        page.tell_page_of(run, "mark-return");
        page.wait_ack_of(run, "mark-return", ACK_TIMEOUT)?;
        let line = page.wait_page_log_line_of(run, RETURN_MARK_LINE, ACK_TIMEOUT)?;
        parse_return_mark(&line)
    }

    /// Страница прогона `run` наблюдает поворот и пишет факты возврата.
    pub(super) fn observe_return(&self, run: &RunId) -> Result<ReturnFacts, String> {
        let page = self.hand.run_ref();
        page.tell_page_of(run, "observe-return");
        page.wait_ack_of(run, "observe-return", ACK_TIMEOUT)?;
        // Строки журнала страница шлёт отдельными запросами: факты могут прийти позже подтверждения.
        let line = page
            .wait_page_log_line_of(run, RETURN_FACTS_LINE, LATE_PAGE_LINE_TIMEOUT)
            .map_err(|problem| format!("no return-facts line after observe-return: {problem}"))?;
        parse_return_facts(&line)
    }

    /// Команда текущей странице и ожидание её подтверждения.
    pub(super) fn tell_and_wait(&mut self, command: &str) -> Result<(), String> {
        let run = self.hand.run();
        run.tell_page(command);
        run.wait_ack(command, ACK_TIMEOUT)
    }

    /// Окно плеера закрылось за `timeout`; возвращает, сколько это заняло.
    pub(super) fn wait_window_gone(&self, timeout: Duration) -> Result<Duration, String> {
        let started = Instant::now();
        self.control
            .wait_until_gone(timeout)
            .map(|()| started.elapsed())
            .map_err(|error| format!("the player window did not close within {timeout:?}: {error}"))
    }

    /// Записать провал шага (`Err`) и вернуть, прошёл ли шаг.
    pub(super) fn record<T>(&mut self, result: Result<T, String>) -> Option<T> {
        match result {
            Ok(value) => Some(value),
            Err(problem) => {
                self.failures.push(problem);
                None
            }
        }
    }

    /// Итог строки: отчёты страниц (текущей и `other_pages`), журнал плеера, посредник; браузер закрыт.
    pub(super) fn close(
        self,
        other_pages: &[(&RunId, &str)],
    ) -> Result<(RowEnd, Vec<String>, Vec<String>), ScenarioError> {
        let Session {
            hand,
            failures,
            notes,
            ..
        } = self;
        let mut run = hand.into_run();
        let current_page = page_id_of(&run.page_url_prefix()).to_owned();
        let mut reports: Vec<(String, Result<PageReport, SiteError>)> =
            vec![(current_page, run.finish_page())];
        for (other_run, page_id) in other_pages {
            reports.push(((*page_id).to_owned(), run.finish_page_of(other_run)));
        }
        let page_log = format_browser_log(&run.take_page_log());
        let player_lines = run.player_log.snapshot_forbidden(PLAYER_END_NEEDLES)?;
        let forbidden = run.player_log.snapshot_forbidden(FORBIDDEN_PLAYER_LINES)?;
        let exit = run.browser.shutdown()?;
        let host = read_host_evidence(&exit.stderr_log)?;
        let mut evidence = Vec::new();
        for (page_id, report) in &reports {
            evidence.extend(page_failures(report, page_id));
        }
        evidence.extend(host_failures(&host));
        evidence.extend(
            unexpected_player_lines(&forbidden)
                .map(|line| format!("forbidden player line: {line}")),
        );
        Ok((
            RowEnd {
                player: PlayerEndFacts::from_lines(&player_lines),
                host,
                page_log,
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
    pub(super) player: PlayerEndFacts,
    pub(super) host: HostLogEvidence,
    pub(super) page_log: String,
    /// Провалы общих улик: отчёты страниц, посредник, запрещённые строки плеера.
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

    #[test]
    fn the_page_id_comes_from_the_page_address() {
        assert_eq!(
            page_id_of("http://127.0.0.1:7/site/pages/return-kit.html"),
            "return-kit"
        );
        assert_eq!(page_id_of("weird"), "weird");
    }

    #[test]
    fn the_second_tab_query_keeps_the_hand_off_mode_and_adds_the_start() {
        let query = query_starting_at(20);
        assert!(query.starts_with(HANDOFF_MODE_QUERY));
        assert!(query.ends_with("&start_s=20"));
    }

    #[test]
    fn a_failing_row_shows_its_problems_and_the_page_log() {
        let end = RowEnd {
            player: PlayerEndFacts::default(),
            host: HostLogEvidence::default(),
            page_log: "page log (1 lines)".to_owned(),
            evidence_failures: vec!["page report is Fail".to_owned()],
        };
        let verdict = verdict(
            &end,
            vec!["no placeholder".to_owned()],
            &["note".to_owned()],
        );
        assert_eq!(verdict.outcome, crate::scenarios::outcome::Outcome::Fail);
        assert!(verdict.detail.contains("no placeholder"));
        assert!(verdict.detail.contains("page report is Fail"));
        assert!(verdict.detail.contains("page log"));
        let clean = RowEnd {
            evidence_failures: Vec::new(),
            ..end
        };
        assert_eq!(
            super::verdict(&clean, Vec::new(), &["fine".to_owned()]),
            Verdict::pass("fine")
        );
    }
}
