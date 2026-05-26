use std::sync::Arc;
use std::time::Duration;

/// Decoded PCM spec, по которому composition layer создаёт audio output.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct AudioOutputSpec {
    /// Sample rate decoded PCM, полученный только после первого decode.
    pub sample_rate: u32,

    /// Количество interleaved channels в decoded PCM.
    pub channels: u32,
}

/// Нейтральная фабрика audio output-а без знания о CPAL или concrete backend-е.
pub trait AudioOutputFactory: Send + Sync {
    /// Создаёт output под decoded audio spec.
    fn create_output(&self, spec: AudioOutputSpec) -> anyhow::Result<Box<dyn PlayerAudioOutput>>;
}

/// Нейтральный output contract, которым управляет playback pipeline.
pub trait PlayerAudioOutput: Send {
    /// Записывает interleaved PCM samples в output buffer.
    fn write_samples(&mut self, samples: &[f32]) -> u64;

    /// Запускает output stream.
    fn play(&mut self) -> anyhow::Result<()>;

    /// Ставит output stream на паузу.
    fn pause(&mut self) -> anyhow::Result<()>;

    /// Очищает queued samples для seek generation и возвращает ack generation.
    fn clear_buffer_for_seek(&mut self, generation: u64) -> anyhow::Result<u64>;

    /// Применяет volume для последующих samples.
    fn set_volume(&mut self, volume: f32);

    /// Возвращает текущий уровень output buffer-а в миллисекундах.
    fn buffer_level_ms(&self) -> f64;

    /// Возвращает playback clock как нейтральный shared handle.
    fn clock(&self) -> Arc<dyn PlayerAudioClock>;
}

/// Нейтральный playback clock для A/V sync и EOF-drain diagnostics.
pub trait PlayerAudioClock: Send + Sync {
    /// Возвращает текущую playback позицию относительно clock base.
    fn now(&self) -> Duration;

    /// Сбрасывает clock state после seek/output clear.
    fn reset(&self);

    /// Возвращает количество output callbacks, где stream недополучил samples.
    fn underrun_callbacks(&self) -> u64;
}

/// Factory по умолчанию для тестов/ручного `PlayerSession::new` без composition wiring.
pub(crate) struct MissingAudioOutputFactory;

impl AudioOutputFactory for MissingAudioOutputFactory {
    /// Явно сообщает, что production adapter не был установлен в composition layer.
    fn create_output(&self, _spec: AudioOutputSpec) -> anyhow::Result<Box<dyn PlayerAudioOutput>> {
        anyhow::bail!("audio output factory is not installed")
    }
}

/// Создаёт shared missing-factory handle для default-конструкторов без CPAL side effects.
pub(crate) fn missing_audio_output_factory() -> Arc<dyn AudioOutputFactory> {
    Arc::new(MissingAudioOutputFactory)
}
