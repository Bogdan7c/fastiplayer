//! CPAL-backed реализация нейтральных audio output contracts.
//!
//! `audio` владеет concrete CPAL output implementation. Playback worker
//! получает только trait objects из `audio-core`, но PCM-запись остаётся прямой
//! и не проходит через дополнительный command thread в real-time refill path.

use std::sync::Arc;
use std::time::Duration;

use audio_core::{AudioOutputFactory, AudioOutputSpec, PlayerAudioClock, PlayerAudioOutput};

use crate::{AudioClock, AudioOutput, AudioOutputDeviceController};

/// Production factory, создающая concrete CPAL output за neutral boundary.
#[derive(Debug, Clone)]
pub struct CpalAudioOutputFactory {
    /// Shared selection owner, который settings runtime может менять через audio API.
    output_device_controller: AudioOutputDeviceController,
}

impl Default for CpalAudioOutputFactory {
    fn default() -> Self {
        Self::new(AudioOutputDeviceController::default())
    }
}

impl CpalAudioOutputFactory {
    /// Создаёт factory с shared output-device controller-ом.
    #[must_use]
    pub fn new(output_device_controller: AudioOutputDeviceController) -> Self {
        Self {
            output_device_controller,
        }
    }

    /// Возвращает текущий stable id, чтобы tests/diagnostics проверяли owner boundary.
    pub fn selected_output_device_id(&self) -> anyhow::Result<String> {
        Ok(self.output_device_controller.selected_device_id()?)
    }
}

impl AudioOutputFactory for CpalAudioOutputFactory {
    /// Создаёт concrete output и отдаёт его как neutral trait object.
    fn create_output(&self, spec: AudioOutputSpec) -> anyhow::Result<Box<dyn PlayerAudioOutput>> {
        let output_device_id = self.output_device_controller.selected_device_id()?;
        let output =
            AudioOutput::new_with_device_id(spec.sample_rate, spec.channels, &output_device_id)?;
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

    use super::{AudioClock, AudioOutput, AudioOutputDeviceController, CpalAudioOutputFactory};

    #[test]
    fn cpal_output_factory_is_exposed_as_neutral_contract_object() {
        let _factory: Arc<dyn AudioOutputFactory> = Arc::new(CpalAudioOutputFactory::default());
    }

    #[test]
    fn selected_available_device_is_kept_inside_audio_owner_boundary() {
        let controller = AudioOutputDeviceController::new("cpal-0.15-name:USB%20DAC");
        let factory = CpalAudioOutputFactory::new(controller);

        assert_eq!(
            factory
                .selected_output_device_id()
                .expect("factory должен читать stable id без CPAL type leakage"),
            "cpal-0.15-name:USB%20DAC"
        );
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

    #[test]
    fn audio_crate_provider_boundary_has_no_app_egui_dependency() {
        let manifest = std::fs::read_to_string(
            std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("Cargo.toml"),
        )
        .expect("audio Cargo.toml должен читаться");

        assert!(
            !manifest.contains("app-egui") && !manifest.contains("app_egui"),
            "audio crate не должен зависеть от app-egui"
        );
    }
}
