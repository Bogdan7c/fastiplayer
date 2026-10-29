//! Пауза обычного плеера при старте браузерного сеанса (решения 41 и 48).
//!
//! Решение владельца сессии 02: пауза идёт через MPRIS `Pause` строго на имя
//! обычного Fastiplayer (`desktop_integration::pause_running_player`), без перебора
//! других плееров — MPRIS браузеров мы не трогаем никогда. Обычный плеер не
//! запущен или уже на паузе — ничего не происходит. Возобновлять его после сеанса
//! не нужно: он остаётся на паузе (решение 48).
//!
//! Звонок по шине может занять до таймаута, поэтому он идёт в отдельном потоке:
//! главный цикл в это время отвечает посреднику, и сеанс не ждёт обычный плеер.

use std::thread::JoinHandle;

use desktop_integration::{PlayerPauseConfig, PlayerPauseOutcome, pause_running_player};
use tracing::{info, warn};

/// Порт «поставить обычный плеер на паузу» (в тестах — подделка со счётчиком).
pub(crate) trait NormalPlayerPause: Send {
    /// Просит паузу, не блокируя вызывающего.
    fn request_pause(&mut self);

    /// Дожидается начатых просьб (каждая ограничена таймаутом звонка).
    fn finish(&mut self);
}

/// Настоящая пауза через MPRIS обычного экземпляра.
#[derive(Debug, Default)]
pub(crate) struct SystemNormalPlayerPause {
    config: PlayerPauseConfig,
    in_flight: Vec<JoinHandle<()>>,
}

impl NormalPlayerPause for SystemNormalPlayerPause {
    fn request_pause(&mut self) {
        let config = self.config;
        let spawned = std::thread::Builder::new()
            .name("browser-session-pause".to_owned())
            .spawn(move || report_pause_outcome(pause_running_player(config)));
        match spawned {
            Ok(handle) => self.in_flight.push(handle),
            Err(spawn_error) => {
                warn!(%spawn_error, "Could not start the thread that pauses the normal player");
            }
        }
    }

    fn finish(&mut self) {
        for handle in self.in_flight.drain(..) {
            if handle.join().is_err() {
                warn!("The thread that paused the normal player panicked");
            }
        }
    }
}

/// Итог паузы только в лог: сеанс от него не зависит.
fn report_pause_outcome(
    outcome: Result<PlayerPauseOutcome, desktop_integration::PlayerPauseError>,
) {
    match outcome {
        Ok(PlayerPauseOutcome::PauseRequested) => {
            info!("Normal player was asked to pause for the browser session");
        }
        Ok(PlayerPauseOutcome::PlayerNotRunning) => {
            info!("Normal player is not running; nothing to pause");
        }
        Err(pause_error) => {
            warn!(%pause_error, "Could not pause the normal player");
        }
    }
}
