//! Звук браузерного режима: настоящий выход с «счётчиком» поверх.
//!
//! Решение владельца сессии 03b: звук идёт в настоящий выход (cpal, устройство из
//! config-а); журнал при этом знает, сколько звука записано и сколько прозвучало.
//! Для e2e-тестов есть переключатель на тихую карту с часами реального времени
//! ([`super::simulated_audio`]) — звук проверяется, но не звучит.
//!
//! Устройство:
//! - [`MeteredAudioOutputFactory`] оборачивает production-фабрику выхода. Плеер сам
//!   создаёт выход на своём потоке; обёртка лишь считает и запоминает итог открытия.
//!   Если устройство не открылось, ошибка уходит плееру без изменений — работает уже
//!   существующий восстановимый путь (видео идёт по монотонным часам), а журнал
//!   получает `audio_output=unavailable`;
//! - [`AudioMeter`] — ручка главного цикла: читает накопленные показания.
//!
//! Переменная окружения читается один раз на краю процесса и сразу превращается в
//! [`BrowserAudioOutputChoice`]; бизнес-логика окружения не видит.

use std::ffi::OsStr;
use std::sync::{Arc, Mutex, MutexGuard, PoisonError};
use std::time::Duration;

use player_core::{
    AudioOutputClockTiming, AudioOutputDeviceRequest, AudioOutputFactory, AudioOutputSpec,
    AudioOutputStreamHealth, AudioOutputWriteError, AudioOutputWriteIntent, AudioOutputWriteReport,
    CreatedAudioOutput, PlayerAudioClock, PlayerAudioOutput,
};
use tracing::{info, warn};

use super::simulated_audio::SimulatedRealTimeAudioFactory;

/// Переменная окружения ТОЛЬКО для тестов: `simulated` — тихая карта реального времени.
///
/// Пользователю она не нужна и не документируется в справке: e2e-тесты процесса
/// браузерного режима проверяют звук без колонок и без звуковой карты на CI.
pub(crate) const TEST_BROWSER_AUDIO_VARIABLE: &str = "FASTIPLAYER_TEST_BROWSER_AUDIO";

/// Значение переменной, включающее тихую карту.
const SIMULATED_AUDIO_VALUE: &str = "simulated";

/// Куда идёт звук браузерного режима.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum BrowserAudioOutputChoice {
    /// Настоящее устройство (cpal), выбранное пользователем в config-е.
    SystemDevice,
    /// Тихая карта, часы которой идут в реальном времени (только тесты).
    SimulatedRealTime,
}

/// Переменная окружения задана, но значение незнакомо.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
#[error("unknown value of {TEST_BROWSER_AUDIO_VARIABLE}; only `{SIMULATED_AUDIO_VALUE}` is known")]
pub(crate) struct UnknownAudioOverride;

impl BrowserAudioOutputChoice {
    /// Чистый разбор значения тестовой переменной (`None` — переменной нет).
    pub(crate) fn from_test_override(value: Option<&OsStr>) -> Result<Self, UnknownAudioOverride> {
        match value {
            None => Ok(Self::SystemDevice),
            Some(text) if text == SIMULATED_AUDIO_VALUE => Ok(Self::SimulatedRealTime),
            Some(_) => Err(UnknownAudioOverride),
        }
    }

    /// Край процесса: читает переменную окружения один раз при старте и пишет в журнал,
    /// если включена тихая карта. Незнакомое значение не ломает запуск: настоящий звук
    /// и предупреждение (значение в журнал не пишется — это чужой ввод).
    pub(crate) fn from_environment() -> Self {
        let raw_value = std::env::var_os(TEST_BROWSER_AUDIO_VARIABLE);
        match Self::from_test_override(raw_value.as_deref()) {
            Ok(Self::SimulatedRealTime) => {
                info!(
                    variable = TEST_BROWSER_AUDIO_VARIABLE,
                    "Browser session audio goes to a silent simulated sink (test-only override)"
                );
                Self::SimulatedRealTime
            }
            Ok(Self::SystemDevice) => Self::SystemDevice,
            Err(error) => {
                warn!(error = %error, "Ignoring the test audio override");
                Self::SystemDevice
            }
        }
    }

    /// Собирает фабрику выхода со счётчиком и ручку счётчика для главного цикла.
    ///
    /// `device_id` — устройство из config-а (`default` — системное по умолчанию).
    pub(crate) fn build_metered_factory(
        self,
        device_id: &str,
    ) -> (Arc<dyn AudioOutputFactory>, AudioMeter) {
        let (inner, opened_status): (Arc<dyn AudioOutputFactory>, AudioOutputStatus) = match self {
            Self::SystemDevice => (
                Arc::new(audio::CpalAudioOutputFactory::new(
                    audio::AudioOutputDeviceController::new(device_id),
                )),
                AudioOutputStatus::Ok,
            ),
            Self::SimulatedRealTime => (
                Arc::new(SimulatedRealTimeAudioFactory),
                AudioOutputStatus::Simulated,
            ),
        };
        let meter = AudioMeter::default();
        let factory = MeteredAudioOutputFactory {
            inner,
            meter: meter.clone(),
            opened_status,
        };
        (Arc::new(factory), meter)
    }
}

/// Состояние выхода звука для строки журнала `audio_output=…`.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub(crate) enum AudioOutputStatus {
    /// Плеер ещё не открывал выход (нет звуковой дорожки или звук не дошёл).
    #[default]
    NotOpened,
    /// Настоящее устройство открыто.
    Ok,
    /// Устройство не открылось: видео идёт по монотонным часам.
    Unavailable,
    /// Тихая карта тестов.
    Simulated,
}

impl AudioOutputStatus {
    /// Стабильная метка для журнала.
    pub(crate) const fn log_label(self) -> &'static str {
        match self {
            Self::NotOpened => "not_opened",
            Self::Ok => "ok",
            Self::Unavailable => "unavailable",
            Self::Simulated => "simulated",
        }
    }
}

/// Накопительное показание счётчика звука.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub(crate) struct AudioMeterReading {
    /// Сколько звука (по времени исходного PCM) плеер записал в выход.
    pub(crate) written: Duration,
    /// Сколько звука прозвучало по часам выхода.
    pub(crate) played: Duration,
    /// Состояние выхода.
    pub(crate) output: AudioOutputStatus,
}

/// Общее состояние счётчика: пишет поток плеера, читает главный цикл.
#[derive(Default)]
struct MeterState {
    /// Записано в выход, наносекунды исходного PCM (u128 — без переполнения).
    written_nanos: u128,
    /// Прозвучало, накопительно.
    played: Duration,
    /// Состояние выхода.
    output: AudioOutputStatus,
    /// Часы текущего выхода (чтобы главный цикл видел, сколько прозвучало).
    clock: Option<Arc<dyn PlayerAudioClock>>,
    /// Прошлое показание часов; от него считается прирост.
    last_audible_position: Duration,
}

/// Ручка счётчика звука для главного цикла.
#[derive(Clone, Default)]
pub(crate) struct AudioMeter {
    /// Общее состояние с обёрткой выхода.
    state: Arc<Mutex<MeterState>>,
}

impl std::fmt::Debug for AudioMeter {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter.debug_struct("AudioMeter").finish_non_exhaustive()
    }
}

impl AudioMeter {
    /// Снимает показание: опрашивает часы выхода и копит прирост «прозвучало».
    ///
    /// Часы сбрасываются при перемотке и при новом выходе; шаг назад не вычитается,
    /// а становится новой точкой отсчёта. Поэтому прирост между двумя опросами, на
    /// которые пришёлся сброс, теряется — это доли секунды на перемотку, и журнал
    /// честно занижает, а не завышает. Вызывать часто (каждый шаг главного цикла).
    pub(crate) fn reading(&self) -> AudioMeterReading {
        // Часы опрашиваются вне замка счётчика: их замок берёт и поток звука.
        let clock = self.lock().clock.clone();
        let audible_position = clock.map(|clock| clock.output_timing().audible_output_position());
        let mut state = self.lock();
        if let Some(position) = audible_position {
            let previous_position = state.last_audible_position;
            if position > previous_position {
                state.played += position - previous_position;
            }
            state.last_audible_position = position;
        }
        AudioMeterReading {
            written: Duration::from_nanos(u64::try_from(state.written_nanos).unwrap_or(u64::MAX)),
            played: state.played,
            output: state.output,
        }
    }

    /// Замок состояния. Отравление не страшно: внутри только числа, каждое обновление
    /// — одно присваивание, согласованность не рвётся.
    fn lock(&self) -> MutexGuard<'_, MeterState> {
        self.state.lock().unwrap_or_else(PoisonError::into_inner)
    }

    /// Выход открыт: новые часы, отсчёт «прозвучало» от их текущего показания.
    fn note_output_opened(&self, status: AudioOutputStatus, clock: Arc<dyn PlayerAudioClock>) {
        let start_position = clock.output_timing().audible_output_position();
        let mut state = self.lock();
        state.output = status;
        state.clock = Some(clock);
        state.last_audible_position = start_position;
    }

    /// Выход не открылся.
    fn note_output_unavailable(&self) {
        let mut state = self.lock();
        state.output = AudioOutputStatus::Unavailable;
        state.clock = None;
    }

    /// Выход принял `frames` кадров PCM при частоте `sample_rate`.
    fn note_written(&self, frames: usize, sample_rate: u32) {
        if sample_rate == 0 {
            return;
        }
        let nanos = (frames as u128) * 1_000_000_000 / u128::from(sample_rate);
        self.lock().written_nanos += nanos;
    }
}

/// Фабрика выхода со счётчиком поверх production-фабрики.
pub(crate) struct MeteredAudioOutputFactory {
    /// Настоящая фабрика (cpal) или тихая карта.
    inner: Arc<dyn AudioOutputFactory>,
    /// Счётчик.
    meter: AudioMeter,
    /// Какое состояние записать при успешном открытии (`Ok` или `Simulated`).
    opened_status: AudioOutputStatus,
}

impl MeteredAudioOutputFactory {
    /// Обёртка над произвольной фабрикой (для тестов счётчика с фейковым выходом).
    #[cfg(test)]
    pub(crate) fn wrap(
        inner: Arc<dyn AudioOutputFactory>,
        opened_status: AudioOutputStatus,
    ) -> (Self, AudioMeter) {
        let meter = AudioMeter::default();
        (
            Self {
                inner,
                meter: meter.clone(),
                opened_status,
            },
            meter,
        )
    }
}

impl AudioOutputFactory for MeteredAudioOutputFactory {
    /// Открывает выход через настоящую фабрику. Ошибка не прячется: плеер получает её
    /// как есть и идёт по своему восстановимому пути; счётчик лишь запоминает итог.
    fn create_output(
        &self,
        spec: AudioOutputSpec,
        device_request: AudioOutputDeviceRequest,
    ) -> anyhow::Result<CreatedAudioOutput> {
        match self.inner.create_output(spec, device_request) {
            Ok(created) => {
                self.meter
                    .note_output_opened(self.opened_status, created.output.clock());
                let output = MeteredAudioOutput {
                    inner: created.output,
                    meter: self.meter.clone(),
                    sample_rate: spec.sample_rate,
                };
                Ok(CreatedAudioOutput::new(Box::new(output), created.route))
            }
            Err(error) => {
                self.meter.note_output_unavailable();
                Err(error)
            }
        }
    }
}

/// Выход со счётчиком: всё пересылает настоящему, по пути считает записанное.
struct MeteredAudioOutput {
    /// Настоящий выход.
    inner: Box<dyn PlayerAudioOutput>,
    /// Счётчик.
    meter: AudioMeter,
    /// Частота исходного PCM (для перевода кадров во время).
    sample_rate: u32,
}

impl PlayerAudioOutput for MeteredAudioOutput {
    fn write_samples(
        &mut self,
        samples: &[f32],
        intent: AudioOutputWriteIntent,
    ) -> Result<AudioOutputWriteReport, AudioOutputWriteError> {
        let report = self.inner.write_samples(samples, intent)?;
        self.meter
            .note_written(report.submitted_input_frames().get(), self.sample_rate);
        Ok(report)
    }

    fn play(&mut self) -> anyhow::Result<()> {
        self.inner.play()
    }

    fn pause_and_freeze_clock(&mut self) -> anyhow::Result<AudioOutputClockTiming> {
        self.inner.pause_and_freeze_clock()
    }

    fn clear_buffer_for_seek(&mut self, generation: u64) -> anyhow::Result<u64> {
        self.inner.clear_buffer_for_seek(generation)
    }

    fn set_volume(&mut self, volume: f32) {
        self.inner.set_volume(volume);
    }

    fn buffer_level_ms(&self) -> f64 {
        self.inner.buffer_level_ms()
    }

    /// Часы — настоящие, без обёртки: от них зависит синхронизация видео.
    fn clock(&self) -> Arc<dyn PlayerAudioClock> {
        self.inner.clock()
    }

    fn stream_health(&self) -> AudioOutputStreamHealth {
        self.inner.stream_health()
    }
}

#[cfg(test)]
mod tests;
