//! Чистые приговоры сценария `auto-handoff`: по собранным фактам решают, играет ли видео у браузера,
//! ушло ли оно в плеер, стоят ли старые вкладки на паузе. Ввода-вывода здесь нет, поэтому каждое
//! нарушение проверяется юнит-тестом.
//!
//! Главное правило: «нет передачи» никогда не судится одним отсутствием события. Каждая такая
//! строка ОБЯЗАНА доказать и положительный факт — страница правда играла силами браузера (настоящие
//! кадры растут, часы идут) и настоящий клик правда дошёл до неё. Иначе мёртвый стенд прошёл бы все
//! строки «ничего не произошло».

use super::super::capture::max_real_frames;
use super::super::entries::return_hand::PlaceholderView;
use super::super::evidence::host_failures;
use super::facts::{PageDuration, StateFacts, WindowFacts, has_user_click};
use crate::observe::HostLogEvidence;
use crate::site::PageReport;

/// Самое короткое окно, на котором кадры браузера что-то доказывают, с.
const MIN_WINDOW_S: f64 = 5.0;
/// Самое короткое окно «под тенью», на котором счётчик браузера что-то доказывает, с (как у `capture`).
const MIN_SHADOW_WINDOW_S: f64 = 6.0;
/// Сколько настоящих кадров в секунду браузер обязан декодировать, когда видео у него. Ролики стенда
/// идут 25 к/с; 5 к/с — запас на медленный запуск и тяжёлую машину, и всё равно в разы выше потолка
/// «под тенью» (`capture`: 5 к/с + запас на переход, а не «не меньше»).
const MIN_BROWSER_FPS: f64 = 5.0;
/// Какая доля окна секунда страницы обязана пройти, когда видео играет у браузера.
const MIN_BROWSER_CLOCK_SHARE: f64 = 0.5;
/// На сколько секунда страницы обязана уйти вперёд под тенью: «часы идут», а не «одно значение».
const MIN_SHADOW_CLOCK_ADVANCE_S: f64 = 1.0;
/// Самое малое число секунд, которое вкладка обязана сыграть до паузы: иначе «стоит на своей секунде»
/// неотличимо от «и не начинала».
const MIN_PLAYED_BEFORE_PAUSE_S: f64 = 1.0;
/// На сколько может отличаться секунда паузы в двух снимках подряд, с (кадр-другой дрожи).
const PAUSED_DRIFT_S: f64 = 0.3;

/// Видео играет у браузера: настоящие кадры растут, часы идут, не на паузе и не кончилось.
pub(super) fn browser_plays_failures(facts: &WindowFacts) -> Vec<String> {
    if !facts.probe_available {
        return vec!["the page could not read the real frame counter".to_owned()];
    }
    let mut failures = Vec::new();
    if facts.window_s < MIN_WINDOW_S {
        failures.push(format!(
            "the window was {:.1} s, too short to judge the browser decoder (need {MIN_WINDOW_S} s)",
            facts.window_s
        ));
        return failures;
    }
    let needed_frames = MIN_BROWSER_FPS * facts.window_s;
    if (facts.real_frames as f64) < needed_frames {
        failures.push(format!(
            "the browser decoded only {} real frames in {:.1} s (need {needed_frames:.0}): the page does not play in the browser",
            facts.real_frames, facts.window_s
        ));
    }
    let needed_clock = MIN_BROWSER_CLOCK_SHARE * facts.window_s;
    // Зацикленное превью переходит через конец ролика: часы по модулю длины — не улика, кадры — улика.
    if !facts.looping && facts.clock_advance_s < needed_clock {
        failures.push(format!(
            "the page clock advanced only {:.2} s in {:.1} s (need {needed_clock:.1} s)",
            facts.clock_advance_s, facts.window_s
        ));
    }
    if facts.paused {
        failures.push("the video is paused for the page".to_owned());
    }
    if facts.ended {
        failures.push("the video ended before the window did".to_owned());
    }
    failures
}

/// То же по метрикам итогового отчёта страницы режима 1 (`runHandoffPhase`: `real_probe_available`,
/// `shadow_window_s`, `real_frames_in_window`): баннер без звука идёт у браузера, пока окно меряется.
pub(super) fn report_plays_in_browser_failures(report: &PageReport) -> Vec<String> {
    let metric = |name: &str| report.metrics.get(name).copied();
    let (Some(available), Some(window_s), Some(real_frames)) = (
        metric("real_probe_available"),
        metric("shadow_window_s"),
        metric("real_frames_in_window"),
    ) else {
        return vec!["the page report has no real-frame metrics".to_owned()];
    };
    browser_plays_failures(&WindowFacts {
        probe_available: available >= 1.0,
        window_s,
        // Метрика целая, но хранится дробной; кадры не отрицательные.
        real_frames: real_frames.max(0.0).round() as u64,
        // Часы баннера (зациклен) метрикой не меряются: их ход судят итоговые проверки страницы.
        clock_advance_s: window_s,
        start_s: 0.0,
        end_s: window_s,
        paused: false,
        muted: true,
        ended: false,
        looping: true,
        duration: PageDuration::Unknown,
    })
}

/// Видео ушло в плеер: браузер почти не декодирует, а часы страницы идут под тенью.
pub(super) fn handed_off_failures(facts: &WindowFacts) -> Vec<String> {
    if !facts.probe_available {
        return vec!["the page could not read the real frame counter".to_owned()];
    }
    let mut failures = Vec::new();
    if facts.window_s < MIN_SHADOW_WINDOW_S {
        failures.push(format!(
            "the shadow window was {:.1} s, too short to judge the browser decoder (need {MIN_SHADOW_WINDOW_S} s)",
            facts.window_s
        ));
        return failures;
    }
    let allowed = max_real_frames(facts.window_s);
    if (facts.real_frames as f64) > allowed {
        failures.push(format!(
            "the browser decoded {} real frames in {:.1} s under the shadow (allowed {allowed:.0}): it still plays by itself",
            facts.real_frames, facts.window_s
        ));
    }
    if facts.clock_advance_s < MIN_SHADOW_CLOCK_ADVANCE_S {
        failures.push(format!(
            "the page clock advanced only {:.2} s under the shadow (need {MIN_SHADOW_CLOCK_ADVANCE_S} s)",
            facts.clock_advance_s
        ));
    }
    if facts.paused {
        failures.push("the page sees the video paused under the shadow".to_owned());
    }
    failures
}

/// Эфир или поток без длины: страница видит `duration = Infinity` и под тенью.
pub(super) fn infinite_duration_failures(facts: &WindowFacts) -> Vec<String> {
    match facts.duration {
        PageDuration::Infinite => Vec::new(),
        other => vec![format!(
            "the page sees duration {other:?} under the shadow, expected Infinity"
        )],
    }
}

/// Вкладка стоит на паузе на своей секунде: два снимка подряд — пауза и та же секунда, и секунда
/// не нулевая (вкладка успела поиграть до замены).
pub(super) fn still_paused_failures(
    label: &str,
    first: &StateFacts,
    second: &StateFacts,
) -> Vec<String> {
    let mut failures = Vec::new();
    if !first.paused || !second.paused {
        failures.push(format!(
            "{label}: expected paused at both checks, saw paused={} then paused={}",
            first.paused, second.paused
        ));
    }
    let drift = (second.time_s - first.time_s).abs();
    if drift > PAUSED_DRIFT_S {
        failures.push(format!(
            "{label}: the second moved {drift:.2} s between two checks while it should stand still ({:.2} -> {:.2})",
            first.time_s, second.time_s
        ));
    }
    if first.time_s < MIN_PLAYED_BEFORE_PAUSE_S {
        failures.push(format!(
            "{label}: stands at {:.2} s, it never played before the replacement",
            first.time_s
        ));
    }
    failures
}

/// Сколько пропусков на сеанс выдал посредник за весь прогон. `expected_grants == 0` — передачи не
/// было: плеер не просили (никаких отметок посредника про сеанс). Иначе — все отметки и точное число.
pub(super) fn session_count_failures(host: &HostLogEvidence, expected_grants: u32) -> Vec<String> {
    let mut failures = if expected_grants == 0 {
        Vec::new()
    } else {
        host_failures(host)
    };
    if host.session_grants != expected_grants {
        failures.push(format!(
            "the player granted {} session(s), expected {expected_grants}",
            host.session_grants
        ));
    }
    failures
}

/// Заглушка «Playing in Fastiplayer»: стоит над видео (`shown`) или её нет.
pub(super) fn placeholder_failures(
    label: &str,
    view: &PlaceholderView,
    shown: bool,
) -> Vec<String> {
    let is_shown = view.handed_off && view.visible && view.return_button.is_some();
    // «Нет заглушки» — это и передача кончилась, и панель спрятана: половина состояния не считается.
    let is_absent = !view.handed_off && !view.visible;
    match (shown, is_shown, is_absent) {
        (true, true, _) | (false, _, true) => Vec::new(),
        (true, false, _) => vec![format!(
            "{label}: the placeholder with a Return button must be shown, the probe says {view:?}"
        )],
        (false, _, false) => vec![format!(
            "{label}: the page must keep its video (no placeholder), the probe says {view:?}"
        )],
    }
}

/// Настоящий клик дошёл до страницы: без этого «нет передачи» ничего не доказывает.
pub(super) fn click_reached_page_failures(label: &str, play_click_lines: &[String]) -> Vec<String> {
    if has_user_click(play_click_lines) {
        Vec::new()
    } else {
        vec![format!(
            "{label}: the page never saw a real click on Play with a user activation (lines: {play_click_lines:?})"
        )]
    }
}

/// Наведение дошло до страницы: превью запустилось по настоящему движению мыши.
pub(super) fn hover_reached_page_failures(hover_lines: &[String]) -> Vec<String> {
    if hover_lines.is_empty() {
        vec!["the page never saw the real mouse over the preview".to_owned()]
    } else {
        Vec::new()
    }
}

// --- реклама перед фильмом по клику (строка `preroll`) ------------------------------------------

/// Самое короткое окно фильма, на котором часы что-то доказывают, с.
const MIN_FILM_WINDOW_S: f64 = 8.0;
/// Потолок настоящих кадров браузера у элемента фильма за всё окно (запас на момент перехода).
const MAX_FILM_REAL_FRAMES: f64 = 15.0;
/// На сколько секунд часы фильма обязаны уйти вперёд за окно.
const MIN_FILM_CLOCK_ADVANCE_S: f64 = 1.0;

/// Метрика отчёта страницы; нет метрики — это провал, а не нуль.
fn metric(report: &PageReport, name: &str, failures: &mut Vec<String>) -> Option<f64> {
    let value = report.metrics.get(name).copied();
    if value.is_none() {
        failures.push(format!("the page report has no metric '{name}'"));
    }
    value
}

/// Метрики страницы «реклама → фильм по клику»: реклама ушла под тень, фильм идёт под тенью.
pub(super) fn preroll_metric_failures(report: &PageReport) -> Vec<String> {
    let mut failures = Vec::new();
    if metric(report, "film_window_s", &mut failures)
        .is_some_and(|window| window < MIN_FILM_WINDOW_S)
    {
        failures.push(format!(
            "the film window was shorter than {MIN_FILM_WINDOW_S} s, too short to judge its clock"
        ));
    }
    if metric(report, "film_clock_advance_s", &mut failures)
        .is_some_and(|advance| advance < MIN_FILM_CLOCK_ADVANCE_S)
    {
        failures.push(format!(
            "the film clock advanced less than {MIN_FILM_CLOCK_ADVANCE_S} s: the film does not play under the shadow"
        ));
    }
    if let Some(frames) = metric(report, "film_real_frames", &mut failures)
        && frames > MAX_FILM_REAL_FRAMES
    {
        failures.push(format!(
            "the browser decoded {frames} real frames of the film (allowed {MAX_FILM_REAL_FRAMES}): the film plays in the browser"
        ));
    }
    if metric(report, "film_paused", &mut failures).is_some_and(|paused| paused > 0.0) {
        failures.push("the page sees the film paused".to_owned());
    }
    if metric(report, "film_play_rejected", &mut failures).is_some_and(|rejected| rejected > 0.0) {
        failures.push("the page's film play() promise was rejected".to_owned());
    }
    if metric(report, "ad_ended_total", &mut failures)
        .is_some_and(|ended| (ended - 1.0).abs() > f64::EPSILON)
    {
        failures.push("the ad 'ended' handler did not fire exactly once".to_owned());
    }
    if let (Some(window_s), Some(frames)) = (
        metric(report, "ad_shadow_window_s", &mut failures),
        metric(report, "ad_real_frames", &mut failures),
    ) && frames > max_real_frames(window_s)
    {
        failures.push(format!(
            "the browser decoded {frames} real frames of the ad in {window_s:.1} s under the shadow: the ad was not handed off"
        ));
    }
    failures
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::scenarios::entries::return_hand::ClientBox;
    use crate::site::{PageStatus, RunId};

    fn playing_in_browser() -> WindowFacts {
        WindowFacts {
            probe_available: true,
            window_s: 12.0,
            real_frames: 290,
            clock_advance_s: 11.8,
            start_s: 0.5,
            end_s: 12.3,
            paused: false,
            muted: false,
            ended: false,
            looping: false,
            duration: PageDuration::Finite(60.0),
        }
    }

    fn under_the_shadow() -> WindowFacts {
        WindowFacts {
            real_frames: 3,
            window_s: 8.0,
            clock_advance_s: 7.9,
            ..playing_in_browser()
        }
    }

    #[test]
    fn a_page_that_plays_by_itself_passes_the_browser_check_and_fails_the_shadow_check() {
        assert!(browser_plays_failures(&playing_in_browser()).is_empty());
        let shadow = handed_off_failures(&WindowFacts {
            window_s: 8.0,
            ..playing_in_browser()
        });
        assert_eq!(shadow.len(), 1, "{shadow:?}");
        assert!(shadow[0].contains("still plays by itself"));
    }

    #[test]
    fn a_page_under_the_shadow_passes_the_shadow_check_and_fails_the_browser_check() {
        assert!(handed_off_failures(&under_the_shadow()).is_empty());
        let in_browser = browser_plays_failures(&under_the_shadow());
        assert!(
            in_browser
                .iter()
                .any(|f| f.contains("does not play in the browser")),
            "{in_browser:?}"
        );
    }

    #[test]
    fn a_dead_bench_cannot_pass_a_no_handoff_row() {
        // Ни кадров, ни хода часов: страница мертва или стоит на паузе — «нет передачи» ничего не значит.
        let dead = WindowFacts {
            real_frames: 0,
            clock_advance_s: 0.0,
            paused: true,
            ..playing_in_browser()
        };
        let failures = browser_plays_failures(&dead);
        assert_eq!(failures.len(), 3, "{failures:?}");
        let blind = WindowFacts {
            probe_available: false,
            ..playing_in_browser()
        };
        assert_eq!(browser_plays_failures(&blind).len(), 1);
        assert_eq!(handed_off_failures(&blind).len(), 1);
    }

    #[test]
    fn the_report_metrics_judge_a_looping_banner_by_its_real_frames() {
        let mut report = preroll_report(0.0, 0.0);
        report.metrics.extend([
            ("real_probe_available".to_owned(), 1.0),
            ("shadow_window_s".to_owned(), 12.0),
            ("real_frames_in_window".to_owned(), 290.0),
        ]);
        assert!(report_plays_in_browser_failures(&report).is_empty());
        report
            .metrics
            .insert("real_frames_in_window".to_owned(), 4.0);
        assert!(
            report_plays_in_browser_failures(&report)[0].contains("does not play in the browser")
        );
        report.metrics.remove("shadow_window_s");
        assert!(report_plays_in_browser_failures(&report)[0].contains("no real-frame metrics"));
    }

    #[test]
    fn a_looping_preview_is_judged_by_its_frames_not_by_its_wrapped_clock() {
        // Ролик 12 с зациклен: за окно 13,7 с часы страницы почти вернулись к нулю.
        let wrapped = WindowFacts {
            clock_advance_s: 0.05,
            looping: true,
            window_s: 13.7,
            ..playing_in_browser()
        };
        assert!(browser_plays_failures(&wrapped).is_empty());
        // Без цикла те же часы — провал; и зацикленное, но без кадров, тоже провал.
        let not_looping = WindowFacts {
            looping: false,
            ..wrapped
        };
        assert_eq!(browser_plays_failures(&not_looping).len(), 1);
        let frozen = WindowFacts {
            real_frames: 0,
            ..wrapped
        };
        assert_eq!(browser_plays_failures(&frozen).len(), 1);
    }

    #[test]
    fn too_short_windows_prove_nothing_in_either_direction() {
        let short = WindowFacts {
            window_s: 2.0,
            ..playing_in_browser()
        };
        assert!(browser_plays_failures(&short)[0].contains("too short"));
        assert!(handed_off_failures(&short)[0].contains("too short"));
    }

    #[test]
    fn a_shadow_whose_clock_stands_still_fails() {
        let frozen = WindowFacts {
            clock_advance_s: 0.2,
            paused: true,
            ..under_the_shadow()
        };
        assert_eq!(handed_off_failures(&frozen).len(), 2);
    }

    #[test]
    fn the_infinite_duration_check_names_what_the_page_saw() {
        let live = WindowFacts {
            duration: PageDuration::Infinite,
            ..under_the_shadow()
        };
        assert!(infinite_duration_failures(&live).is_empty());
        assert!(infinite_duration_failures(&under_the_shadow())[0].contains("Finite(60.0)"));
    }

    #[test]
    fn a_tab_must_stand_still_on_its_own_nonzero_second() {
        let at = |time_s, paused| StateFacts {
            time_s,
            paused,
            ended: false,
        };
        assert!(still_paused_failures("tab 1", &at(7.2, true), &at(7.25, true)).is_empty());
        assert!(
            still_paused_failures("tab 1", &at(7.2, true), &at(9.2, true))[0].contains("moved")
        );
        assert!(
            still_paused_failures("tab 1", &at(7.2, false), &at(9.2, false))[0].contains("paused")
        );
        assert!(
            still_paused_failures("tab 1", &at(0.2, true), &at(0.2, true))[0]
                .contains("never played")
        );
    }

    #[test]
    fn the_session_count_distinguishes_no_session_from_the_wrong_number_of_sessions() {
        let none = HostLogEvidence::default();
        assert!(session_count_failures(&none, 0).is_empty());
        let granted = HostLogEvidence {
            caller_accepted: true,
            hello_accepted: true,
            session_granted: true,
            session_grants: 1,
        };
        assert!(session_count_failures(&granted, 1).is_empty());
        // Передача, которой не должно быть.
        assert!(
            session_count_failures(&granted, 0)[0].contains("granted 1 session(s), expected 0")
        );
        // Передачи, которой не хватает: и отметки, и счёт.
        assert_eq!(session_count_failures(&none, 1).len(), 4);
        // Лишняя вторая передача.
        let twice = HostLogEvidence {
            session_grants: 2,
            ..granted
        };
        assert!(session_count_failures(&twice, 1)[0].contains("granted 2 session(s), expected 1"));
    }

    #[test]
    fn the_placeholder_is_shown_with_its_button_or_absent() {
        let button = ClientBox {
            left: 1.0,
            top: 1.0,
            width: 10.0,
            height: 10.0,
        };
        let shown = PlaceholderView {
            handed_off: true,
            visible: true,
            panel: Some(button),
            return_button: Some(button),
        };
        let absent = PlaceholderView::default();
        assert!(placeholder_failures("t", &shown, true).is_empty());
        assert!(placeholder_failures("t", &absent, false).is_empty());
        assert!(placeholder_failures("t", &absent, true)[0].contains("must be shown"));
        assert!(placeholder_failures("t", &shown, false)[0].contains("no placeholder"));
        // Наполовину снятая заглушка (передача кончилась, панель ещё видна) — тоже не «нет заглушки».
        let leftover = PlaceholderView {
            handed_off: false,
            visible: true,
            ..absent
        };
        assert!(!placeholder_failures("t", &leftover, false).is_empty());
    }

    #[test]
    fn the_click_and_the_hover_must_be_seen_by_the_page() {
        let seen = vec!["auto-film: play-clicked active=true".to_owned()];
        assert!(click_reached_page_failures("row", &seen).is_empty());
        assert!(!click_reached_page_failures("row", &[]).is_empty());
        let no_activation = vec!["auto-film: play-clicked active=false".to_owned()];
        assert!(!click_reached_page_failures("row", &no_activation).is_empty());
        assert!(hover_reached_page_failures(&["preview-hover-started".to_owned()]).is_empty());
        assert!(!hover_reached_page_failures(&[]).is_empty());
    }

    fn preroll_report(film_real_frames: f64, ad_real_frames: f64) -> PageReport {
        let metrics = [
            ("film_window_s", 11.0),
            ("film_clock_advance_s", 10.5),
            ("film_real_frames", film_real_frames),
            ("film_paused", 0.0),
            ("film_play_rejected", 0.0),
            ("ad_ended_total", 1.0),
            ("ad_shadow_window_s", 6.0),
            ("ad_real_frames", ad_real_frames),
        ];
        PageReport {
            run: RunId::new("r-1").expect("run id"),
            scenario: "auto-preroll".into(),
            status: PageStatus::Pass,
            checks: Vec::new(),
            user_agent: String::new(),
            elapsed_ms: 0,
            metrics: metrics
                .into_iter()
                .map(|(name, value)| (name.to_owned(), value))
                .collect(),
        }
    }

    #[test]
    fn the_film_after_the_ad_must_play_under_the_shadow_not_in_the_browser() {
        assert!(preroll_metric_failures(&preroll_report(2.0, 1.0)).is_empty());
        let film_in_browser = preroll_metric_failures(&preroll_report(250.0, 1.0));
        assert!(film_in_browser[0].contains("film plays in the browser"));
        let ad_in_browser = preroll_metric_failures(&preroll_report(2.0, 400.0));
        assert!(ad_in_browser[0].contains("the ad was not handed off"));
    }

    #[test]
    fn a_missing_preroll_metric_is_a_failure_not_a_zero() {
        let mut report = preroll_report(2.0, 1.0);
        report.metrics.remove("film_clock_advance_s");
        let failures = preroll_metric_failures(&report);
        assert!(failures.iter().any(|f| f.contains("film_clock_advance_s")));
    }
}
