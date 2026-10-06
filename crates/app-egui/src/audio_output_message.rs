//! Тексты для пользователя о пропаже звукового устройства (UX edge cases, сессия 10).
//!
//! player-core сообщает типизированный факт: звук переведён на устройство по умолчанию
//! (`PlayerEvent::AudioOutputSwitchedToSystemDefault`) или звук недоступен совсем
//! (`PlayerErrorKind::AudioDeviceUnavailable`). Здесь — только формулировки; жизненный цикл
//! плашек остаётся у `NotificationCenter` (`state/notifications/player_feed.rs`).
//!
//! Решения владельца: звук сам переходит на устройство по умолчанию, пользователь об этом
//! узнаёт; выбор в настройках не меняется, следующее открытие снова пробует выбранное.

use player_core::AudioOutputSwitchReason;

/// Текст, когда звук не удалось вывести ни на одно устройство.
pub(crate) const AUDIO_OUTPUT_UNAVAILABLE_MESSAGE: &str =
    "Звук недоступен: не удалось открыть устройство вывода. Видео воспроизводится без звука";

/// Формулирует уведомление о переходе звука на устройство по умолчанию.
pub(crate) fn audio_output_switch_message(reason: &AudioOutputSwitchReason) -> String {
    match reason {
        AudioOutputSwitchReason::SelectedDeviceUnavailable { device_name } => format!(
            "Звуковое устройство «{device_name}» недоступно — звук идёт на устройство по умолчанию"
        ),
        AudioOutputSwitchReason::ActiveDeviceStopped => {
            "Звуковое устройство отключено — звук переключён на устройство по умолчанию".to_string()
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn missing_saved_device_message_names_the_device() {
        let message =
            audio_output_switch_message(&AudioOutputSwitchReason::SelectedDeviceUnavailable {
                device_name: "USB Headset".to_string(),
            });

        assert_eq!(
            message,
            "Звуковое устройство «USB Headset» недоступно — звук идёт на устройство по умолчанию"
        );
    }

    #[test]
    fn unplugged_device_message_says_where_sound_went() {
        let message = audio_output_switch_message(&AudioOutputSwitchReason::ActiveDeviceStopped);

        assert!(message.contains("отключено"), "{message}");
        assert!(message.contains("устройство по умолчанию"), "{message}");
    }
}
