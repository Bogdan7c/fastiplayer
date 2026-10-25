//! Единое определение «аудио-часы стоят» для сессии.
//!
//! Раньше условие жило прямо в планировщике показа; теперь им пользуются и планировщик
//! (повтор кадра при остановке звука), и детектор отставания декодера (остановка
//! аудио-устройства — не вина декодера). Владелец данных — pipeline (позиция и момент
//! последнего движения часов), а порог берётся из [`PlayerTickConfig`].

use std::time::{Duration, Instant};

use super::{PlayerSession, tick::PlayerTickConfig};

/// Состояние аудио-часов относительно порога остановки.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum AudioClockStall {
    /// Часы идут (или звук ещё слишком рано, чтобы считать остановку реальной).
    Running,

    /// Часы не двигались дольше `audio_stall_timeout` после `audio_stall_min_position`.
    Stalled {
        /// Сколько часы стоят.
        stalled_for: Duration,
    },
}

impl PlayerSession {
    /// Классифицирует аудио-часы на момент `now`.
    ///
    /// Не пишет в pipeline: сэмпл часов (`note_audio_clock_sample`) обновляет планировщик.
    #[must_use]
    pub(crate) fn audio_clock_stall(
        &self,
        now: Instant,
        tick_config: &PlayerTickConfig,
    ) -> AudioClockStall {
        let stalled_for = self.pipeline.audio_clock_stalled_for(now);
        let is_stalled = self.audio_clock_now() >= tick_config.audio_stall_min_position
            && stalled_for >= tick_config.audio_stall_timeout;
        if is_stalled {
            AudioClockStall::Stalled { stalled_for }
        } else {
            AudioClockStall::Running
        }
    }
}
