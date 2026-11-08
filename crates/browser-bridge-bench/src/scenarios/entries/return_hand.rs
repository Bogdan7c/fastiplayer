//! Рука сценария `page-return` (сессия 11a): узкий вход в мышиную механику `entries` для заглушки
//! «Playing in Fastiplayer» и кнопки «Return».
//!
//! Зачем отдельный модуль. Калибровка экрана, настоящий клик, зонд слоя экстеншена и вход по ссылке
//! живут в `entries` и видны только внутри него. Сценарию возврата нужна малая часть: открыть
//! страницу с роботом, прочитать заглушку, узнать, где на экране видео и кнопка, кликнуть и войти по
//! ссылке. Этот модуль отдаёт ровно это, своими типами данных, не раскрывая внутренности `entries`.

use std::time::Duration;

use super::super::context::{ScenarioError, Stand};
use super::super::handoff_run::HandoffRun;
use super::super::outcome::Verdict;
use super::super::run_tweaks::RunTweaks;
use super::Entry;
use super::common::{EarlyUnsupported, Opened, ReadyRun, open_ready};
use super::extension_probe::{ButtonRect, PlaceholderState, read_page_ui};
use super::link_rows::{check_tab_opened_and_active, tab_still_open, target_run};
use super::menu_entry::ClickSpot;
use super::page_geometry::{ClientRect, ScreenPlace};
use super::start_rows::prepare_entry;
use crate::browsers::BrowserKind;
use crate::desktop_input::Point;
use crate::environment::BenchEnvironment;
use crate::site::RunId;

/// Прямоугольник в клиентских координатах окна страницы (то, что дают `getBoundingClientRect` и зонды).
#[derive(Debug, Clone, Copy, PartialEq)]
pub(in crate::scenarios) struct ClientBox {
    pub(in crate::scenarios) left: f64,
    pub(in crate::scenarios) top: f64,
    pub(in crate::scenarios) width: f64,
    pub(in crate::scenarios) height: f64,
}

impl From<ButtonRect> for ClientBox {
    fn from(rect: ButtonRect) -> Self {
        Self {
            left: rect.left,
            top: rect.top,
            width: rect.width,
            height: rect.height,
        }
    }
}

impl From<ClientRect> for ClientBox {
    fn from(rect: ClientRect) -> Self {
        Self {
            left: rect.left,
            top: rect.top,
            width: rect.width,
            height: rect.height,
        }
    }
}

/// Заглушка глазами зонда экстеншена.
#[derive(Debug, Clone, Copy, PartialEq, Default)]
pub(in crate::scenarios) struct PlaceholderView {
    /// Изолированный мир знает переданное видео.
    pub(in crate::scenarios) handed_off: bool,
    /// Панель видна.
    pub(in crate::scenarios) visible: bool,
    pub(in crate::scenarios) panel: Option<ClientBox>,
    pub(in crate::scenarios) return_button: Option<ClientBox>,
}

impl From<PlaceholderState> for PlaceholderView {
    fn from(state: PlaceholderState) -> Self {
        Self {
            handed_off: state.handed_off,
            visible: state.visible,
            panel: state.panel.map(ClientBox::from),
            return_button: state.return_button.map(ClientBox::from),
        }
    }
}

/// Где страница на экране прямо сейчас (свежая калибровка) и где на тот миг лежало её видео.
pub(in crate::scenarios) struct PagePlace(ScreenPlace);

impl PagePlace {
    /// Прямоугольник главного видео страницы на момент калибровки.
    pub(in crate::scenarios) fn video_rect(&self) -> Option<ClientBox> {
        self.0.client_rect("video").map(ClientBox::from)
    }

    /// Точка экрана внутри прямоугольника `area` (доли ширины и высоты, 0.5 — центр).
    pub(in crate::scenarios) fn screen_point_in(
        &self,
        area: ClientBox,
        share_x: f64,
        share_y: f64,
    ) -> Point {
        self.0.point_from_client(
            area.left + area.width * share_x,
            area.top + area.height * share_y,
        )
    }
}

/// Чем кончилось открытие страницы с рукой.
pub(in crate::scenarios) enum HandOpened<'a, 'env> {
    /// Страница играет и ждёт команд, рука на месте.
    Ready(Box<ReturnHand<'a, 'env>>),
    /// Страница отчиталась раньше (`unsupported` там, где каталог разрешает) или не дошла до готовности.
    Finished(Verdict),
}

/// Прогон страницы с настоящей рукой.
pub(in crate::scenarios) struct ReturnHand<'a, 'env> {
    ready: ReadyRun<'a, 'env>,
}

/// Открывает страницу стенда (`query` — хвост адреса после `run=…`) с рукой на невидимом экране.
/// Нужно окружение с правом на настоящий ввод (`DesktopSetup::pointer_stand`).
pub(in crate::scenarios) fn open_hand<'a, 'env>(
    stand: &'a Stand,
    environment: &'env BenchEnvironment,
    browser: BrowserKind,
    page_id: &'static str,
    query: &str,
) -> Result<HandOpened<'a, 'env>, ScenarioError> {
    Ok(
        match open_ready(
            stand,
            environment,
            browser,
            page_id,
            &RunTweaks::default(),
            query,
            EarlyUnsupported::OnlyWhereTheTableAllows,
        )? {
            Opened::Ready(ready) => HandOpened::Ready(Box::new(ReturnHand { ready: *ready })),
            Opened::Finished(verdict) => HandOpened::Finished(verdict),
        },
    )
}

/// Вход по ссылке состоялся: вкладка цели открыта, её страница под своим прогоном.
pub(in crate::scenarios) struct LinkEntered {
    /// Прогон страницы цели (`<прогон>-t`).
    pub(in crate::scenarios) target_run: RunId,
    /// Что видно про вкладку (для сводки строки).
    pub(in crate::scenarios) note: String,
    /// Провалы входа (вкладка не открылась или не активна).
    pub(in crate::scenarios) failures: Vec<String>,
}

impl<'a> ReturnHand<'a, '_> {
    /// Прогон страницы (команды, журналы, браузер).
    pub(in crate::scenarios) fn run(&mut self) -> &mut HandoffRun<'a> {
        &mut self.ready.run
    }

    /// То же только для чтения.
    pub(in crate::scenarios) fn run_ref(&self) -> &HandoffRun<'a> {
        &self.ready.run
    }

    /// Отдать прогон (закрыть браузер, собрать журналы); рука больше не нужна.
    pub(in crate::scenarios) fn into_run(self) -> HandoffRun<'a> {
        self.ready.run
    }

    /// Открывает служебную страницу фоновой вкладкой: через неё читается слой экстеншена на странице.
    /// Звать ПОСЛЕ передачи роботом (`hand_off`): тот сам открывает и закрывает свою служебную страницу.
    pub(in crate::scenarios) fn watch_page_layer(&mut self) -> Result<(), ScenarioError> {
        prepare_entry(&mut self.ready, Entry::LinkMenu)
    }

    /// Заглушка на вкладке с адресом `prefix` (страница стенда без строки запроса).
    pub(in crate::scenarios) fn placeholder_on(
        &mut self,
        prefix: &str,
    ) -> Result<PlaceholderView, String> {
        read_page_ui(self.ready.run.browser.as_mut(), prefix)
            .map(|state| PlaceholderView::from(state.placeholder))
    }

    /// Калибровка по зонду страницы прогона `run`: браузер выводится вперёд, мышь проходит две точки.
    pub(in crate::scenarios) fn locate(&self, run: &RunId) -> Result<PagePlace, ScenarioError> {
        self.ready.locate_page_of(run).map(PagePlace)
    }

    /// Настоящий левый клик в точку экрана.
    pub(in crate::scenarios) fn click(&self, point: Point) -> Result<(), ScenarioError> {
        self.ready.robot.click_screen(point)
    }

    /// Снимок экрана в каталог прогона (для разбора провала).
    pub(in crate::scenarios) fn screenshot(&self, label: &str) {
        // Снимок — лишь подсказка к провалу: его отсутствие провал не меняет и не прячет.
        let _ = self.ready.robot.take_screenshot(label);
    }

    /// Вход «правый клик по ссылке» (решение 217): `link` — цель зонда на странице-источнике,
    /// `target_prefix` — адрес страницы, на которую она ведёт. Служебная страница уже открыта.
    pub(in crate::scenarios) fn enter_by_link(
        &mut self,
        link: &'static str,
        target_prefix: &str,
    ) -> Result<LinkEntered, ScenarioError> {
        self.ready
            .enter_at(Entry::LinkMenu, ClickSpot::center_of(link))?;
        let mut failures = Vec::new();
        let note = check_tab_opened_and_active(&mut self.ready, target_prefix, &mut failures);
        Ok(LinkEntered {
            target_run: target_run(&self.ready)?,
            note,
            failures,
        })
    }

    /// Плашка `expected` на вкладке `prefix` (у входа по ссылке — «opened» на вкладке цели).
    pub(in crate::scenarios) fn wait_banner_on(
        &mut self,
        prefix: &str,
        expected: &str,
        timeout: Duration,
    ) -> Result<(), String> {
        self.ready.wait_for_banner_on(prefix, expected, timeout)
    }

    /// Вкладка `prefix` всё ещё открыта (иначе причина).
    pub(in crate::scenarios) fn tab_problem(&mut self, prefix: &str) -> Option<String> {
        tab_still_open(&mut self.ready, prefix)
    }
}
