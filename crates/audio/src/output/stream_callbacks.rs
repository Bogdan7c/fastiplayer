//! Общее состояние между владельцем `AudioOutput` и callback-ами CPAL stream-а.
//!
//! CPAL вызывает два callback-а в своём потоке: data callback (забрать PCM) и error callback
//! (поток сломался). Владелец output-а живёт в playback worker-е. Здесь собрано всё, чем они
//! делятся, чтобы сигнатуры построения stream-а не росли с каждым новым флагом.

use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};

use audio_core::AudioOutputStreamHealth;
use ringbuf::HeapCons;
use tracing::warn;

use crate::clock::AudioClock;

/// Состояние, которое CPAL callback-и разделяют с владельцем `AudioOutput`.
pub(super) struct StreamCallbackShared {
    /// Consumer ring buffer-а; владелец берёт его только для явной очистки при seek.
    pub(super) consumer: Arc<Mutex<HeapCons<f32>>>,

    /// Общий clock для A/V sync.
    pub(super) clock: Arc<AudioClock>,

    /// Количество каналов output stream-а.
    pub(super) channels: usize,

    /// Флаг «backend сообщил об ошибке потока».
    pub(super) stream_failure: StreamFailureSignal,
}

/// Lock-free сигнал «поток вывода сломан» от error callback-а к владельцу output-а.
///
/// Почему только флаг, а не пересоздание прямо в callback-е:
/// - error callback вызывается в потоке backend-а; блокировать его или строить там новый
///   stream нельзя (на ALSA это тот же поток, что кормит устройство);
/// - решение «что делать» (переключиться на устройство по умолчанию, сохранить позицию,
///   уведомить пользователя) принадлежит player-core, который опрашивает здоровье output-а.
///
/// Почему любая ошибка считается поломкой: в CPAL 0.15 на ALSA xrun (`EPIPE`) обрабатывается
/// внутри backend-а и в error callback не попадает; выдернутое устройство приходит как
/// `BackendSpecific` (POLLERR/ENODEV), а не `DeviceNotAvailable`. Значит всё, что дошло до
/// callback-а, — настоящая поломка потока.
#[derive(Debug, Clone, Default)]
pub(crate) struct StreamFailureSignal {
    /// `true` после первой ошибки; обратно не сбрасывается (восстановление = новый output).
    failed: Arc<AtomicBool>,
}

impl StreamFailureSignal {
    /// Отмечает ошибку потока; вызывается из error callback-а backend-а.
    ///
    /// Пишет в лог только первую ошибку: после выдёргивания устройства цикл ALSA-backend-а
    /// продолжает крутиться и может вызывать callback тысячи раз в секунду — лог не должен
    /// превращаться в шторм.
    pub(crate) fn record_failure(&self, error: &dyn std::fmt::Display) {
        let already_failed = self.failed.swap(true, Ordering::AcqRel);
        if !already_failed {
            warn!(error = %error, "Поток вывода звука сломан; player пересоздаст output");
        }
    }

    /// Возвращает здоровье потока для нейтрального output-контракта.
    #[must_use]
    pub(crate) fn health(&self) -> AudioOutputStreamHealth {
        if self.failed.load(Ordering::Acquire) {
            AudioOutputStreamHealth::Failed
        } else {
            AudioOutputStreamHealth::Running
        }
    }
}

#[cfg(test)]
mod tests {
    use std::thread;

    use super::*;

    #[test]
    fn fresh_signal_reports_running_stream() {
        let signal = StreamFailureSignal::default();

        assert_eq!(signal.health(), AudioOutputStreamHealth::Running);
    }

    #[test]
    fn error_from_backend_thread_is_visible_to_output_owner() {
        let owner_signal = StreamFailureSignal::default();
        let callback_signal = owner_signal.clone();

        // Error callback CPAL выполняется в другом потоке — проверяем именно этот путь.
        thread::spawn(move || callback_signal.record_failure(&"device unplugged"))
            .join()
            .expect("поток error callback-а не должен паниковать");

        assert_eq!(owner_signal.health(), AudioOutputStreamHealth::Failed);
    }

    #[test]
    fn repeated_errors_keep_stream_failed_without_reset() {
        let signal = StreamFailureSignal::default();

        for _ in 0..1_000 {
            signal.record_failure(&"POLLERR");
        }

        assert_eq!(signal.health(), AudioOutputStreamHealth::Failed);
    }
}
