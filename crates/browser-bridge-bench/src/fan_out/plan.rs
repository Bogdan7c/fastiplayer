//! Части прогона: команда × браузер × страница (или строка) — то, что идёт одним дочерним
//! процессом стенда.
//!
//! Строка стенда и раньше жила в своей песочнице (временный HOME, частная шина, свой
//! невидимый KWin), но сервер стенда один на процесс, и судьи берут его улики (запросы
//! медиа, отказы CDN) по времени строки. Поэтому делим по процессам, а не по потокам: у
//! каждой части свой сервер, и улики соседей в неё не попадают.

use browser_bridge_bench::browsers::BrowserKind;
use browser_bridge_bench::scenarios::{PageFilter, RowFilter};

use crate::cli::Command;

/// Строка таблицы, которую часть обязана выдать (быстрый набор: отбор строки не должен
/// молча выбрать пусто или лишнее).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ExpectedRow {
    /// Колонка COMMAND таблицы (`capture`, `series-memory-autoplay`).
    pub command: &'static str,
    /// Колонка SUBJECT таблицы.
    pub subject: &'static str,
}

/// Когда часть стартует относительно соседей. Таблица всегда идёт в порядке частей, а
/// запуск — долгие первыми: иначе долгая часть, взятая последней, одна тянет хвост прогона.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub enum StartOrder {
    /// Долгая часть (минута и больше реального времени сценария): в начало очереди.
    Early,
    /// Обычная часть.
    Normal,
}

/// Одна часть прогона.
#[derive(Debug, Clone, Copy)]
pub struct RunPart {
    /// Команда (никогда не `all` и не `quick`).
    pub command: Command,
    /// Браузер.
    pub browser: BrowserKind,
    /// Страница команды; `None` — команда без списка страниц идёт целиком.
    pub page: Option<&'static str>,
    /// Отбор строки внутри страницы (быстрый набор).
    pub row: Option<RowFilter>,
    /// Какие строки часть обязана выдать; `None` — сколько выйдет (обычный прогон).
    pub expected: Option<&'static [ExpectedRow]>,
    /// Когда стартовать относительно соседей.
    pub start: StartOrder,
}

impl RunPart {
    /// Короткое имя части для прогресса и имён файлов: `capture chromium mse-fmp4 feed-pool`.
    pub fn label(&self) -> String {
        let mut label = format!(
            "{} {}",
            self.command.cli_name(),
            self.browser.catalog_name()
        );
        if let Some(page) = self.page {
            label.push(' ');
            label.push_str(page);
        }
        if let Some(row) = self.row {
            label.push(' ');
            label.push_str(row.env_pair().1);
        }
        label
    }
}

/// Части обычного прогона: каждая страница команды — своя часть, команда без страниц —
/// одна часть. Порядок — порядок шагов, внутри шага — браузеры, внутри браузера — страницы.
pub fn plan_parts(steps: &[Command], browsers: &[BrowserKind], pages: &PageFilter) -> Vec<RunPart> {
    let mut parts = Vec::new();
    for &command in steps {
        for &browser in browsers {
            let part = |page: Option<&'static str>| RunPart {
                command,
                browser,
                page,
                row: None,
                expected: None,
                start: StartOrder::Normal,
            };
            match command.page_ids() {
                Some(ids) => parts.extend(
                    ids.into_iter()
                        .filter(|id| pages.admits(id))
                        .map(|id| part(Some(id))),
                ),
                None => parts.push(part(None)),
            }
        }
    }
    parts
}

#[cfg(test)]
mod tests {
    use super::*;

    const BOTH: [BrowserKind; 2] = [BrowserKind::Chromium, BrowserKind::Firefox];

    #[test]
    fn every_page_of_a_command_is_its_own_part_in_both_browsers() {
        let pages = Command::Episodes.page_ids().expect("episodes have pages");
        let parts = plan_parts(&[Command::Episodes], &BOTH, &PageFilter::all());
        assert_eq!(parts.len(), pages.len() * 2);
        assert!(parts.iter().all(|part| part.page.is_some()));
        assert_eq!(parts[0].browser, BrowserKind::Chromium);
        assert_eq!(parts[pages.len()].browser, BrowserKind::Firefox);
    }

    #[test]
    fn a_command_without_pages_is_one_part_per_browser() {
        let parts = plan_parts(&[Command::HlsLive], &BOTH, &PageFilter::all());
        assert_eq!(parts.len(), 2);
        assert!(parts.iter().all(|part| part.page.is_none()));
    }

    #[test]
    fn the_page_filter_narrows_the_parts() {
        let only = PageFilter::only(["mse-fmp4".to_owned()]);
        let parts = plan_parts(
            &[Command::Capture, Command::PlayerCommands],
            &[BrowserKind::Chromium],
            &only,
        );
        let labels: Vec<String> = parts.iter().map(RunPart::label).collect();
        assert_eq!(
            labels,
            [
                "capture chromium mse-fmp4",
                "player-commands chromium mse-fmp4"
            ]
        );
    }
}
