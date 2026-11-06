//! Входы настоящей мышью: правый клик по странице, пункт родного меню, ожидание плашки.
//!
//! Цепь входа: калибровка (где окно на экране) -> правый клик в точку цели -> родное меню ->
//! клик по строке со значком экстеншена. Что вход РАБОТАЕТ, доказывает не клик, а то, что за
//! ним следует (плашка на странице, кадры в плеере): здесь только рука.

use std::time::{Duration, Instant};

use super::super::context::ScenarioError;
use super::common::ReadyRun;
use super::extension_probe::read_page_ui;
use super::native_menu::{
    ItemGrouping, MenuLook, MenuPick, right_click_and_look, right_click_and_pick,
};
use super::page_geometry::{POINTER_LINE_MARK, PointerSample, ScreenPlace, parse_pointer_line};
use crate::desktop_input::Point;
use crate::site::RunId;

/// Первая калибровочная точка экрана: окно браузера стенда стоит по центру (~x 500..1415), а содержимое страниц стенда занимает его левую часть (до ~x 1040), эта точка правее.
const CALIBRATION_FIRST: Point = Point { x: 1200, y: 700 };
/// Вторая точка: другой угол смещения, чтобы поймать масштаб и сдвиг окна.
const CALIBRATION_SECOND: Point = Point { x: 1260, y: 760 };
/// Сколько ждём строку зонда после движения мыши.
const POINTER_LINE_TIMEOUT: Duration = Duration::from_secs(10);
/// Пауза, чтобы все события мыши после движения дошли до журнала.
const POINTER_SETTLE: Duration = Duration::from_millis(400);
/// Шаг ожидания строки.
const POLL_STEP: Duration = Duration::from_millis(100);
/// Сколько ждём плашку на странице после входа (передача стартует секунды).
pub(super) const BANNER_TIMEOUT: Duration = Duration::from_secs(40);
/// Шаг опроса слоя страницы: плашка успеха живёт 5 секунд, опрос обязан быть заметно чаще.
const BANNER_POLL: Duration = Duration::from_millis(250);

/// Точка внутри цели страницы, куда кликает рука.
#[derive(Debug, Clone, Copy, PartialEq)]
pub(super) struct ClickSpot {
    /// Имя цели в зонде страницы (`video`, `video:second`, `iframe`, `around`, `link:autoplay`).
    pub(super) target: &'static str,
    /// Доля ширины цели слева направо.
    pub(super) share_x: f64,
    /// Доля высоты цели сверху вниз.
    pub(super) share_y: f64,
}

impl ClickSpot {
    /// Центр цели.
    pub(super) const fn center_of(target: &'static str) -> Self {
        Self {
            target,
            share_x: 0.5,
            share_y: 0.5,
        }
    }

    /// Главное видео страницы (его центр).
    pub(super) const VIDEO: Self = Self::center_of("video");
}

/// Область цели, где страница сама рисует отклик на наведение: панель управления видео и рамки
/// появляются под курсором и не являются меню. Исключаем только видео и фреймы (родное меню выше и
/// шире любого видео стенда). Остальные цели (пустое место, рамка вокруг видео, слой, ссылка) могут
/// целиком накрывать меню, и исключение спрятало бы его.
fn hover_exclusion(place: &ScreenPlace, spot: ClickSpot) -> Option<super::native_menu::Rect> {
    let is_media =
        spot.target == "video" || spot.target.starts_with("video:") || spot.target == "iframe";
    if is_media {
        place.screen_rect(spot.target)
    } else {
        None
    }
}

/// Правый клик внутри фрейма попадает под два пункта сразу (видео и фрейм): браузер складывает их в
/// подменю. На обычной странице пункт один.
fn grouping_of(spot: ClickSpot) -> ItemGrouping {
    if spot.target == "iframe" {
        ItemGrouping::Submenu
    } else {
        ItemGrouping::SingleItem
    }
}

impl ReadyRun<'_, '_> {
    /// Ждёт новую строку зонда после движения мыши в `at` и разбирает её.
    pub(super) fn sample_pointer_at(&self, at: Point) -> Result<PointerSample, ScenarioError> {
        self.sample_pointer_in(&self.run.run, at)
    }

    /// То же для страницы другого прогона `run` (страница по ссылке в своей вкладке).
    fn sample_pointer_in(&self, run: &RunId, at: Point) -> Result<PointerSample, ScenarioError> {
        let before = self.run.log_lines_of(run, POINTER_LINE_MARK).len();
        self.robot.move_pointer(at)?;
        let deadline = Instant::now() + POINTER_LINE_TIMEOUT;
        loop {
            let lines = self.run.log_lines_of(run, POINTER_LINE_MARK);
            if lines.len() > before {
                // Браузер сам досылает «мышь двинулась» после перекладки страницы (с устаревшими
                // координатами): даём событиям осесть и берём ПОСЛЕДНЮЮ строку, она про наш курсор.
                std::thread::sleep(POINTER_SETTLE);
                let latest = self.run.log_lines_of(run, POINTER_LINE_MARK);
                let line = latest.last().unwrap_or(&lines[before]);
                return parse_pointer_line(line).map_err(ScenarioError::EntryNotActivated);
            }
            if Instant::now() >= deadline {
                let _ = self.robot.take_screenshot("pointer-not-reported");
                return Err(ScenarioError::EntryNotActivated(format!(
                    "the page did not report the pointer at {at:?} within {POINTER_LINE_TIMEOUT:?}: the probe is deaf or the point is not over the page"
                )));
            }
            std::thread::sleep(POLL_STEP);
        }
    }

    /// Калибровка: где страница стоит на экране прямо сейчас (панели браузера двигаются, поэтому
    /// перед каждым входом заново).
    pub(super) fn locate_page(&self) -> Result<ScreenPlace, ScenarioError> {
        self.locate_page_of(&self.run.run)
    }

    /// Калибровка по зонду страницы другого прогона (вкладка по ссылке).
    pub(super) fn locate_page_of(&self, run: &RunId) -> Result<ScreenPlace, ScenarioError> {
        self.robot.bring_browser_forward()?;
        let first = self.sample_pointer_in(run, CALIBRATION_FIRST)?;
        let second = self.sample_pointer_in(run, CALIBRATION_SECOND)?;
        ScreenPlace::calibrate(CALIBRATION_FIRST, &first, CALIBRATION_SECOND, &second)
            .map_err(ScenarioError::EntryNotActivated)
    }

    /// Правый клик в `spot` и клик по пункту экстеншена в родном меню (по свежей калибровке).
    pub(super) fn right_click_and_pick(&self, spot: ClickSpot) -> Result<MenuPick, ScenarioError> {
        let place = self.locate_page()?;
        let click = place
            .point_in(spot.target, spot.share_x, spot.share_y)
            .map_err(ScenarioError::EntryNotActivated)?;
        right_click_and_pick(
            &self.robot,
            click,
            hover_exclusion(&place, spot),
            grouping_of(spot),
        )
    }

    /// Правый клик в `spot` только чтобы посмотреть меню (пункт не нажимается, меню остаётся открытым).
    pub(super) fn right_click_and_look(&self, spot: ClickSpot) -> Result<MenuLook, ScenarioError> {
        let place = self.locate_page()?;
        let click = place
            .point_in(spot.target, spot.share_x, spot.share_y)
            .map_err(ScenarioError::EntryNotActivated)?;
        right_click_and_look(&self.robot, click, hover_exclusion(&place, spot))
    }

    /// То же, но меню и пункт обязаны быть: иначе ошибка с причиной.
    pub(super) fn pick_from_native_menu(&self, spot: ClickSpot) -> Result<(), ScenarioError> {
        match self.right_click_and_pick(spot)? {
            MenuPick::Picked { .. } => Ok(()),
            MenuPick::ItemAbsent { painted_pixels } => {
                Err(ScenarioError::EntryNotActivated(format!(
                    "the native menu opened ({painted_pixels} px painted) but has no extension item"
                )))
            }
            MenuPick::MenuNotOpened { painted_pixels } => {
                Err(ScenarioError::EntryNotActivated(format!(
                    "the right click opened no menu ({painted_pixels} px painted outside the target)"
                )))
            }
        }
    }

    /// Ждёт плашку страницы с именем случая `expected` (`opened`, `switch_to_player`, ...).
    /// Возвращает, что показано на самом деле, если за срок увидели другое или ничего.
    pub(super) fn wait_for_banner(
        &mut self,
        expected: &str,
        timeout: Duration,
    ) -> Result<(), String> {
        let prefix = self.run.page_url_prefix();
        self.wait_for_banner_on(&prefix, expected, timeout)
    }

    /// То же на вкладке с адресом `prefix` (плашка идёт на вкладку входа, а не на исходную).
    pub(super) fn wait_for_banner_on(
        &mut self,
        prefix: &str,
        expected: &str,
        timeout: Duration,
    ) -> Result<(), String> {
        let deadline = Instant::now() + timeout;
        let mut seen: Vec<String> = Vec::new();
        let mut last_problem = String::new();
        loop {
            match read_page_ui(self.run.browser.as_mut(), prefix) {
                Ok(state) => match state.banner {
                    Some(banner) if banner == expected => return Ok(()),
                    Some(banner) if !seen.contains(&banner) => seen.push(banner),
                    Some(_) => {}
                    None => {}
                },
                Err(problem) => last_problem = problem,
            }
            if Instant::now() >= deadline {
                return Err(format!(
                    "no banner '{expected}' within {timeout:?}; banners seen: {seen:?}; last probe problem: {last_problem:?}"
                ));
            }
            std::thread::sleep(BANNER_POLL);
        }
    }
}
