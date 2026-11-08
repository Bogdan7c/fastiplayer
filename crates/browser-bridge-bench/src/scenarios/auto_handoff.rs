//! `auto-handoff` (сессия 12a): режим 2 «автозапуск» — человек сам нажимает play на видео со звуком, и
//! экстеншен без кнопки «Отдать» уводит его в плеер (тот же движок, что у режима 1). Настоящая мышь в
//! невидимом kwin, настоящий экстеншен; настройка режима пишется в `chrome.storage.local` экстеншена из
//! его служебной страницы (`settings`), как это сделала бы страница настроек.
//!
//! Автовоспроизведение без клика в стенде РАЗРЕШЕНО (`autoplay_without_gesture`): именно это доказывает,
//! что экстеншен смотрит на настоящий жест человека, а не на то, что видео заиграло.
//!
//! Что доказывает каждая строка (подробности — в модулях строк):
//!  * `default-off` — режим выключен (как у нового пользователя), клик по Play: передачи нет, фильм играет у браузера;
//!  * `click-hands-off` — режим включён, клик по Play на фильме 60 с со звуком: кадры в рендере плеера
//!    (`presented_frames`), заглушка «Return», браузер почти не декодирует (как судит `capture`);
//!  * `muted-autoplay` — беззвучный баннер, который заиграл сам: передачи нет, баннер играет у браузера;
//!  * `hover-preview` — превью по наведению мыши (беззвучное, без клика): передачи нет;
//!  * `click-beside-feed` — лента (решение 262): клик по «Like» под видео, ролик страница запускает сама —
//!    передачи нет, хотя браузерная активация жива и фильтры выключены;
//!  * `short-clip` — ролик 30 с со звуком по клику (короче порога, фильтры включены): передачи нет;
//!  * `short-clip-default-filters` — тот же ролик при фильтрах по умолчанию (все выключены, решение 261): передача;
//!  * `preroll-to-film` — реклама перед фильмом по клику: передача уже на рекламе, потом сеанс
//!    переезжает на фильм в том же окне;
//!  * `site-rule-never` и `own-stop-list` — правило сайта «никогда» и сайт в своём стоп-листе: передачи нет;
//!  * `after-return` — после «Return» (настоящий клик) страница играет сама, новой передачи нет;
//!    настоящая пауза и новый клик по Play — передача снова;
//!  * `three-tabs` — три вкладки подряд: играет последняя, две первые на паузе на своих секундах;
//!  * `live-hls`, `unknown-duration` — эфир HLS и поток без длины (`duration = Infinity`): передача сразу;
//!  * `ask-*` (12b, модуль `rows_offer`) — предложение «Ask»: `ask-offer-shown` (висит без срока, видео стоит),
//!    `ask-play-here`, `ask-play-in-fastiplayer`, `ask-always`, `ask-never` — настоящий клик по кнопке панели и
//!    исход (браузер или плеер) вместе с правилом сайта в хранилище экстеншена.
//!
//! Каждая строка «нет передачи» обязана доказать и положительный факт — страница правда играла у
//! браузера (настоящие кадры растут), настоящий клик дошёл до неё: иначе мёртвый стенд прошёл бы всё.
//! Судьи чистые (`judge`), факты страницы пишет `site/lib/auto-handoff-kit.js`, разбор — `facts`.

mod facts;
mod judge;
mod offer_judge;
mod rows_click;
mod rows_flow;
mod rows_offer;
mod settings;
mod steps;

use std::time::Instant;

use self::rows_click::{ClickRow, Expected, Gesture};
use self::rows_offer::OfferRow;
use self::settings::SettingsPreset;
use super::context::{ScenarioError, Stand};
use super::outcome::{StepResult, Verdict};
use super::page_filter::PageFilter;
use crate::browsers::BrowserKind;
use crate::environment::{BenchEnvironment, DesktopSetup, PlayerConfigProfile};

/// Переменная окружения для отладки: прогонять только строки, в подписи которых есть этот текст
/// (`BRIDGE_BENCH_AUTO_HANDOFF_ROW=three-tabs`). Без неё идут все строки, отобранные `--page`.
const SUBJECT_FILTER_VARIABLE: &str = "BRIDGE_BENCH_AUTO_HANDOFF_ROW";
/// Фильм 60 с со звуком и кнопками Play / Pause.
const FILM_PAGE: &str = "auto-film";
/// Ролик 30 с со звуком (MSE) и кнопка Play.
const CLIP_PAGE: &str = "auto-clip";
/// Поток MSE без известной длины и кнопка Play.
const UNKNOWN_DURATION_PAGE: &str = "auto-unknown-duration";
/// Нативный HLS LIVE и кнопка Play (только Chromium).
const LIVE_PAGE: &str = "auto-live";
/// Превью по наведению мыши.
const HOVER_PAGE: &str = "auto-hover-preview";
/// Реклама перед фильмом по клику.
const PREROLL_PAGE: &str = "auto-preroll";
/// Лента: клик по «Like» под видео, ролик страница запускает сама через 0,3 с (решение 262).
const FEED_PAGE: &str = "auto-feed";
/// Беззвучный баннер и превью, которые играют сами (страница режима 1, переиспользована).
const BANNER_PAGE: &str = "banner-autoplay";

/// Что проверяет строка.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum AutoRow {
    /// Одна вкладка, один жест, приговор по описанию.
    Click(&'static str, ClickRow),
    /// Баннер, заигравший сам, при включённом режиме.
    MutedAutoplay,
    PrerollToFilm,
    AfterReturn,
    ThreeTabs,
    /// Предложение «Ask» и выбор человека (12b).
    Offer(OfferRow),
}

impl AutoRow {
    fn name(self) -> &'static str {
        match self {
            Self::Click(name, _) => name,
            Self::MutedAutoplay => "muted-autoplay",
            Self::PrerollToFilm => "preroll-to-film",
            Self::AfterReturn => "after-return",
            Self::ThreeTabs => "three-tabs",
            Self::Offer(row) => row.name(),
        }
    }
}

/// Строка таблицы: страница (для `--page`) и правило.
#[derive(Debug, Clone, Copy)]
struct Row {
    page_id: &'static str,
    kind: AutoRow,
}

const fn click_row(
    name: &'static str,
    page_id: &'static str,
    preset: SettingsPreset,
    gesture: Gesture,
    expected: Expected,
) -> Row {
    Row {
        page_id,
        kind: AutoRow::Click(
            name,
            ClickRow {
                page_id,
                preset,
                gesture,
                expected,
            },
        ),
    }
}

const fn flow_row(page_id: &'static str, kind: AutoRow) -> Row {
    Row { page_id, kind }
}

/// Строки по порядку: от выключенного режима к передаче, ложным срабатываниям, фильтрам и цепочкам.
const ROWS: &[Row] = &[
    click_row(
        "default-off",
        FILM_PAGE,
        SettingsPreset::UntouchedDefault,
        Gesture::ClickPlay,
        Expected::StaysInBrowser,
    ),
    click_row(
        "click-hands-off",
        FILM_PAGE,
        SettingsPreset::On,
        Gesture::ClickPlay,
        Expected::HandedOff { endless: false },
    ),
    flow_row(BANNER_PAGE, AutoRow::MutedAutoplay),
    click_row(
        "hover-preview",
        HOVER_PAGE,
        SettingsPreset::On,
        Gesture::HoverPreview,
        Expected::StaysInBrowser,
    ),
    // Находка M2 на X: клик мимо видео, а лента сама запустила беззвучный ролик — передачи нет.
    click_row(
        "click-beside-feed",
        FEED_PAGE,
        SettingsPreset::OnDefaultFilters,
        Gesture::ClickBesideVideo,
        Expected::StaysInBrowser,
    ),
    click_row(
        "short-clip",
        CLIP_PAGE,
        SettingsPreset::On,
        Gesture::ClickPlay,
        Expected::StaysInBrowser,
    ),
    // Тот же ролик при фильтрах по умолчанию (решение 261: все выключены) уходит в плеер.
    click_row(
        "short-clip-default-filters",
        CLIP_PAGE,
        SettingsPreset::OnDefaultFilters,
        Gesture::ClickPlay,
        Expected::HandedOff { endless: false },
    ),
    flow_row(PREROLL_PAGE, AutoRow::PrerollToFilm),
    click_row(
        "site-rule-never",
        FILM_PAGE,
        SettingsPreset::SiteRuleNever,
        Gesture::ClickPlay,
        Expected::StaysInBrowser,
    ),
    click_row(
        "own-stop-list",
        FILM_PAGE,
        SettingsPreset::OwnStopList,
        Gesture::ClickPlay,
        Expected::StaysInBrowser,
    ),
    flow_row(FILM_PAGE, AutoRow::AfterReturn),
    flow_row(FILM_PAGE, AutoRow::ThreeTabs),
    click_row(
        "live-hls",
        LIVE_PAGE,
        SettingsPreset::On,
        Gesture::ClickPlay,
        Expected::HandedOff { endless: true },
    ),
    click_row(
        "unknown-duration",
        UNKNOWN_DURATION_PAGE,
        SettingsPreset::On,
        Gesture::ClickPlay,
        Expected::HandedOff { endless: true },
    ),
    flow_row(FILM_PAGE, AutoRow::Offer(OfferRow::Shown)),
    flow_row(FILM_PAGE, AutoRow::Offer(OfferRow::PlayHere)),
    flow_row(FILM_PAGE, AutoRow::Offer(OfferRow::PlayInFastiplayer)),
    flow_row(FILM_PAGE, AutoRow::Offer(OfferRow::Always)),
    flow_row(FILM_PAGE, AutoRow::Offer(OfferRow::Never)),
];

/// Подпись строки таблицы: правило и страница.
fn subject(row: &Row) -> String {
    format!("{} x {}", row.kind.name(), row.page_id)
}

/// Идентификаторы страниц сценария (для проверки `--page` в командной строке).
pub(super) fn page_ids() -> Vec<&'static str> {
    let mut ids: Vec<&'static str> = Vec::new();
    for row in ROWS {
        if !ids.contains(&row.page_id) {
            ids.push(row.page_id);
        }
    }
    ids
}

/// Все строки одного браузера, каждая в свежем окружении с правом на настоящий ввод.
pub(super) fn run_all(stand: &Stand, browser: BrowserKind, filter: &PageFilter) -> Vec<StepResult> {
    let subject_filter = std::env::var(SUBJECT_FILTER_VARIABLE).ok();
    ROWS.iter()
        .filter(|row| filter.admits(row.page_id))
        .filter(|row| {
            subject_filter
                .as_deref()
                .is_none_or(|part| subject(row).contains(part))
        })
        .map(|row| run_row(stand, browser, *row))
        .collect()
}

fn run_row(stand: &Stand, browser: BrowserKind, row: Row) -> StepResult {
    let started = Instant::now();
    let verdict = stand.with_environment_on_desktop(
        PlayerConfigProfile::Clean,
        DesktopSetup::pointer_stand(),
        |environment| drive(stand, environment, browser, row),
    );
    StepResult::new(
        "auto-handoff",
        browser,
        subject(&row),
        verdict,
        started.elapsed(),
    )
}

fn drive(
    stand: &Stand,
    environment: &BenchEnvironment,
    browser: BrowserKind,
    row: Row,
) -> Result<Verdict, ScenarioError> {
    match row.kind {
        AutoRow::Click(_, click) => rows_click::run_click_row(stand, environment, browser, click),
        AutoRow::MutedAutoplay => {
            rows_click::run_banner_row(stand, environment, browser, row.page_id, SettingsPreset::On)
        }
        AutoRow::PrerollToFilm => rows_flow::run_preroll(stand, environment, browser),
        AutoRow::AfterReturn => rows_flow::run_after_return(stand, environment, browser),
        AutoRow::ThreeTabs => rows_flow::run_three_tabs(stand, environment, browser),
        AutoRow::Offer(offer) => rows_offer::run_offer_row(stand, environment, browser, offer),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::scenarios::page_catalog::PAGE_SCENARIOS;

    fn site_file(relative: &str) -> String {
        let path = std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
            .join("site")
            .join(relative);
        std::fs::read_to_string(&path).unwrap_or_else(|error| panic!("{}: {error}", path.display()))
    }

    /// Страница вместе с общей библиотекой режима 2: команды и цели объявляет она.
    fn page_source(id: &str) -> String {
        let mut source = site_file(&format!("pages/{id}.html"));
        source.push_str(&site_file("lib/auto-handoff-kit.js"));
        source
    }

    #[test]
    fn every_row_page_is_a_known_stand_page_and_subjects_are_unique() {
        let mut seen = std::collections::HashSet::new();
        for row in ROWS {
            assert!(
                PAGE_SCENARIOS.iter().any(|page| page.id == row.page_id),
                "{} is not a stand page",
                row.page_id
            );
            assert!(
                seen.insert(subject(row)),
                "duplicate subject {}",
                subject(row)
            );
        }
    }

    #[test]
    fn the_table_covers_every_row_of_the_assignment() {
        let names: Vec<&str> = ROWS.iter().map(|row| row.kind.name()).collect();
        for wanted in [
            "default-off",
            "click-hands-off",
            "muted-autoplay",
            "hover-preview",
            "click-beside-feed",
            "short-clip",
            "short-clip-default-filters",
            "preroll-to-film",
            "site-rule-never",
            "own-stop-list",
            "after-return",
            "three-tabs",
            "live-hls",
            "unknown-duration",
            "ask-offer-shown",
            "ask-play-here",
            "ask-play-in-fastiplayer",
            "ask-always",
            "ask-never",
        ] {
            assert!(names.contains(&wanted), "row {wanted} is missing");
        }
    }

    #[test]
    fn rows_that_must_not_hand_off_never_share_a_preset_with_the_ones_that_must() {
        // «Нет передачи» из-за выключенного режима / правила / стоп-листа не должна исходить от
        // настройки, при которой режим 2 сработал бы: пары «включено» и «выключено» на одной странице.
        for row in ROWS {
            if let AutoRow::Click(name, click) = row.kind {
                let handed_off = matches!(click.expected, Expected::HandedOff { .. });
                let blocked_by_settings = matches!(
                    click.preset,
                    SettingsPreset::UntouchedDefault
                        | SettingsPreset::SiteRuleNever
                        | SettingsPreset::OwnStopList
                );
                assert!(
                    !(handed_off && blocked_by_settings),
                    "{name} expects a hand-off with a blocking preset"
                );
            }
        }
        let film_click_rows: Vec<&Row> = ROWS
            .iter()
            .filter(|row| row.page_id == FILM_PAGE && matches!(row.kind, AutoRow::Click(..)))
            .collect();
        assert!(
            film_click_rows.len() >= 4,
            "the film needs its on/off/rule/stop-list rows"
        );
    }

    /// Команды и цели, которые шлёт и кликает робот, объявляют страницы и их общая библиотека.
    #[test]
    fn every_command_and_target_the_robot_uses_is_declared_by_the_pages() {
        let kit = site_file("lib/auto-handoff-kit.js");
        for command in [
            "hook:mark-window",
            "hook:report-window",
            "hook:report-state",
        ] {
            assert!(kit.contains(&format!("'{command}'")), "{command}");
        }
        for line in [
            "auto-window ",
            "auto-state ",
            "play-clicked",
            "pause-clicked",
        ] {
            assert!(kit.contains(line), "the kit does not write '{line}'");
        }
        for row in ROWS {
            if let AutoRow::Click(name, click) = row.kind {
                let source = page_source(click.page_id);
                match click.gesture {
                    Gesture::ClickPlay => {
                        assert!(source.contains("addPlayControls"), "{name}: no Play button");
                        assert!(source.contains("registerEntryTarget('play'"), "{name}");
                    }
                    Gesture::HoverPreview => {
                        assert!(source.contains("registerEntryTarget('preview'"), "{name}");
                        assert!(source.contains(facts::PREVIEW_HOVER_LINE), "{name}");
                    }
                    Gesture::ClickBesideVideo => {
                        assert!(source.contains("registerEntryTarget('like'"), "{name}");
                        assert!(source.contains(facts::LIKE_CLICKED_LINE), "{name}");
                    }
                }
            }
        }
        // «Return» после клика ищет Pause на странице фильма.
        assert!(page_source(FILM_PAGE).contains("registerEntryTarget('pause'"));
        // Реклама по клику: кнопка Play и команда `handed-off` прячутся в каркасе `ad-hop-kit`.
        let preroll = page_source(PREROLL_PAGE);
        assert!(preroll.contains("registerEntryTarget('play'"));
        assert!(
            preroll.contains("film-started")
                || site_file("lib/ad-hop-kit.js").contains("film-started")
        );
    }

    /// Страницы НЕ играют сами: автозапуск разрешён в стенде, и страница, стартующая без клика,
    /// сделала бы «нет передачи» ничего не значащим.
    #[test]
    fn the_click_pages_wait_for_the_click_instead_of_autoplaying() {
        for page in [
            FILM_PAGE,
            CLIP_PAGE,
            UNKNOWN_DURATION_PAGE,
            LIVE_PAGE,
            HOVER_PAGE,
        ] {
            let source = site_file(&format!("pages/{page}.html"));
            assert!(
                source.contains("-waits-for-the-click") || source.contains("waits-for-the-mouse"),
                "{page} does not check that it waits for the person"
            );
            assert!(
                !source.contains("autoplay: true") && !source.contains("autoplay = true"),
                "{page} autoplays"
            );
        }
    }

    #[test]
    fn the_unknown_duration_page_really_has_no_finite_length() {
        let page = site_file(&format!("pages/{UNKNOWN_DURATION_PAGE}.html"));
        assert!(page.contains("duration_s: Infinity"));
        assert!(page.contains("video.duration === Infinity"));
    }

    #[test]
    fn page_ids_list_every_row_page_once() {
        let ids = page_ids();
        assert_eq!(
            ids.len(),
            ids.iter().collect::<std::collections::HashSet<_>>().len()
        );
        for page in [
            FILM_PAGE,
            CLIP_PAGE,
            UNKNOWN_DURATION_PAGE,
            LIVE_PAGE,
            HOVER_PAGE,
            PREROLL_PAGE,
            BANNER_PAGE,
        ] {
            assert!(ids.contains(&page), "{page}");
        }
    }

    #[test]
    fn firefox_may_report_the_live_page_unsupported_and_nothing_else_here() {
        for page in PAGE_SCENARIOS
            .iter()
            .filter(|page| page.id.starts_with("auto-"))
        {
            assert_eq!(
                page.may_be_unsupported_in(BrowserKind::Firefox),
                page.id == LIVE_PAGE,
                "{}",
                page.id
            );
            assert!(
                !page.may_be_unsupported_in(BrowserKind::Chromium),
                "{}",
                page.id
            );
        }
    }
}
