//! Индикатор ожидания данных поверх видео (UX edge cases, сессия 15).
//!
//! Раньше `PlaybackState::Buffering` был виден только в URL-сайдбаре и телеметрии: при сетевой
//! задержке кадр просто замирал, и было непонятно, завис плеер или ждёт сеть. Теперь владелец
//! уведомлений следит, как долго player ждёт данные, и показывает в центре спиннер.
//!
//! Решения владельца (8 октября 2026):
//! - вид — только спиннер, без текста;
//! - «ждём данные» — `Buffering` (preroll после открытия, нехватка данных посреди просмотра)
//!   и `Seeking` (перемотка, в том числе на паузе); `Opening` не показывается — у подготовки
//!   media свой текст в центре, а `Scrubbing` показывает кадры превью;
//! - задержка появления [`BUFFERING_INDICATOR_APPEAR_DELAY`] — обычный старт локального файла
//!   и быстрая перемотка укладываются в неё, поэтому индикатор не мигает.
//!
//! Инварианты:
//! - вход — только `PlaybackState` из snapshot-а; player-core не меняется и не получает команд;
//! - выход из ожидания прячет индикатор в том же кадре (без задержки исчезновения);
//! - переход `Seeking` → `Buffering` — одно непрерывное ожидание, таймер не перезапускается;
//! - индикатор не рисуется поверх сообщения в центре (ошибка, прогресс открытия) — это
//!   решает [`NotificationCenter::frame`], а не отрисовка.

use std::time::{Duration, Instant};

use player_core::PlaybackState;

use super::NotificationCenter;

#[cfg(test)]
mod tests;

/// Сколько player должен непрерывно ждать данные, прежде чем появится спиннер.
///
/// 500 мс: preroll локального файла (50 мс звука) и перемотка по локальному файлу обычно
/// короче, а сетевую задержку, которую замечает глаз, индикатор уже объясняет.
pub(crate) const BUFFERING_INDICATOR_APPEAR_DELAY: Duration = Duration::from_millis(500);

/// Показывать ли спиннер ожидания в этом кадре.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum BufferingIndicator {
    /// Player не ждёт данные, ждёт меньше задержки или центр занят другим сообщением.
    Hidden,
    /// Player ждёт данные дольше задержки — спиннер в центре.
    Visible,
}

/// Наблюдение за непрерывным ожиданием данных (поле [`NotificationCenter`]).
#[derive(Debug, Default)]
pub(super) struct PlaybackWaitingWatch {
    /// Текущий эпизод ожидания; `None` — player сейчас не ждёт данные.
    episode: Option<WaitingEpisode>,
}

/// Один непрерывный эпизод ожидания.
#[derive(Debug, Clone, Copy)]
struct WaitingEpisode {
    /// Кадр, на котором ожидание впервые замечено.
    started_at: Instant,
    /// Срок появления уже наступил хотя бы в одном кадре этого эпизода (спиннер показан
    /// или скрыт сообщением центра).
    ///
    /// Нужен будильнику: после этого срок появления остаётся в прошлом, и возвращать его
    /// как deadline значило бы будить окно без конца.
    revealed: bool,
}

impl PlaybackWaitingWatch {
    /// Учитывает состояние player-а текущего кадра.
    fn observe(&mut self, playback_state: PlaybackState, now: Instant) {
        if !waits_for_media_data(playback_state) {
            self.episode = None;
            return;
        }
        // Уже идущий эпизод продолжается: Seeking → Buffering не перезапускает таймер.
        self.episode.get_or_insert(WaitingEpisode {
            started_at: now,
            revealed: false,
        });
    }

    /// Решение для кадра в момент `now`; запоминает, что индикатор показан.
    pub(super) fn indicator(&mut self, now: Instant) -> BufferingIndicator {
        let Some(episode) = self.episode.as_mut() else {
            return BufferingIndicator::Hidden;
        };
        if now.saturating_duration_since(episode.started_at) < BUFFERING_INDICATOR_APPEAR_DELAY {
            return BufferingIndicator::Hidden;
        }
        episode.revealed = true;
        BufferingIndicator::Visible
    }

    /// Момент, к которому окно надо перерисовать, чтобы спиннер появился вовремя.
    ///
    /// Во время ожидания кадры обычно и так рисуются непрерывно, но будильник не полагается
    /// на это: без него на паузе (перемотка на паузе — тоже ожидание) спиннер мог бы
    /// появиться только при движении мыши.
    pub(super) fn appear_deadline(&self) -> Option<Instant> {
        self.episode
            .filter(|episode| !episode.revealed)
            .map(|episode| episode.started_at + BUFFERING_INDICATOR_APPEAR_DELAY)
    }
}

/// Ждёт ли player данные в этом состоянии (решение владельца, см. модуль).
///
/// Перечисление полное намеренно: новое состояние player-а не должно молча попасть
/// в одну из групп — компилятор заставит решить, показывать ли для него спиннер.
fn waits_for_media_data(playback_state: PlaybackState) -> bool {
    match playback_state {
        PlaybackState::Buffering | PlaybackState::Seeking => true,
        PlaybackState::Idle
        | PlaybackState::Opening
        | PlaybackState::Paused
        | PlaybackState::Playing
        | PlaybackState::Scrubbing
        | PlaybackState::Draining
        | PlaybackState::Ended
        | PlaybackState::Stopped
        | PlaybackState::Failed => false,
    }
}

impl NotificationCenter {
    /// Учитывает состояние player-а текущего кадра для индикатора ожидания.
    ///
    /// Вызывается каждый кадр до [`NotificationCenter::frame`] того же кадра, поэтому
    /// выход из ожидания прячет спиннер сразу.
    pub(crate) fn observe_playback_waiting(&mut self, playback_state: PlaybackState, now: Instant) {
        self.playback_waiting.observe(playback_state, now);
    }
}
