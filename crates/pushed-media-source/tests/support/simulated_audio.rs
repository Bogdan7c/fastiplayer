//! Симулятор звуковой карты для безоконных тестов: принимает настоящий PCM от
//! настоящего декодера, а часы двигает виртуальное время теста.
//!
//! Часы не обгоняют записанное: если плеер не дал звука, «карта» стоит, как
//! настоящая при опустошении буфера. Поэтому позиция плеера (её ведут часы
//! звука) честно отражает, сколько звука реально «прозвучало».

use std::sync::{Arc, Mutex, MutexGuard};
use std::time::Duration;

use player_core::{
    AudioOutputClockTiming, AudioOutputDeviceRequest, AudioOutputDeviceRoute, AudioOutputFactory,
    AudioOutputInputFrameCount, AudioOutputSpec, AudioOutputStreamFrameCount,
    AudioOutputWriteError, AudioOutputWriteIntent, AudioOutputWriteReport, CreatedAudioOutput,
    PlayerAudioClock, PlayerAudioOutput,
};

/// Общее состояние симулятора: его видит и плеер (через выход и часы), и тест.
#[derive(Debug, Default)]
struct DeviceState {
    /// Частота и число каналов открытого выхода.
    format: Option<(u32, usize)>,
    /// Весь PCM, который плеер записал (для проверки звука).
    written_samples: Vec<f32>,
    /// Кадров принято.
    submitted_frames: u64,
    /// Кадров «прозвучало».
    played_frames: u64,
    /// Идёт ли воспроизведение.
    playing: bool,
    /// Заморожены ли часы паузой.
    frozen: Option<AudioOutputClockTiming>,
}

/// Ручка теста к симулятору.
#[derive(Debug, Clone, Default)]
pub struct SimulatedAudioDevice {
    /// Общее состояние.
    state: Arc<Mutex<DeviceState>>,
}

impl SimulatedAudioDevice {
    /// Фабрика выходов для `PlayerSession`.
    pub fn factory(&self) -> Arc<dyn AudioOutputFactory> {
        Arc::new(SimulatedOutputFactory {
            device: self.clone(),
        })
    }

    /// Проходит виртуальное время: играющая «карта» проигрывает записанное.
    pub fn advance(&self, elapsed: Duration) {
        let mut state = self.lock();
        let Some((sample_rate, _)) = state.format else {
            return;
        };
        if !state.playing {
            return;
        }
        let frames = u64::try_from(elapsed.as_nanos() * u128::from(sample_rate) / 1_000_000_000)
            .unwrap_or(u64::MAX);
        state.played_frames = (state.played_frames + frames).min(state.submitted_frames);
    }

    /// Весь записанный PCM.
    pub fn written_samples(&self) -> Vec<f32> {
        self.lock().written_samples.clone()
    }

    /// Частота и каналы открытого выхода.
    pub fn format(&self) -> Option<(u32, usize)> {
        self.lock().format
    }

    fn lock(&self) -> MutexGuard<'_, DeviceState> {
        self.state.lock().expect("simulated audio state")
    }

    fn timing(&self) -> AudioOutputClockTiming {
        let state = self.lock();
        if let Some(frozen) = state.frozen {
            return frozen;
        }
        let Some((sample_rate, _)) = state.format else {
            return AudioOutputClockTiming::new(Duration::ZERO, Duration::ZERO);
        };
        let to_time = |frames: u64| {
            Duration::from_nanos(
                u64::try_from(u128::from(frames) * 1_000_000_000 / u128::from(sample_rate))
                    .unwrap_or(u64::MAX),
            )
        };
        AudioOutputClockTiming::new(
            to_time(state.played_frames),
            to_time(state.submitted_frames),
        )
    }
}

/// Фабрика выходов.
struct SimulatedOutputFactory {
    device: SimulatedAudioDevice,
}

impl AudioOutputFactory for SimulatedOutputFactory {
    fn create_output(
        &self,
        spec: AudioOutputSpec,
        _device_request: AudioOutputDeviceRequest,
    ) -> anyhow::Result<CreatedAudioOutput> {
        let channels = usize::try_from(spec.channels()).expect("channel count");
        {
            let mut state = self.device.lock();
            state.format = Some((spec.sample_rate, channels));
        }
        Ok(CreatedAudioOutput::new(
            Box::new(SimulatedOutput {
                device: self.device.clone(),
                channels,
            }),
            AudioOutputDeviceRoute::SystemDefault,
        ))
    }
}

/// Выход звука симулятора.
struct SimulatedOutput {
    device: SimulatedAudioDevice,
    channels: usize,
}

impl PlayerAudioOutput for SimulatedOutput {
    fn write_samples(
        &mut self,
        samples: &[f32],
        _intent: AudioOutputWriteIntent,
    ) -> Result<AudioOutputWriteReport, AudioOutputWriteError> {
        if !samples.len().is_multiple_of(self.channels) {
            return Err(AudioOutputWriteError::InputNotFrameAligned {
                input_samples: samples.len(),
                input_channels: self.channels,
            });
        }
        let frames = samples.len() / self.channels;
        let mut state = self.device.lock();
        state.written_samples.extend_from_slice(samples);
        state.submitted_frames += frames as u64;
        Ok(AudioOutputWriteReport::complete(
            AudioOutputInputFrameCount::new(frames),
            AudioOutputStreamFrameCount::new(frames),
        ))
    }

    fn play(&mut self) -> anyhow::Result<()> {
        let mut state = self.device.lock();
        state.playing = true;
        state.frozen = None;
        Ok(())
    }

    fn pause_and_freeze_clock(&mut self) -> anyhow::Result<AudioOutputClockTiming> {
        let timing = self.device.timing();
        let mut state = self.device.lock();
        state.playing = false;
        state.frozen = Some(timing);
        Ok(timing)
    }

    fn clear_buffer_for_seek(&mut self, generation: u64) -> anyhow::Result<u64> {
        let mut state = self.device.lock();
        state.submitted_frames = state.played_frames;
        Ok(generation)
    }

    fn set_volume(&mut self, _volume: f32) {}

    fn buffer_level_ms(&self) -> f64 {
        let timing = self.device.timing();
        timing
            .submitted_output_end_position()
            .saturating_sub(timing.audible_output_position())
            .as_secs_f64()
            * 1000.0
    }

    fn clock(&self) -> Arc<dyn PlayerAudioClock> {
        Arc::new(SimulatedClock {
            device: self.device.clone(),
        })
    }
}

/// Часы симулятора.
struct SimulatedClock {
    device: SimulatedAudioDevice,
}

impl PlayerAudioClock for SimulatedClock {
    fn now(&self) -> Duration {
        self.device.timing().audible_output_position()
    }

    fn output_timing(&self) -> AudioOutputClockTiming {
        self.device.timing()
    }

    fn reset(&self) {
        let mut state = self.device.lock();
        state.played_frames = 0;
        state.submitted_frames = 0;
        state.frozen = None;
    }

    fn underrun_callbacks(&self) -> u64 {
        0
    }
}
