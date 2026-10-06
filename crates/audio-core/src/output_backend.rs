//! Нейтральные runtime-контракты backend-а вывода: фабрика, устройство, здоровье потока, clock.
//!
//! Здесь живёт фабрика audio output-а, playback clock и всё, что player должен знать об устройстве,
//! не зная CPAL/ALSA/PipeWire:
//!
//! - **какое устройство просим** ([`AudioOutputDeviceRequest`]): только выбранное в настройках,
//!   выбранное с запасным вариантом «по умолчанию» или сразу системное по умолчанию;
//! - **какое устройство реально открыто** ([`AudioOutputDeviceRoute`]): чтобы player мог
//!   сообщить пользователю «устройство X недоступно, звук на устройстве по умолчанию»;
//! - **жив ли поток** ([`AudioOutputStreamHealth`]): backend сообщает, что устройство пропало,
//!   а решение о реакции принимает player-core, а не audio thread.
//!
//! Выбор пользователя (stable id в настройках) этот контракт никогда не меняет: запасной
//! маршрут действует только для одного созданного output-а.

use std::time::Duration;

use anyhow::Result;

use crate::{AudioOutputClockTiming, AudioOutputSpec, PlayerAudioOutput};

/// Какое устройство вывода player просит у фабрики.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum AudioOutputDeviceRequest {
    /// Только устройство, выбранное в настройках; ошибка не подменяется другим устройством.
    ///
    /// Нужен там, где пользователь только что явно выбрал устройство: молча играть в другое
    /// место было бы обманом, ошибка должна дойти до слоя настроек.
    SelectedOnly,

    /// Выбранное устройство, а если его открыть не удалось — системное по умолчанию.
    ///
    /// Обычное открытие media: пропавшие наушники не должны выключать звук целиком.
    SelectedOrSystemDefault,

    /// Сразу системное устройство по умолчанию, не пытаясь открыть выбранное.
    ///
    /// Восстановление после смерти потока: выбранное устройство только что сломалось,
    /// повторное открытие того же устройства привело бы к шторму ошибок.
    SystemDefault,
}

/// Какое устройство фабрика реально открыла для созданного output-а.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum AudioOutputDeviceRoute {
    /// Открыто устройство, выбранное в настройках (это может быть и «по умолчанию»).
    SelectedDevice,

    /// Player сам попросил системное устройство по умолчанию.
    SystemDefault,

    /// Выбранное устройство открыть не удалось, вместо него открыто устройство по умолчанию.
    SystemDefaultInsteadOfUnavailable {
        /// Человекочитаемое имя недоступного устройства (как в списке настроек).
        unavailable_device_name: String,
    },
}

/// Созданный output вместе с фактическим маршрутом устройства.
pub struct CreatedAudioOutput {
    /// Готовый output, ещё не запущенный (`play` вызывает player).
    pub output: Box<dyn PlayerAudioOutput>,

    /// Какое устройство реально стоит за output-ом.
    pub route: AudioOutputDeviceRoute,
}

impl CreatedAudioOutput {
    /// Связывает output с маршрутом устройства.
    #[must_use]
    pub fn new(output: Box<dyn PlayerAudioOutput>, route: AudioOutputDeviceRoute) -> Self {
        Self { output, route }
    }
}

impl std::fmt::Debug for CreatedAudioOutput {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        // Trait object output-а не реализует Debug; маршрута достаточно для диагностики.
        formatter
            .debug_struct("CreatedAudioOutput")
            .field("route", &self.route)
            .finish_non_exhaustive()
    }
}

/// Состояние backend stream-а, которое output сообщает player-у.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum AudioOutputStreamHealth {
    /// Backend не сообщал об ошибках потока.
    Running,

    /// Backend сообщил об ошибке потока (например, устройство отключено).
    ///
    /// Состояние необратимо для этого output-а: восстановление — это новый output.
    Failed,
}

/// Нейтральная фабрика audio output-а без знания о CPAL или concrete backend-е.
pub trait AudioOutputFactory: Send + Sync {
    /// Создаёт output под decoded audio spec на устройстве согласно `device_request`.
    ///
    /// Возвращает фактический маршрут устройства, чтобы player мог уведомить пользователя
    /// о запасном варианте. Ошибка означает, что ни одно разрешённое запросом устройство
    /// не открылось.
    fn create_output(
        &self,
        spec: AudioOutputSpec,
        device_request: AudioOutputDeviceRequest,
    ) -> Result<CreatedAudioOutput>;
}

/// Нейтральный playback clock для A/V sync и EOF-drain diagnostics.
pub trait PlayerAudioClock: Send + Sync {
    /// Возвращает текущую playback позицию относительно clock base.
    fn now(&self) -> Duration;

    /// Возвращает audible clock и конец всего уже принятого output PCM.
    fn output_timing(&self) -> AudioOutputClockTiming;

    /// Сбрасывает clock state после seek/output clear.
    fn reset(&self);

    /// Возвращает количество output callbacks, где stream недополучил samples.
    fn underrun_callbacks(&self) -> u64;
}
