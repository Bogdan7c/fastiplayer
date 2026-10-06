//! Восстановление звука после пропажи устройства вывода (UX edge cases, сессия 10).
//!
//! Решения владельца:
//! - поток вывода сломался (выдернули USB-гарнитуру) → звук автоматически переходит на
//!   системное устройство по умолчанию, позиция сохраняется, пользователь получает
//!   уведомление; выбор в настройках не меняется;
//! - обратно на вернувшееся устройство автоматически не переключаемся (бэклог);
//! - если восстановить звук не удалось — видео продолжает играть без звука с понятным
//!   уведомлением, следующее открытие media снова попробует выбранное устройство.
//!
//! Владение: concrete output (crate `audio`) только сообщает факт поломки через
//! `PlayerAudioOutput::stream_health`; решение и lifecycle (новый output, clock, события)
//! принадлежат session. Пересоздание переиспользует `recreate_active_audio_output`.
//!
//! Защита от шторма: если и устройство по умолчанию ломается или не открывается, попытки
//! ограничены по числу и по интервалу — иначе player пересоздавал бы output каждый tick.

use std::time::{Duration, Instant};

use tracing::{info, warn};

use crate::{
    AudioOutputDeviceRequest, AudioOutputDeviceRoute, AudioOutputStreamHealth,
    AudioOutputSwitchReason, PlayerError, PlayerErrorKind, PlayerEvent,
    PlayerRuntimeAcceptedChange,
};

use super::PlayerSession;

/// Сколько раз подряд можно пересоздать сломанный output для одного созданного звука.
///
/// Одной попытки обычно достаточно (выдернули устройство → default работает). Запас на случай,
/// когда звуковой сервер сам перезапускается и default на мгновение недоступен.
const MAX_AUDIO_OUTPUT_RECOVERY_ATTEMPTS: u32 = 3;

/// Минимальный интервал между попытками пересоздания.
///
/// Не даёт превратить сломанное устройство в цикл «создать → сломалось → создать» на
/// каждом tick-е worker-а (это сотни попыток в секунду).
const MIN_AUDIO_OUTPUT_RECOVERY_INTERVAL: Duration = Duration::from_secs(1);

/// Счётчик попыток восстановления для текущего звукового output-а.
///
/// Сбрасывается, когда session создаёт output заново для media/track
/// (`ensure_audio_output_for_decoded_spec`), но не при автоматическом восстановлении —
/// иначе ограничение никогда бы не сработало.
#[derive(Debug, Default)]
pub(super) struct AudioOutputRecoveryBudget {
    /// Сколько попыток пересоздания уже сделано.
    attempts_made: u32,

    /// Когда была последняя попытка; `None` — попыток ещё не было.
    last_attempt_at: Option<Instant>,
}

/// Можно ли сейчас делать очередную попытку.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum AudioOutputRecoveryAdmission {
    /// Попытка разрешена.
    Allowed,
    /// Прошлая попытка была слишком недавно — ждём следующий tick.
    WaitForRetryInterval,
    /// Попытки исчерпаны — дальше видео без звука.
    Exhausted,
}

impl AudioOutputRecoveryBudget {
    /// Решает, можно ли пробовать сейчас, не меняя состояние.
    fn admission(&self, now: Instant) -> AudioOutputRecoveryAdmission {
        if self.attempts_made >= MAX_AUDIO_OUTPUT_RECOVERY_ATTEMPTS {
            return AudioOutputRecoveryAdmission::Exhausted;
        }
        match self.last_attempt_at {
            Some(last_attempt_at)
                if now.saturating_duration_since(last_attempt_at)
                    < MIN_AUDIO_OUTPUT_RECOVERY_INTERVAL =>
            {
                AudioOutputRecoveryAdmission::WaitForRetryInterval
            }
            _ => AudioOutputRecoveryAdmission::Allowed,
        }
    }

    /// Учитывает начатую попытку.
    fn record_attempt(&mut self, now: Instant) {
        self.attempts_made = self.attempts_made.saturating_add(1);
        self.last_attempt_at = Some(now);
    }
}

/// Итог одной проверки здоровья звука на tick-е (для тестов и телеметрии).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) enum AudioOutputRecoveryOutcome {
    /// Output-а нет — восстанавливать нечего.
    NoActiveOutput,
    /// Поток жив.
    StreamRunning,
    /// Новый output на устройстве по умолчанию установлен.
    SwitchedToSystemDefault,
    /// Попытка не удалась или слишком рано; повторим на следующих tick-ах.
    RetryPending,
    /// Попытки исчерпаны: звук отключён, видео продолжает играть.
    AudioDisabled,
}

impl PlayerSession {
    /// Проверяет здоровье output stream-а и восстанавливает звук, если поток сломан.
    ///
    /// Вызывается на каждом tick-е; при живом потоке это одно атомарное чтение.
    pub(super) fn recover_failed_audio_output_if_needed(
        &mut self,
        now: Instant,
    ) -> AudioOutputRecoveryOutcome {
        match self.pipeline.audio_output_stream_health() {
            None => return AudioOutputRecoveryOutcome::NoActiveOutput,
            Some(AudioOutputStreamHealth::Running) => {
                return AudioOutputRecoveryOutcome::StreamRunning;
            }
            Some(AudioOutputStreamHealth::Failed) => {}
        }

        match self.audio_output_recovery.admission(now) {
            AudioOutputRecoveryAdmission::Allowed => {}
            AudioOutputRecoveryAdmission::WaitForRetryInterval => {
                return AudioOutputRecoveryOutcome::RetryPending;
            }
            AudioOutputRecoveryAdmission::Exhausted => {
                return self.disable_audio_after_failed_recovery(now);
            }
        }

        self.audio_output_recovery.record_attempt(now);
        match self.recreate_active_audio_output(AudioOutputDeviceRequest::SystemDefault) {
            Ok(PlayerRuntimeAcceptedChange::Applied) => {
                info!("Поток вывода звука восстановлен на устройстве по умолчанию");
                self.push_player_event(PlayerEvent::AudioOutputSwitchedToSystemDefault(
                    AudioOutputSwitchReason::ActiveDeviceStopped,
                ));
                AudioOutputRecoveryOutcome::SwitchedToSystemDefault
            }
            // Output есть, но без input spec восстановить нечего: тот же исход, что без output-а.
            Ok(PlayerRuntimeAcceptedChange::Unchanged) => {
                AudioOutputRecoveryOutcome::NoActiveOutput
            }
            Err(error) => {
                warn!(
                    error = %error,
                    "Не удалось пересоздать вывод звука на устройстве по умолчанию"
                );
                if self.audio_output_recovery.admission(now)
                    == AudioOutputRecoveryAdmission::Exhausted
                {
                    self.disable_audio_after_failed_recovery(now)
                } else {
                    AudioOutputRecoveryOutcome::RetryPending
                }
            }
        }
    }

    /// Начинает новый счётчик попыток для только что созданного output-а.
    pub(super) fn begin_audio_output_recovery_budget(&mut self) {
        self.audio_output_recovery = AudioOutputRecoveryBudget::default();
    }

    /// Публикует уведомление, если factory открыла default вместо выбранного устройства.
    pub(super) fn publish_audio_output_route(&mut self, route: AudioOutputDeviceRoute) {
        match route {
            AudioOutputDeviceRoute::SelectedDevice | AudioOutputDeviceRoute::SystemDefault => {}
            AudioOutputDeviceRoute::SystemDefaultInsteadOfUnavailable {
                unavailable_device_name,
            } => {
                self.push_player_event(PlayerEvent::AudioOutputSwitchedToSystemDefault(
                    AudioOutputSwitchReason::SelectedDeviceUnavailable {
                        device_name: unavailable_device_name,
                    },
                ));
            }
        }
    }

    /// Отключает звук после исчерпанных попыток, чтобы видео не стояло на мёртвом clock-е.
    ///
    /// Сломанный output больше не двигает audio clock; без него presentation clock
    /// переходит на существующий monotonic fallback с текущей позиции — видео продолжает
    /// играть с того же места.
    fn disable_audio_after_failed_recovery(&mut self, now: Instant) -> AudioOutputRecoveryOutcome {
        warn!("Звук отключён: устройство вывода не удалось открыть заново");
        self.disable_selected_audio_path();
        self.reanchor_no_audio_clock(self.current_source_position, now);
        self.record_recoverable_error(PlayerError::new(
            PlayerErrorKind::AudioDeviceUnavailable,
            "Audio output stream failed and no output device could be reopened",
        ));
        AudioOutputRecoveryOutcome::AudioDisabled
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn fresh_budget_allows_first_attempt() {
        let budget = AudioOutputRecoveryBudget::default();

        assert_eq!(
            budget.admission(Instant::now()),
            AudioOutputRecoveryAdmission::Allowed
        );
    }

    #[test]
    fn attempt_inside_interval_waits_and_after_interval_is_allowed() {
        let started_at = Instant::now();
        let mut budget = AudioOutputRecoveryBudget::default();
        budget.record_attempt(started_at);

        assert_eq!(
            budget.admission(started_at + Duration::from_millis(999)),
            AudioOutputRecoveryAdmission::WaitForRetryInterval
        );
        assert_eq!(
            budget.admission(started_at + MIN_AUDIO_OUTPUT_RECOVERY_INTERVAL),
            AudioOutputRecoveryAdmission::Allowed
        );
    }

    #[test]
    fn budget_is_exhausted_after_max_attempts_regardless_of_time() {
        let started_at = Instant::now();
        let mut budget = AudioOutputRecoveryBudget::default();
        for attempt in 0..MAX_AUDIO_OUTPUT_RECOVERY_ATTEMPTS {
            budget.record_attempt(started_at + Duration::from_secs(u64::from(attempt) * 10));
        }

        assert_eq!(
            budget.admission(started_at + Duration::from_secs(3_600)),
            AudioOutputRecoveryAdmission::Exhausted
        );
    }
}
