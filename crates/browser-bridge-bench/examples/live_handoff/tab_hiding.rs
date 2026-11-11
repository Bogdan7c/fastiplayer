//! Скрыть вкладку страницы после передачи (`--hide-tab-after <с>`, 16a).
//!
//! Замер решения 303: сколько проходит от конца серии до следующей, когда вкладка давно скрыта и
//! браузер притормаживает её таймеры. Помощник открывает пустую вкладку и делает её активной: вкладка
//! страницы становится скрытой (`document.hidden`), как у человека, который ушёл на другую вкладку.
//! Служебную страницу экстеншена в таком прогоне не звать: она возвращает вкладку страницы вперёд.

use std::time::{Duration, Instant};

use browser_bridge_bench::browsers::BrowserUnderTest;

/// Что открыть поверх страницы: пустая вкладка не качает и не играет, своего шума в замер не вносит.
const COVER_TAB_URL: &str = "about:blank";

/// Когда скрыть вкладку страницы; один раз за прогон.
pub(crate) struct TabHiding {
    /// Момент скрытия; `None` — уже скрыта или скрывать не просили.
    hide_at: Option<Instant>,
}

impl TabHiding {
    /// План от момента передачи; `None` в `after` — вкладку не скрывать.
    pub(crate) fn planned(handed_off_at: Instant, after: Option<Duration>) -> Self {
        Self {
            hide_at: after.map(|delay| handed_off_at + delay),
        }
    }

    /// Пора — открыть пустую вкладку и сделать её активной. Сбой печатается, прогон не рвётся:
    /// это помощник человека, без скрытия замер просто не годится, и это видно в выводе.
    pub(crate) fn poll(&mut self, browser: &mut dyn BrowserUnderTest, now: Instant) {
        let Some(at) = self.hide_at else {
            return;
        };
        if now < at {
            return;
        }
        // Скрываем один раз: повтор открыл бы новые пустые вкладки на каждом шаге цикла.
        self.hide_at = None;
        let cover = match browser.open_tab(COVER_TAB_URL) {
            Ok(cover) => cover,
            Err(error) => {
                println!("tab hiding: the cover tab did not open: {error}");
                return;
            }
        };
        match browser.activate_tab(&cover) {
            Ok(()) => println!("tab hiding: the page tab is hidden behind an empty tab"),
            Err(error) => println!("tab hiding: activating the cover tab failed: {error}"),
        }
    }
}

/// Секунды после передачи для `--<имя> <с>` в длительность (общая форма опций «через столько»).
pub(crate) fn parse_after_seconds(name: &str, value: &str) -> Result<Duration, String> {
    value
        .parse::<u64>()
        .map(Duration::from_secs)
        .map_err(|error| format!("{name}: {error}"))
}
