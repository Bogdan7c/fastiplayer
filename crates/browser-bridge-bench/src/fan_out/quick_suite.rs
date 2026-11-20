//! Быстрый набор стенда (`quick`, сессия 16b3c2, решение 386; список утвердил владелец).
//!
//! Главные строки сайтов с контентом и по одной строке лент на устройство. Каждая строка —
//! своя часть (дочерний процесс), выбранная страницей и, где на странице несколько строк,
//! отбором строки ([`RowFilter`]). Часть обязана выдать ровно ожидаемые строки
//! ([`ExpectedRow`]): опечатка в отборе не превратится в пустой «зелёный» прогон.

use browser_bridge_bench::browsers::BrowserKind;
use browser_bridge_bench::scenarios::RowFilter;

use super::plan::{ExpectedRow, RunPart, StartOrder};
use crate::cli::Command;

/// Одна строка быстрого набора.
#[derive(Debug, Clone, Copy)]
struct QuickEntry {
    /// Команда стенда.
    command: Command,
    /// Страница команды.
    page: &'static str,
    /// Отбор строки на странице, где строк несколько.
    row: Option<RowFilter>,
    /// Строки таблицы, которые часть обязана выдать.
    expected: &'static [ExpectedRow],
    /// Долгие строки (серия ждёт следующую в реальном времени) стартуют первыми.
    start: StartOrder,
}

/// Строка таблицы `capture` с этим предметом.
const fn capture(subject: &'static str) -> ExpectedRow {
    ExpectedRow {
        command: "capture",
        subject,
    }
}

/// Список быстрого набора. Порядок — как его утвердил владелец (контент, потом ленты).
const QUICK_SUITE: [QuickEntry; 16] = [
    // 1. MSE fMP4 (H.264, как YouTube); контроль линейки — браузер без экстеншена играет сам.
    QuickEntry {
        command: Command::Capture,
        page: "mse-fmp4",
        row: None,
        expected: &[capture("(control) mse-fmp4"), capture("mse-fmp4")],
        start: StartOrder::Normal,
    },
    // 2. MSE WebM (VP9/Opus, как YouTube).
    QuickEntry {
        command: Command::Capture,
        page: "mse-webm",
        row: None,
        expected: &[capture("mse-webm")],
        start: StartOrder::Normal,
    },
    // 3. Смена качества на ходу.
    QuickEntry {
        command: Command::Capture,
        page: "mse-abr",
        row: None,
        expected: &[capture("(control) mse-abr"), capture("mse-abr")],
        start: StartOrder::Normal,
    },
    // 4. Реклама → фильм в том же элементе (как YouTube).
    QuickEntry {
        command: Command::Capture,
        page: "ad-film-same-element",
        row: Some(RowFilter::ad_hop("=ad-film-same-element")),
        expected: &[capture("ad-film-same-element")],
        start: StartOrder::Normal,
    },
    // 5. Реклама отдельным роликом → фильм (сериальные сайты).
    QuickEntry {
        command: Command::Capture,
        page: "ad-preroll",
        row: Some(RowFilter::ad_hop("=ad-preroll")),
        expected: &[capture("ad-preroll")],
        start: StartOrder::Normal,
    },
    // 6. Пауза, play, перемотка в буфере и за буфер, закрытие окна.
    QuickEntry {
        command: Command::PlayerCommands,
        page: "mse-fmp4",
        row: None,
        expected: &[ExpectedRow {
            command: "player-commands",
            subject: "mse-fmp4",
        }],
        start: StartOrder::Normal,
    },
    // 7. Файл `<video src>`.
    QuickEntry {
        command: Command::Capture,
        page: "video-src",
        row: None,
        expected: &[capture("video-src")],
        start: StartOrder::Normal,
    },
    // 8. Нативный HLS в `src` (в Firefox — честное unsupported: своего HLS у него нет).
    QuickEntry {
        command: Command::Capture,
        page: "m3u8-in-src",
        row: None,
        expected: &[capture("m3u8-in-src")],
        start: StartOrder::Normal,
    },
    // 9. Следующая серия на той же странице.
    QuickEntry {
        command: Command::Episodes,
        page: "episode-gap",
        row: Some(RowFilter::episodes("=delayed-next")),
        expected: &[ExpectedRow {
            command: "episodes",
            subject: "delayed-next",
        }],
        start: StartOrder::Early,
    },
    // 10. Следующая серия встаёт в запомненной озвучке.
    QuickEntry {
        command: Command::SeriesMemory,
        page: "series-variants",
        row: Some(RowFilter::series_memory("=series-memory-autoplay")),
        expected: &[ExpectedRow {
            command: "series-memory-autoplay",
            subject: "series-variants",
        }],
        start: StartOrder::Early,
    },
    // 11. Перезагрузка страницы → то же окно.
    QuickEntry {
        command: Command::PageReload,
        page: "page-reload",
        row: Some(RowFilter::page_reload("=reload-same-url")),
        expected: &[ExpectedRow {
            command: "page-reload",
            subject: "reload-same-url",
        }],
        start: StartOrder::Normal,
    },
    // 12. Закрыл вкладку → «Return» вернул её в тот же сеанс.
    QuickEntry {
        command: Command::TabReturn,
        page: "tab-return",
        row: Some(RowFilter::tab_return("return-mse")),
        expected: &[ExpectedRow {
            command: "tab-return",
            subject: "return-mse x tab-return",
        }],
        start: StartOrder::Normal,
    },
    // 13. Режим 2: клик по видео — и оно ушло в плеер.
    QuickEntry {
        command: Command::AutoHandoff,
        page: "auto-film",
        row: Some(RowFilter::auto_handoff("click-hands-off")),
        expected: &[ExpectedRow {
            command: "auto-handoff",
            subject: "click-hands-off x auto-film",
        }],
        start: StartOrder::Normal,
    },
    // 14. Лента из многих `<video>` (Instagram).
    QuickEntry {
        command: Command::Capture,
        page: "feed",
        row: Some(RowFilter::feed("feed-many")),
        expected: &[capture("feed-many")],
        start: StartOrder::Normal,
    },
    // 15. Лента из пула двух элементов (TikTok).
    QuickEntry {
        command: Command::Capture,
        page: "feed",
        row: Some(RowFilter::feed("feed-pool")),
        expected: &[capture("feed-pool")],
        start: StartOrder::Normal,
    },
    // 16. Лента одного переносимого элемента (Shorts).
    QuickEntry {
        command: Command::Capture,
        page: "feed",
        row: Some(RowFilter::feed("feed-moved")),
        expected: &[capture("feed-moved")],
        start: StartOrder::Normal,
    },
];

/// Части быстрого набора: каждая строка списка в каждом выбранном браузере. Сначала все
/// строки одного браузера, потом другого — как в таблице обычного прогона.
pub fn quick_parts(browsers: &[BrowserKind]) -> Vec<RunPart> {
    browsers
        .iter()
        .flat_map(|&browser| {
            QUICK_SUITE.iter().map(move |entry| RunPart {
                command: entry.command,
                browser,
                page: Some(entry.page),
                row: entry.row,
                expected: Some(entry.expected),
                start: entry.start,
            })
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn every_page_belongs_to_its_command() {
        for entry in QUICK_SUITE {
            let pages = entry.command.page_ids().expect("a command with pages");
            assert!(
                pages.contains(&entry.page),
                "{} is not a page of {}",
                entry.page,
                entry.command.cli_name()
            );
        }
    }

    #[test]
    fn every_row_filter_selects_exactly_its_expected_row() {
        for entry in QUICK_SUITE {
            let Some(row) = entry.row else {
                continue;
            };
            let matching = row.matching_rows();
            assert_eq!(
                matching.len(),
                1,
                "{:?} selects {matching:?}",
                row.env_pair()
            );
            // Имя строки в каталоге — её SUBJECT, а у памяти выбора — колонка COMMAND.
            let [expected] = entry.expected else {
                panic!("a filtered entry expects one row: {:?}", entry.expected);
            };
            assert!(
                matching[0] == expected.subject || matching[0] == expected.command,
                "{:?} selects {matching:?}, the entry expects {expected:?}",
                row.env_pair()
            );
        }
    }

    #[test]
    fn the_suite_has_the_sixteen_approved_rows_in_each_browser() {
        let parts = quick_parts(&[BrowserKind::Chromium, BrowserKind::Firefox]);
        assert_eq!(parts.len(), 32);
        assert!(
            parts[..16]
                .iter()
                .all(|part| part.browser == BrowserKind::Chromium)
        );
        assert!(
            parts[16..]
                .iter()
                .all(|part| part.browser == BrowserKind::Firefox)
        );
        let feed_rows = parts[..16]
            .iter()
            .filter(|part| part.page == Some("feed"))
            .count();
        assert_eq!(feed_rows, 3, "one feed row per feed device");
    }
}
