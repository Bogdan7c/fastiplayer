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
    "Audio is unavailable: could not open the output device. Video is playing without sound";

/// Формулирует уведомление о переходе звука на устройство по умолчанию.
pub(crate) fn audio_output_switch_message(reason: &AudioOutputSwitchReason) -> String {
    match reason {
        AudioOutputSwitchReason::SelectedDeviceUnavailable { device_name } => {
            format!("Audio device “{device_name}” is unavailable; using the default device")
        }
        AudioOutputSwitchReason::ActiveDeviceStopped => {
            "Audio device disconnected; switched to the default device".to_string()
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
            "Audio device “USB Headset” is unavailable; using the default device"
        );
    }

    #[test]
    fn unplugged_device_message_says_where_sound_went() {
        let message = audio_output_switch_message(&AudioOutputSwitchReason::ActiveDeviceStopped);

        assert!(message.contains("disconnected"), "{message}");
        assert!(message.contains("default device"), "{message}");
    }
}
