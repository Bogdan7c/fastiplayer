//! Кнопка поверх видео (решение 53): выключена по умолчанию, включена настройкой без перезапуска.
//!
//! Строка 1: по умолчанию наведение на видео кнопку НЕ показывает; положительный контроль в той же
//! строке: включили настройку — то же наведение показывает кнопку (значит, зонд её видит).
//! Строка 2: кнопка включена, настоящий клик по ней отдаёт видео плееру (кадры, плашка `opened`),
//! а вёрстка страницы не сдвинулась: прямоугольник видео и размер прокручиваемой области те же
//! до и после показа кнопки.

use std::time::{Duration, Instant};

use super::super::context::{ScenarioError, Stand};
use super::super::outcome::Verdict;
use super::super::run_tweaks::RunTweaks;
use super::Entry;
use super::common::{
    DEFAULT_QUERY, EarlyUnsupported, Opened, ReadyRun, open_ready, with_extra_failures,
};
use super::entry_evidence::collect;
use super::extension_probe::{ButtonRect, EntrySettingsSetup, read_page_ui, write_entry_settings};
use super::menu_entry::ClickSpot;
use super::start_rows::prepare_entry;
use crate::browsers::BrowserKind;
use crate::desktop_input::Point;
use crate::environment::BenchEnvironment;

/// Страница обеих строк.
const PAGE: &str = "mse-fmp4";
/// Сколько даём настройке дойти до слоя страницы.
const SETTING_APPLY_WAIT: Duration = Duration::from_millis(1500);
/// Сколько ждём кнопку после наведения.
const BUTTON_TIMEOUT: Duration = Duration::from_secs(8);
/// Сколько наблюдаем отсутствие кнопки у выключенного входа (десятки периодов опроса зонда наведения).
const ABSENCE_OBSERVE: Duration = Duration::from_secs(3);
/// Сдвиги мыши при наведении: событий должно быть несколько, одного может не хватить.
const HOVER_WIGGLE: [(u32, u32); 3] = [(0, 0), (14, 8), (-6i32 as u32, 0)];
const OVERLAY_ON: EntrySettingsSetup = EntrySettingsSetup {
    overlay_button: true,
    ..EntrySettingsSetup::DEFAULT
};

/// Какая строка кнопки.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) enum OverlayRow {
    /// По умолчанию выключена, включённая показывается.
    OffByDefault,
    /// Включённая: клик запускает плеер, вёрстка на месте.
    PlaysAndKeepsLayout,
}

impl OverlayRow {
    pub(super) fn name(self) -> &'static str {
        match self {
            Self::OffByDefault => "overlay-off-by-default",
            Self::PlaysAndKeepsLayout => "overlay-click-plays-layout-intact",
        }
    }
}

/// Наводит мышь на видео несколькими движениями (кнопка слушает `mousemove`).
fn hover_video(ready: &ReadyRun<'_, '_>, over: Point) -> Result<(), ScenarioError> {
    for (dx, dy) in HOVER_WIGGLE {
        let moved = Point::new(over.x.wrapping_add(dx), over.y.wrapping_add(dy));
        ready.robot.move_pointer(moved)?;
        std::thread::sleep(Duration::from_millis(150));
    }
    Ok(())
}

/// Ждёт кнопку (или проверяет её отсутствие, когда `timeout` — наблюдение за отсутствием).
fn wait_for_button(
    ready: &mut ReadyRun<'_, '_>,
    timeout: Duration,
) -> Result<Option<ButtonRect>, String> {
    let prefix = ready.run.page_url_prefix();
    let deadline = Instant::now() + timeout;
    loop {
        let state = read_page_ui(ready.run.browser.as_mut(), &prefix)?;
        if state.overlay_button.is_some() || Instant::now() >= deadline {
            return Ok(state.overlay_button);
        }
        std::thread::sleep(Duration::from_millis(200));
    }
}

impl ReadyRun<'_, '_> {
    /// Рука: калибровка, наведение на видео, ожидание кнопки, проверка вёрстки и НАСТОЯЩИЙ клик по кнопке.
    pub(super) fn press_overlay_button(&mut self, spot: ClickSpot) -> Result<(), ScenarioError> {
        let place = self.locate_page()?;
        let video_before = place.client_rect(spot.target);
        let scroll_before = place.scroll_size();
        let over = place
            .point_in(spot.target, spot.share_x, spot.share_y)
            .map_err(ScenarioError::EntryNotActivated)?;
        hover_video(self, over)?;
        let button = wait_for_button(self, BUTTON_TIMEOUT)
            .map_err(ScenarioError::EntryNotActivated)?
            .ok_or_else(|| {
                ScenarioError::EntryNotActivated("hovering the video showed no button".to_owned())
            })?;
        let center = place.point_from_client(
            button.left + button.width / 2.0,
            button.top + button.height / 2.0,
        );
        // Курсор на кнопке: страница присылает свежие размеры уже ПРИ показанной кнопке.
        let shown = self.sample_pointer_at(center)?;
        let video_after = shown.rects.get(spot.target).copied();
        if video_after != video_before || shown.scroll_size != scroll_before {
            return Err(ScenarioError::EntryNotActivated(format!(
                "the page layout moved when the button appeared: video {video_before:?} -> {video_after:?}, scroll {scroll_before:?} -> {:?}",
                shown.scroll_size
            )));
        }
        self.robot.click_screen(center)?;
        Ok(())
    }
}

pub(super) fn run_overlay(
    stand: &Stand,
    environment: &BenchEnvironment,
    browser: BrowserKind,
    row: OverlayRow,
) -> Result<Verdict, ScenarioError> {
    let mut ready = match open_ready(
        stand,
        environment,
        browser,
        PAGE,
        &RunTweaks::default(),
        DEFAULT_QUERY,
        EarlyUnsupported::OnlyWhereTheTableAllows,
    )? {
        Opened::Ready(ready) => *ready,
        Opened::Finished(verdict) => return Ok(verdict),
    };
    prepare_entry(&mut ready, Entry::OverlayButton)?;
    match row {
        OverlayRow::OffByDefault => run_off_by_default(ready),
        OverlayRow::PlaysAndKeepsLayout => run_plays(ready),
    }
}

fn run_off_by_default(mut ready: ReadyRun<'_, '_>) -> Result<Verdict, ScenarioError> {
    let mut failures = Vec::new();
    let place = ready.locate_page()?;
    let over = place
        .point_in("video", 0.5, 0.5)
        .map_err(ScenarioError::EntryNotActivated)?;
    hover_video(&ready, over)?;
    let default_state =
        wait_for_button(&mut ready, ABSENCE_OBSERVE).map_err(ScenarioError::EntryNotActivated)?;
    if default_state.is_some() {
        failures.push(format!(
            "the button is shown by default although it must be off: {default_state:?}"
        ));
    }
    // Контроль: включённая настройка показывает кнопку при том же наведении.
    write_entry_settings(ready.run.browser.as_mut(), OVERLAY_ON)
        .map_err(ScenarioError::EntryNotActivated)?;
    std::thread::sleep(SETTING_APPLY_WAIT);
    hover_video(&ready, over)?;
    let enabled_state =
        wait_for_button(&mut ready, BUTTON_TIMEOUT).map_err(ScenarioError::EntryNotActivated)?;
    if enabled_state.is_none() {
        failures.push(
            "control: with the setting on, hovering the video must show the button".to_owned(),
        );
    }
    let _ = ready.robot.take_screenshot("end-of-row");
    ready.run.browser.shutdown()?;
    let summary = format!("by default: {default_state:?}; enabled: {enabled_state:?}");
    Ok(if failures.is_empty() {
        Verdict::pass(summary)
    } else {
        Verdict::fail(format!("{}\n{summary}", failures.join("; ")))
    })
}

fn run_plays(mut ready: ReadyRun<'_, '_>) -> Result<Verdict, ScenarioError> {
    write_entry_settings(ready.run.browser.as_mut(), OVERLAY_ON)
        .map_err(ScenarioError::EntryNotActivated)?;
    std::thread::sleep(SETTING_APPLY_WAIT);
    let mut failures = Vec::new();
    ready.enter_at(Entry::OverlayButton, ClickSpot::VIDEO)?;
    failures.extend(ready.wait_entry_started(Entry::OverlayButton));
    ready.run.tell_page("handed-off");
    let frames = ready.wait_frames()?;
    let evidence = collect(ready, frames)?;
    failures.extend(evidence.failures(PAGE));
    let summary = format!(
        "button pressed with a real click, layout unchanged; presented_frames={}",
        evidence.presented_frames()
    );
    Ok(with_extra_failures(Verdict::pass(summary), &failures))
}
