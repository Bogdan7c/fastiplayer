//! CPAL-backed реализация нейтральных audio output contracts.
//!
//! `audio` владеет concrete CPAL output implementation. Playback worker
//! получает только trait objects из `audio-core`, но PCM-запись остаётся прямой
//! и не проходит через дополнительный command thread в real-time refill path.

use std::sync::Arc;
use std::time::Duration;

use audio_core::{AudioOutputFactory, AudioOutputSpec, PlayerAudioClock, PlayerAudioOutput};

use crate::{AudioClock, AudioOutput};

/// Production factory, создающая concrete CPAL output за neutral boundary.
#[derive(Debug, Default)]
pub struct CpalAudioOutputFactory;

impl AudioOutputFactory for CpalAudioOutputFactory {
    /// Создаёт concrete output и отдаёт его как neutral trait object.
    fn create_output(&self, spec: AudioOutputSpec) -> anyhow::Result<Box<dyn PlayerAudioOutput>> {
        let output = AudioOutput::new(spec.sample_rate, spec.channels)?;
        Ok(Box::new(output))
    }
}

impl PlayerAudioOutput for AudioOutput {
    /// Записывает PCM samples напрямую в concrete ring buffer output-а.
    fn write_samples(&mut self, samples: &[f32]) -> u64 {
        AudioOutput::write_samples(self, samples)
    }

    /// Запускает CPAL stream и возвращает backend error без сокрытия.
    fn play(&mut self) -> anyhow::Result<()> {
        AudioOutput::play(self)
    }

    /// Ставит CPAL stream на паузу и возвращает backend error без сокрытия.
    fn pause(&mut self) -> anyhow::Result<()> {
        AudioOutput::pause(self)
    }

    /// Очищает buffer/resampler state на владельце concrete output-а.
    fn clear_buffer_for_seek(&mut self, generation: u64) -> anyhow::Result<u64> {
        AudioOutput::clear_buffer_for_seek(self, generation)
    }

    /// Обновляет volume для следующих записываемых samples.
    fn set_volume(&mut self, volume: f32) {
        AudioOutput::set_volume(self, volume);
    }

    /// Возвращает текущий уровень concrete output buffer-а.
    fn buffer_level_ms(&self) -> f64 {
        AudioOutput::buffer_level_ms(self)
    }

    /// Возвращает concrete clock как neutral shared handle.
    fn clock(&self) -> Arc<dyn PlayerAudioClock> {
        let clock: Arc<dyn PlayerAudioClock> = self.clock().clone();
        clock
    }
}

impl PlayerAudioClock for AudioClock {
    /// Возвращает playback позицию concrete clock-а.
    fn now(&self) -> Duration {
        AudioClock::now(self)
    }

    /// Сбрасывает concrete clock при seek/reset playback.
    fn reset(&self) {
        AudioClock::reset(self);
    }

    /// Возвращает число CPAL callbacks, где output писал silence из-за underrun-а.
    fn underrun_callbacks(&self) -> u64 {
        AudioClock::underrun_callbacks(self)
    }
}

#[cfg(test)]
mod tests {
    use std::sync::Arc;

    use audio_core::{AudioOutputFactory, PlayerAudioClock, PlayerAudioOutput};

    use super::{AudioClock, AudioOutput, CpalAudioOutputFactory};

    #[test]
    fn cpal_output_factory_is_exposed_as_neutral_contract_object() {
        let _factory: Arc<dyn AudioOutputFactory> = Arc::new(CpalAudioOutputFactory);
    }

    #[test]
    fn concrete_audio_output_satisfies_neutral_output_contract() {
        fn assert_output_contract<T: PlayerAudioOutput>() {}

        assert_output_contract::<AudioOutput>();
    }

    #[test]
    fn concrete_audio_clock_satisfies_neutral_clock_contract() {
        fn assert_clock_contract<T: PlayerAudioClock + Send + Sync>() {}

        assert_clock_contract::<AudioClock>();
    }
}
