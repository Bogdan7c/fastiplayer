//! Матрица входов 10c: правый клик, ссылка, кнопка поверх видео, отказы без попапа.
//! Строки отдельные от общей матрицы `ROWS`: у этих входов другая форма (нет попапа, итог
//! читается с плашки на странице), но судья прежний — кадры в плеере и журналы.

use super::super::capture::{CapturePage, plain_page};
use super::super::context::{ScenarioError, Stand};
use super::super::outcome::Verdict;
use super::Entry;
use super::already_open_rows::{self, AlreadyOpenRow};
use super::disabled_rows::{self, DisabledRow};
use super::link_rows::{self, LinkRow};
use super::menu_entry::ClickSpot;
use super::overlay_rows::{self, OverlayRow};
use super::pick_rows::{self, LONG_FILE, PickRow, Precondition, SHORT_FILE};
use super::report_rows::{self, ReportRow};
use super::start_rows;
use crate::browsers::BrowserKind;
use crate::environment::BenchEnvironment;

/// Что проверяет строка.
#[derive(Debug, Clone, Copy)]
pub(super) enum MenuRowKind {
    /// Вход на странице с одним видео → кадры в рендере, плашка `opened`.
    Start(CapturePage),
    /// Выбор видео: в плеере то, по которому кликнули (журнал сервера).
    Pick(PickRow),
    /// Ссылка: вкладка спереди, кадры, конец сеанса, срок, страница забрала видео.
    Link(LinkRow),
    /// Выключенный в настройках вход не показывается.
    Disabled(DisabledRow),
    /// Отказ без попапа: плашка и `!` на значке.
    Report(ReportRow),
    /// Окно плеера уже открыто: плашка `switch_to_player`.
    AlreadyOpen(AlreadyOpenRow),
    /// Кнопка поверх видео.
    Overlay(OverlayRow),
}

/// Строка матрицы входов 10c.
#[derive(Debug, Clone, Copy)]
pub(super) struct MenuRow {
    pub(super) entry: Entry,
    pub(super) page_id: &'static str,
    pub(super) kind: MenuRowKind,
}

const fn start(entry: Entry, page_id: &'static str) -> MenuRow {
    MenuRow {
        entry,
        page_id,
        kind: MenuRowKind::Start(plain_page(page_id)),
    }
}

/// Строки по порядку: от простого к сложному.
pub(super) const MENU_ROWS: &[MenuRow] = &[
    start(Entry::VideoMenu, "mse-fmp4"),
    start(Entry::VideoMenu, "mse-webm"),
    start(Entry::VideoMenu, "video-src"),
    start(Entry::VideoMenu, "iframe-cross-origin"),
    start(Entry::VideoMenu, "shadow-dom"),
    pick(
        Entry::VideoMenu,
        "menu-two-videos",
        ClickSpot::center_of("video:first"),
        "pick-first-of-two",
        LONG_FILE,
        Some(SHORT_FILE),
        Precondition::None,
    ),
    pick(
        Entry::VideoMenu,
        "menu-two-videos",
        ClickSpot::center_of("video:second"),
        "pick-second-of-two",
        SHORT_FILE,
        Some(LONG_FILE),
        Precondition::None,
    ),
    pick(
        Entry::PageMenu,
        "menu-taken-contextmenu",
        AROUND_VIDEO,
        "site-takes-right-click",
        LONG_FILE,
        None,
        Precondition::SiteTakesRightClickOnVideo,
    ),
    pick(
        Entry::PageMenu,
        "menu-site-layer",
        ClickSpot::center_of("layer"),
        "site-layer-over-video",
        SHORT_FILE,
        Some(LONG_FILE),
        Precondition::None,
    ),
    link(LinkRow::Plays),
    link(LinkRow::ManualPlay),
    link(LinkRow::NoVideo),
    link(LinkRow::PageWithdraws),
    disabled(DisabledRow::VideoItem),
    disabled(DisabledRow::LinkItem),
    report(
        Entry::VideoMenu,
        "drm-clearkey",
        ClickSpot::VIDEO,
        "protected",
        false,
    ),
    report(
        Entry::PageMenu,
        "banner-autoplay",
        VIEWPORT_BLANK,
        "no_video",
        true,
    ),
    already_open(AlreadyOpenRow::SameVideoAgain),
    already_open(AlreadyOpenRow::OtherTab),
    overlay(OverlayRow::OffByDefault),
    overlay(OverlayRow::PlaysAndKeepsLayout),
];

const fn overlay(row: OverlayRow) -> MenuRow {
    MenuRow {
        entry: Entry::OverlayButton,
        page_id: "mse-fmp4",
        kind: MenuRowKind::Overlay(row),
    }
}

/// Пустое место страницы (правее и ниже видео, но высоко в окне, чтобы родное меню целиком
/// помещалось на экране: Firefox не сдвигает меню вверх, и пункт экстеншена уходил за край экрана).
const VIEWPORT_BLANK: ClickSpot = ClickSpot {
    target: "viewport",
    share_x: 0.5,
    share_y: 0.25,
};

const fn disabled(row: DisabledRow) -> MenuRow {
    MenuRow {
        entry: Entry::VideoMenu,
        page_id: row.page_id(),
        kind: MenuRowKind::Disabled(row),
    }
}

const fn report(
    entry: Entry,
    page_id: &'static str,
    click: ClickSpot,
    banner: &'static str,
    check_browser_decodes: bool,
) -> MenuRow {
    MenuRow {
        entry,
        page_id,
        kind: MenuRowKind::Report(ReportRow {
            entry,
            page_id,
            click,
            banner,
            check_browser_decodes,
        }),
    }
}

const fn already_open(row: AlreadyOpenRow) -> MenuRow {
    MenuRow {
        entry: Entry::VideoMenu,
        page_id: "mse-fmp4",
        kind: MenuRowKind::AlreadyOpen(row),
    }
}

const fn link(row: LinkRow) -> MenuRow {
    MenuRow {
        entry: Entry::LinkMenu,
        page_id: "link-source",
        kind: MenuRowKind::Link(row),
    }
}

const fn pick(
    entry: Entry,
    page_id: &'static str,
    click: ClickSpot,
    rule: &'static str,
    expected_file: &'static str,
    untouched_file: Option<&'static str>,
    precondition: Precondition,
) -> MenuRow {
    MenuRow {
        entry,
        page_id,
        kind: MenuRowKind::Pick(PickRow {
            entry,
            page_id,
            click,
            expected_file,
            untouched_file,
            precondition,
            rule,
        }),
    }
}

/// Доли точки «в рамке вокруг видео»: у левого края рамки, посередине высоты.
const AROUND_VIDEO: ClickSpot = ClickSpot {
    target: "around",
    share_x: 0.05,
    share_y: 0.5,
};

/// Подпись строки таблицы.
pub(super) fn subject(row: &MenuRow) -> String {
    let rule = match row.kind {
        MenuRowKind::Start(_) => "start",
        MenuRowKind::Pick(pick) => pick.rule,
        MenuRowKind::Link(link) => link.name(),
        MenuRowKind::Disabled(disabled) => disabled.name(),
        MenuRowKind::Report(report) => report.banner,
        MenuRowKind::AlreadyOpen(row) => row.name(),
        MenuRowKind::Overlay(row) => row.name(),
    };
    format!("{} x {} [{rule}]", row.entry.name(), row.page_id)
}

/// Прогон одной строки в готовом окружении.
pub(super) fn run(
    stand: &Stand,
    environment: &BenchEnvironment,
    browser: BrowserKind,
    row: MenuRow,
) -> Result<Verdict, ScenarioError> {
    match row.kind {
        MenuRowKind::Start(page) => {
            start_rows::run_start(stand, environment, browser, row.entry, page)
        }
        MenuRowKind::Pick(pick) => pick_rows::run_pick(stand, environment, browser, pick),
        MenuRowKind::Link(link) => link_rows::run_link(stand, environment, browser, link),
        MenuRowKind::Disabled(disabled) => {
            disabled_rows::run_disabled(stand, environment, browser, disabled)
        }
        MenuRowKind::Report(report) => report_rows::run_report(stand, environment, browser, report),
        MenuRowKind::Overlay(overlay) => {
            overlay_rows::run_overlay(stand, environment, browser, overlay)
        }
        MenuRowKind::AlreadyOpen(open_row) => {
            already_open_rows::run_already_open(stand, environment, browser, row.entry, open_row)
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::scenarios::page_catalog::PAGE_SCENARIOS;

    #[test]
    fn every_menu_row_has_a_unique_subject_and_a_known_stand_page() {
        let mut seen = std::collections::HashSet::new();
        for row in MENU_ROWS {
            assert!(
                seen.insert(subject(row)),
                "duplicate subject {}",
                subject(row)
            );
            assert!(
                PAGE_SCENARIOS.iter().any(|known| known.id == row.page_id),
                "{} is not a stand page",
                row.page_id
            );
        }
    }

    #[test]
    fn the_matrix_covers_every_entry_of_the_assignment() {
        for entry in [
            Entry::VideoMenu,
            Entry::PageMenu,
            Entry::LinkMenu,
            Entry::OverlayButton,
        ] {
            assert!(
                MENU_ROWS.iter().any(|row| row.entry == entry),
                "{entry:?} has no row"
            );
        }
        for page in [
            "mse-fmp4",
            "mse-webm",
            "video-src",
            "iframe-cross-origin",
            "shadow-dom",
            "menu-two-videos",
            "menu-taken-contextmenu",
            "menu-site-layer",
            "link-source",
            "drm-clearkey",
            "banner-autoplay",
        ] {
            assert!(
                MENU_ROWS.iter().any(|row| row.page_id == page),
                "{page} has no menu row"
            );
        }
    }

    #[test]
    fn a_pick_row_names_a_file_the_other_video_does_not_share() {
        for row in MENU_ROWS {
            if let MenuRowKind::Pick(pick) = row.kind {
                assert_ne!(Some(pick.expected_file), pick.untouched_file);
            }
        }
    }
}
