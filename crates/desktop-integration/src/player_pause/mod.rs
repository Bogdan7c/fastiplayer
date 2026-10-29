//! Пауза обычного экземпляра плеера, когда стартует браузерный режим.
//!
//! # Зачем
//!
//! Если пользователь включил видео в браузерном режиме, а в обычном окне плеера
//! что-то играет, звук наложится. Перед стартом сеанса браузерный режим просит
//! обычный экземпляр встать на паузу через MPRIS (`Player.Pause`).
//!
//! # Решение владельца (29.10.2026): только наше имя
//!
//! Звонок идёт **строго** на `org.mpris.MediaPlayer2.fastiplayer`. Модуль не
//! перечисляет и не трогает другие `org.mpris.MediaPlayer2.*`: у браузеров свой
//! MPRIS (YouTube в соседней вкладке и т.п.), и ставить чужие плееры на паузу мы
//! не вправе. Если владельца имени нет, обычный плеер не запущен
//! ([`PlayerPauseOutcome::PlayerNotRunning`]); автозапуска нет и быть не может:
//! нужен только уже играющий экземпляр. Владелец имени сверяется по uid.
//!
//! На платформах кроме Linux функция возвращает `UnsupportedPlatform`.

#[cfg(target_os = "linux")]
mod linux;

use std::time::Duration;

use thiserror::Error;

/// Настройки паузы.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct PlayerPauseConfig {
    /// Сколько ждать ответа обычного экземпляра на `Pause`.
    pub call_timeout: Duration,
}

impl Default for PlayerPauseConfig {
    /// 2 с: `Pause` лишь кладёт команду в очередь плеера и отвечает сразу; пауза —
    /// вежливость перед стартом сеанса, из-за неё запуск браузерного режима не должен
    /// заметно задерживаться, если обычный плеер завис.
    fn default() -> Self {
        Self {
            call_timeout: Duration::from_secs(2),
        }
    }
}

/// Чем закончилась просьба о паузе.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PlayerPauseOutcome {
    /// Обычный экземпляр принял команду `Pause`.
    PauseRequested,
    /// Обычный экземпляр не запущен (у MPRIS-имени нет владельца); ничего не запускалось.
    PlayerNotRunning,
}

/// Почему просьбу о паузе не удалось выполнить.
#[derive(Debug, Clone, PartialEq, Eq, Error)]
pub enum PlayerPauseError {
    /// Нет сессионной шины.
    #[error("D-Bus session bus not available: {0}")]
    SessionBusUnavailable(String),
    /// MPRIS-имя плеера принадлежит процессу другого пользователя; ничего не отправлено.
    #[error("The MPRIS name of the player belongs to another user")]
    ForeignOwner,
    /// Обычный экземпляр не ответил вовремя.
    #[error("The running player does not respond")]
    NotResponding,
    /// Прочая ошибка транспорта.
    #[error("Pause transport error: {0}")]
    Transport(String),
    /// На этой платформе MPRIS не реализован.
    #[error("Pausing the player is not supported on this platform.")]
    UnsupportedPlatform,
}

/// Просит обычный экземпляр Fastiplayer встать на паузу. Блокирующий вызов.
pub fn pause_running_player(
    config: PlayerPauseConfig,
) -> Result<PlayerPauseOutcome, PlayerPauseError> {
    #[cfg(target_os = "linux")]
    {
        linux::pause_on_session_bus(config)
    }

    #[cfg(not(target_os = "linux"))]
    {
        let _ = config;
        Err(PlayerPauseError::UnsupportedPlatform)
    }
}
