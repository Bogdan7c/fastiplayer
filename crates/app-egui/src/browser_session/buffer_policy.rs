//! «Текущая политика буфера» браузерного режима: окно буфера для СЛЕДУЮЩЕГО видео.
//!
//! Решения 96/124: ключи `[browser_session] buffer_ahead_s/buffer_behind_s` из окна
//! настроек действуют на следующее поколение (видео со страницы); источник уже идущего
//! потока не перестраивается — поток от страницы нельзя открыть заново.
//!
//! Владелец значения — [`LiveBufferWindow`]. Пишет его главный поток (применение маршрута
//! `browser_session` в `SettingsRuntime` окна, через [`BrowserBufferWindowOwner`]), читает
//! поток приёма в момент создания нового потока ([`PushedStreamFactory`](super::translator::stream_port::PushedStreamFactory)),
//! поэтому значение лежит под замком и отдаётся копией: уже созданный поток своей копии
//! не меняет.

use std::sync::{Arc, Mutex, PoisonError};

use fastiplayer_config::BrowserSessionBufferWindow;
use pushed_media_source::BufferWindow;

use crate::settings_runtime::BrowserBufferWindowOwner;

/// Общий владелец окна буфера для будущих потоков. Клонирование — ещё одна ручка к тому
/// же значению.
#[derive(Debug, Clone)]
pub(crate) struct LiveBufferWindow {
    /// Окно, с которым стартует следующий поток.
    next_stream_window: Arc<Mutex<BufferWindow>>,
}

impl LiveBufferWindow {
    /// Начальное окно — из config-а при старте процесса.
    pub(crate) fn new(initial: BrowserSessionBufferWindow) -> Self {
        Self {
            next_stream_window: Arc::new(Mutex::new(source_window(initial))),
        }
    }

    /// Окно, с которым стартует следующий поток (копия: идущие потоки её не видят).
    pub(crate) fn window_for_next_stream(&self) -> BufferWindow {
        *self
            .next_stream_window
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
    }
}

impl BrowserBufferWindowOwner for LiveBufferWindow {
    fn adopt_buffer_window_for_next_stream(&self, window: BrowserSessionBufferWindow) {
        *self
            .next_stream_window
            .lock()
            .unwrap_or_else(PoisonError::into_inner) = source_window(window);
    }
}

/// Перевод окна из config-а в тип источника.
pub(crate) fn source_window(window: BrowserSessionBufferWindow) -> BufferWindow {
    BufferWindow {
        ahead: window.ahead(),
        behind: window.behind(),
    }
}

#[cfg(test)]
mod tests {
    use std::time::Duration;

    use fastiplayer_config::AppConfig;

    use super::*;

    /// Окно из config-а с заданными секундами.
    fn config_window(ahead_s: u16, behind_s: u16) -> BrowserSessionBufferWindow {
        let mut config = AppConfig::default();
        config.browser_session.buffer_ahead_s = ahead_s;
        config.browser_session.buffer_behind_s = behind_s;
        config.browser_session.buffer_window()
    }

    /// Усыновление нового окна меняет то, с чем стартует следующий поток; ранее
    /// выданная копия (окно идущего потока) остаётся прежней.
    #[test]
    fn adopting_changes_only_the_window_of_the_next_stream() {
        let live = LiveBufferWindow::new(config_window(120, 30));
        let running_stream_window = live.window_for_next_stream();

        live.adopt_buffer_window_for_next_stream(config_window(45, 10));

        assert_eq!(running_stream_window.ahead, Duration::from_secs(120));
        assert_eq!(running_stream_window.behind, Duration::from_secs(30));
        let next = live.window_for_next_stream();
        assert_eq!(next.ahead, Duration::from_secs(45));
        assert_eq!(next.behind, Duration::from_secs(10));
    }

    /// Ручки делят одно значение: усыновил один владелец — видит читатель на другом потоке.
    #[test]
    fn clones_share_the_value_across_threads() {
        let live = LiveBufferWindow::new(config_window(120, 30));
        let writer = live.clone();

        std::thread::spawn(move || {
            writer.adopt_buffer_window_for_next_stream(config_window(60, 5));
        })
        .join()
        .expect("writer thread");

        assert_eq!(live.window_for_next_stream().ahead, Duration::from_secs(60));
    }
}
