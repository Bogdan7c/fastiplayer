//! Строки предложения «Ask» режима 2 (сессия 12b): человек нажал play на фильме со звуком на сайте с
//! правилом «ask» — видео замирает, поверх него панель из четырёх кнопок, и человек выбирает настоящей
//! мышью. Все пять строк идут на странице фильма и делят общие звенья:
//!  1. `ask-offer-shown` — предложение появилось и висит без срока, видео стоит, плеера нет;
//!  2. `ask-play-here` — «Play here»: видео играет у браузера, настройка не тронута, повторный play не
//!     вызывает нового предложения;
//!  3. `ask-play-in-fastiplayer` — «Play in Fastiplayer»: передача, кадры в рендере, настройка не тронута;
//!  4. `ask-always` — «Always for this site»: передача и правило сайта «always» в хранилище;
//!  5. `ask-never` — «Never for this site»: видео у браузера, правило «never», повторный play без предложения.
//!
//! Каждая строка «нет передачи» требует положительного факта (настоящие кадры браузера растут, клик с
//! активацией дошёл до страницы): мёртвый стенд её не пройдёт. Приговоры — чистые функции `offer_judge`.

use std::time::Duration;

use serde_json::Value;

use super::judge::{
    browser_plays_failures, click_reached_page_failures, placeholder_failures,
    session_count_failures,
};
use super::offer_judge::{
    held_by_offer_failures, offer_absent_failures, offer_shown_failures, stored_settings_failures,
};
use super::rows_click::handed_off_after_gesture_steps;
use super::rows_flow::pause_with_real_click;
use super::settings::{SettingsPreset, SiteRule, site_key_of_origin};
use super::steps::{AutoSession, Begun, begin, verdict};
use crate::browsers::BrowserKind;
use crate::environment::BenchEnvironment;
use crate::scenarios::capture::browser_does_not_decode;
use crate::scenarios::context::{ScenarioError, Stand};
use crate::scenarios::entries::return_hand::OfferChoice;
use crate::scenarios::outcome::Verdict;
use crate::site::RunId;

/// Страница фильма: Play, Pause, звук, 60 с.
const FILM_PAGE: &str = "auto-film";
/// Сколько предложение обязано провисеть без выбора: у него нет срока (решение 256), 5 с с запасом
/// выше любых «коротких» таймеров экстеншена и достаточно для окна «видео стоит» (не короче 5 с).
const OFFER_HANG: Duration = Duration::from_secs(5);
/// Окно, в котором видео играет у браузера после «Play here» / «Never» (судья требует не меньше 5 с).
const BROWSER_PLAY_WINDOW: Duration = Duration::from_secs(8);
/// Сколько после повторного play следим, не появится ли предложение снова.
const NO_NEW_OFFER_WATCH: Duration = Duration::from_secs(4);
/// Шаг опроса зонда в этом наблюдении.
const OFFER_POLL: Duration = Duration::from_millis(500);

/// Пять строк предложения.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) enum OfferRow {
    Shown,
    PlayHere,
    PlayInFastiplayer,
    Always,
    Never,
}

impl OfferRow {
    pub(super) fn name(self) -> &'static str {
        match self {
            Self::Shown => "ask-offer-shown",
            Self::PlayHere => "ask-play-here",
            Self::PlayInFastiplayer => "ask-play-in-fastiplayer",
            Self::Always => "ask-always",
            Self::Never => "ask-never",
        }
    }

    /// Настройка строки. `ask-play-in-fastiplayer` идёт через «новый сайт + правило для новых = ask при
    /// включённом режиме», остальные — через собственное правило сайта при ВЫКЛЮЧЕННОМ режиме
    /// (правило «ask» обязано работать и так).
    pub(super) fn preset(self) -> SettingsPreset {
        match self {
            Self::PlayInFastiplayer => SettingsPreset::UnknownSiteAsk,
            Self::Shown | Self::PlayHere | Self::Always | Self::Never => {
                SettingsPreset::SiteRuleAsk
            }
        }
    }

    /// Что человек нажимает; у строки без выбора (`Shown`) — ничего.
    fn choice(self) -> Option<OfferChoice> {
        match self {
            Self::Shown => None,
            Self::PlayHere => Some(OfferChoice::Here),
            Self::PlayInFastiplayer => Some(OfferChoice::Fastiplayer),
            Self::Always => Some(OfferChoice::Always),
            Self::Never => Some(OfferChoice::Never),
        }
    }

    /// Правило, которое выбор обязан записать для сайта; `None` — настройка остаётся как была.
    fn rule_written(self) -> Option<SiteRule> {
        match self.choice() {
            Some(OfferChoice::Always) => Some(SiteRule::Always),
            Some(OfferChoice::Never) => Some(SiteRule::Never),
            Some(OfferChoice::Here | OfferChoice::Fastiplayer) | None => None,
        }
    }

    /// Видео после выбора идёт в плеер (иначе играет у браузера).
    fn hands_off(self) -> bool {
        matches!(self, Self::PlayInFastiplayer | Self::Always)
    }

    /// Настройка, которая обязана лежать в хранилище после строки.
    fn expected_settings(self, site_key: &str) -> Option<Value> {
        let start = self.preset().setup(site_key)?;
        Some(
            match self.rule_written() {
                Some(rule) => start.with_site_rule(site_key, rule),
                None => start,
            }
            .to_stored_value(),
        )
    }
}

/// Одна строка предложения.
pub(super) fn run_offer_row(
    stand: &Stand,
    environment: &BenchEnvironment,
    browser: BrowserKind,
    row: OfferRow,
) -> Result<Verdict, ScenarioError> {
    // Ключ сайта нужен для ожидаемой настройки; считаем его до открытия браузера, чтобы провал не оставил его висеть.
    let site_key = match site_key_of_origin(&stand.site.main_origin()) {
        Ok(key) => key,
        Err(problem) => return Ok(Verdict::fail(problem)),
    };
    let mut session = match begin(stand, environment, browser, FILM_PAGE, row.preset())? {
        Begun::Ready(session) => *session,
        Begun::Finished(verdict) => return Ok(verdict),
    };
    match row {
        OfferRow::Shown => offer_hangs_steps(&mut session),
        _ => choice_steps(&mut session, row, &site_key),
    }
    let (end, mut failures, notes) = session.close(&[])?;
    let expected_grants = u32::from(row.hands_off());
    failures.extend(session_count_failures(&end.host, expected_grants));
    if row.hands_off()
        && let Ok(report) = &end.current_report
    {
        failures.extend(browser_does_not_decode(report));
    }
    Ok(verdict(&end, failures, &notes))
}

/// Настоящий клик по Play и ожидание предложения целиком. `None` — шаг провален (провал записан).
fn click_play_and_wait_for_offer(
    session: &mut AutoSession<'_, '_>,
    run: &RunId,
    prefix: &str,
) -> Option<()> {
    let clicked = session.click_target(run, "play");
    session.record(clicked)?;
    let shown = session.wait_offer_shown(prefix);
    let view = session.record(shown)?;
    session
        .failures
        .extend(offer_shown_failures("after the Play click", &view));
    Some(())
}

/// Строка 1: предложение появилось, висит без срока, видео стоит, плеера нет.
fn offer_hangs_steps(session: &mut AutoSession<'_, '_>) {
    let run = session.current_run();
    let prefix = session.current_prefix();
    let marked = session.mark_window(&run);
    if session.record(marked).is_none() {
        return;
    }
    if click_play_and_wait_for_offer(session, &run, &prefix).is_none() {
        return;
    }
    // Срока у предложения нет: через OFFER_HANG оно обязано висеть так же целиком.
    std::thread::sleep(OFFER_HANG);
    let still = session.viewer_offer(&prefix);
    if let Some(view) = session.record(still) {
        session.failures.extend(offer_shown_failures(
            "after waiting without a choice",
            &view,
        ));
    }
    let reported = session.report_window(&run);
    if let Some(facts) = session.record(reported) {
        session.failures.extend(held_by_offer_failures(&facts));
        session.notes.push(format!(
            "the offer held the video: paused={}, {} real frames in {:.1} s, clock +{:.2} s",
            facts.paused, facts.real_frames, facts.window_s, facts.clock_advance_s
        ));
    }
    let lines = session.play_click_lines(&run);
    session
        .failures
        .extend(click_reached_page_failures("the Play click", &lines));
    let placeholder = session.placeholder(&prefix);
    if let Some(view) = session.record(placeholder) {
        session
            .failures
            .extend(placeholder_failures("while the offer hangs", &view, false));
    }
}

/// Строки 2-5: предложение, настоящий клик по кнопке, исход и настройка в хранилище.
fn choice_steps(session: &mut AutoSession<'_, '_>, row: OfferRow, site_key: &str) {
    let Some(choice) = row.choice() else {
        return;
    };
    let run = session.current_run();
    let prefix = session.current_prefix();
    if click_play_and_wait_for_offer(session, &run, &prefix).is_none() {
        return;
    }
    let chosen = session.click_offer_button(&run, &prefix, choice);
    if session.record(chosen).is_none() {
        return;
    }
    let gone = session.wait_offer_gone(&prefix);
    session.record(gone);
    if row.hands_off() {
        handed_off_after_gesture_steps(session, &run, &prefix, false);
    } else {
        plays_in_browser_steps(session, &run, &prefix);
        no_new_offer_steps(session, &run, &prefix);
    }
    check_stored_settings(session, row, site_key);
}

/// После «Play here» / «Never»: видео играет силами браузера, плеера и заглушки нет, предложения нет.
fn plays_in_browser_steps(session: &mut AutoSession<'_, '_>, run: &RunId, prefix: &str) {
    let marked = session.mark_window(run);
    if session.record(marked).is_none() {
        return;
    }
    std::thread::sleep(BROWSER_PLAY_WINDOW);
    let reported = session.report_window(run);
    if let Some(facts) = session.record(reported) {
        session.failures.extend(browser_plays_failures(&facts));
        session.notes.push(format!(
            "after the choice the page played in the browser: {} real frames in {:.1} s, clock +{:.1} s",
            facts.real_frames, facts.window_s, facts.clock_advance_s
        ));
    }
    let lines = session.play_click_lines(run);
    session
        .failures
        .extend(click_reached_page_failures("the Play click", &lines));
    let placeholder = session.placeholder(prefix);
    if let Some(view) = session.record(placeholder) {
        session
            .failures
            .extend(placeholder_failures("after the choice", &view, false));
    }
    let offer = session.viewer_offer(prefix);
    if let Some(view) = session.record(offer) {
        session
            .failures
            .extend(offer_absent_failures("after the choice", &view));
    }
}

/// Пауза настоящим кликом и новый настоящий клик по Play: предложения нет, видео снова играет у браузера.
fn no_new_offer_steps(session: &mut AutoSession<'_, '_>, run: &RunId, prefix: &str) {
    let paused = pause_with_real_click(session, run);
    if session.record(paused).is_none() {
        return;
    }
    let marked = session.mark_window(run);
    if session.record(marked).is_none() {
        return;
    }
    let clicked = session.click_target(run, "play");
    if session.record(clicked).is_none() {
        return;
    }
    // Следим за зондом: предложение не должно появиться ни на миг.
    let watch_until = std::time::Instant::now() + NO_NEW_OFFER_WATCH;
    while std::time::Instant::now() < watch_until {
        let offer = session.viewer_offer(prefix);
        if let Some(view) = session.record(offer) {
            session
                .failures
                .extend(offer_absent_failures("after the second Play click", &view));
        }
        std::thread::sleep(OFFER_POLL);
    }
    std::thread::sleep(BROWSER_PLAY_WINDOW.saturating_sub(NO_NEW_OFFER_WATCH));
    let reported = session.report_window(run);
    if let Some(facts) = session.record(reported) {
        session.failures.extend(browser_plays_failures(&facts));
        session.notes.push(format!(
            "after pause and a new Play the page played in the browser again: {} real frames in {:.1} s",
            facts.real_frames, facts.window_s
        ));
    }
    let lines = session.play_click_lines(run);
    if lines.len() < 2 {
        session.failures.push(format!(
            "the page saw {} click(s) on Play, expected two (the offer one and the repeat)",
            lines.len()
        ));
    }
    let placeholder = session.placeholder(prefix);
    if let Some(view) = session.record(placeholder) {
        session
            .failures
            .extend(placeholder_failures("after the second click", &view, false));
    }
}

/// Хранилище экстеншена после строки: правило сайта изменилось ровно так, как обещал выбор.
fn check_stored_settings(session: &mut AutoSession<'_, '_>, row: OfferRow, site_key: &str) {
    let Some(expected) = row.expected_settings(site_key) else {
        return;
    };
    let settled = session.settings_settle_to(&expected);
    if let Some(stored) = session.record(settled) {
        session
            .failures
            .extend(stored_settings_failures(row.name(), &expected, &stored));
        session.notes.push(format!(
            "stored rule for {site_key}: {}",
            stored["site_rules"][site_key]
        ));
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    const ALL: [OfferRow; 5] = [
        OfferRow::Shown,
        OfferRow::PlayHere,
        OfferRow::PlayInFastiplayer,
        OfferRow::Always,
        OfferRow::Never,
    ];

    #[test]
    fn every_row_has_a_distinct_name_and_an_ask_preset() {
        let names: std::collections::HashSet<_> = ALL.iter().map(|row| row.name()).collect();
        assert_eq!(names.len(), ALL.len());
        for row in ALL {
            assert!(
                matches!(
                    row.preset(),
                    SettingsPreset::SiteRuleAsk | SettingsPreset::UnknownSiteAsk
                ),
                "{}",
                row.name()
            );
        }
    }

    #[test]
    fn always_and_never_rewrite_the_site_rule_and_the_others_leave_the_settings_alone() {
        let start = |row: OfferRow| row.preset().setup("h").unwrap().to_stored_value();
        for row in [OfferRow::PlayHere, OfferRow::PlayInFastiplayer] {
            assert_eq!(
                row.expected_settings("h"),
                Some(start(row)),
                "{}",
                row.name()
            );
        }
        let always = OfferRow::Always.expected_settings("h").unwrap();
        assert_eq!(always["site_rules"], json!({ "h": "always" }));
        assert_eq!(always["enabled"], json!(false));
        let never = OfferRow::Never.expected_settings("h").unwrap();
        assert_eq!(never["site_rules"], json!({ "h": "never" }));
    }

    #[test]
    fn only_the_fastiplayer_and_always_choices_end_in_the_player() {
        let handed: Vec<&str> = ALL
            .iter()
            .filter(|row| row.hands_off())
            .map(|row| row.name())
            .collect();
        assert_eq!(handed, ["ask-play-in-fastiplayer", "ask-always"]);
        assert_eq!(OfferRow::Shown.choice(), None);
    }
}
