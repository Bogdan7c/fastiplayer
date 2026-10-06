//! Уведомления о пропаже звукового устройства (UX edge cases, сессия 10).
//!
//! Проверяется путь «событие player-а → плашка с человеческим текстом», который видит
//! пользователь: без имён Rust-типов и технических сообщений.

use std::time::Instant;

use player_core::{AudioOutputSwitchReason, PlayerError, PlayerErrorKind, PlayerEvent};

use super::*;

/// Видимые toast-ы на момент `now`: (вид, текст), самый новый первым.
fn visible_toasts(center: &mut NotificationCenter, now: Instant) -> Vec<(ToastKind, String)> {
    center
        .frame(OpenProgress::Idle, UiMotion::Standard, now)
        .toasts
        .iter()
        .map(|toast| (toast.kind, toast.message.to_string()))
        .collect()
}

#[test]
fn unplugged_device_becomes_info_toast_about_default_device() {
    let now = Instant::now();
    let mut center = NotificationCenter::default();

    center.record_player_event(
        &PlayerEvent::AudioOutputSwitchedToSystemDefault(
            AudioOutputSwitchReason::ActiveDeviceStopped,
        ),
        now,
    );

    assert_eq!(
        visible_toasts(&mut center, now),
        vec![(
            ToastKind::Info,
            "Звуковое устройство отключено — звук переключён на устройство по умолчанию"
                .to_string()
        )]
    );
    // Это не ошибка media: центр экрана свободен.
    assert_eq!(
        center
            .frame(OpenProgress::Idle, UiMotion::Standard, now)
            .center,
        None
    );
}

#[test]
fn missing_saved_device_toast_names_device() {
    let now = Instant::now();
    let mut center = NotificationCenter::default();

    center.record_player_event(
        &PlayerEvent::AudioOutputSwitchedToSystemDefault(
            AudioOutputSwitchReason::SelectedDeviceUnavailable {
                device_name: "front:CARD=Headset,DEV=0".to_string(),
            },
        ),
        now,
    );

    let toasts = visible_toasts(&mut center, now);
    assert_eq!(toasts.len(), 1);
    assert!(
        toasts[0]
            .1
            .contains("«front:CARD=Headset,DEV=0» недоступно"),
        "{toasts:?}"
    );
}

#[test]
fn audio_device_error_is_shown_in_plain_russian_without_technical_text() {
    let now = Instant::now();
    let mut center = NotificationCenter::default();

    center.record_player_event(
        &PlayerEvent::RecoverableError(PlayerError::new(
            PlayerErrorKind::AudioDeviceUnavailable,
            "Audio output init failed: no scripted audio device",
        )),
        now,
    );

    let toasts = visible_toasts(&mut center, now);
    assert_eq!(
        toasts,
        vec![(
            ToastKind::Info,
            crate::audio_output_message::AUDIO_OUTPUT_UNAVAILABLE_MESSAGE.to_string()
        )]
    );
    assert!(!toasts[0].1.contains("AudioDeviceUnavailable"));
}

#[test]
fn other_recoverable_errors_keep_previous_transient_behavior() {
    let now = Instant::now();
    let mut center = NotificationCenter::default();

    center.record_player_event(
        &PlayerEvent::RecoverableError(PlayerError::new(
            PlayerErrorKind::SeekUnavailable,
            "Seek невозможен",
        )),
        now,
    );

    let toasts = visible_toasts(&mut center, now);
    assert_eq!(toasts.len(), 1);
    assert_eq!(toasts[0].0, ToastKind::Transient);
    assert!(toasts[0].1.contains("Seek невозможен"));
}
