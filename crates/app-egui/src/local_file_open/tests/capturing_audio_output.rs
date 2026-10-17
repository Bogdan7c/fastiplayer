//! Фейковый audio output для сквозных тестов: принимает весь PCM и считает, сколько дошло.
//!
//! Clock «проигрывает» записанное мгновенно (audible = конец записанного), поэтому player
//! доходит до `Ended` быстрее реального времени, а тест видит объём и громкость звука.

use std::sync::{Arc, Mutex};
use std::time::Duration;

use player_core::{
    AudioOutputClockTiming, AudioOutputDeviceRequest, AudioOutputDeviceRoute, AudioOutputFactory,
    AudioOutputInputFrameCount, AudioOutputSpec, AudioOutputStreamFrameCount,
    AudioOutputWriteError, AudioOutputWriteIntent, AudioOutputWriteReport, CreatedAudioOutput,
    PlayerAudioClock, PlayerAudioOutput,
};

/// Что дошло до output-а за всё воспроизведение (сумма по всем созданным output-ам).
#[derive(Debug, Clone, Copy, Default)]
pub(super) struct CapturedAudio {
    /// Число PCM frames (по одному сэмплу на канал).
    pub(super) pcm_frames: usize,
    /// Максимальная абсолютная амплитуда сэмпла.
    pub(super) peak_amplitude: f32,
}

/// Factory, у которой тест потом спрашивает итог.
#[derive(Default)]
pub(super) struct CapturingAudioOutputFactory {
    captured: Arc<Mutex<CapturedAudio>>,
}

impl CapturingAudioOutputFactory {
    /// Снимок накопленного итога.
    pub(super) fn captured(&self) -> CapturedAudio {
        *self.captured.lock().expect("captured audio lock")
    }
}

impl AudioOutputFactory for CapturingAudioOutputFactory {
    fn create_output(
        &self,
        spec: AudioOutputSpec,
        _device_request: AudioOutputDeviceRequest,
    ) -> anyhow::Result<CreatedAudioOutput> {
        let channels = usize::try_from(spec.channels())?;
        anyhow::ensure!(channels > 0, "decoded audio spec has no channels");
        let output = CapturingAudioOutput {
            channels,
            clock: Arc::new(InstantClock::new(spec.sample_rate)),
            captured: Arc::clone(&self.captured),
        };
        Ok(CreatedAudioOutput::new(
            Box::new(output),
            AudioOutputDeviceRoute::SystemDefault,
        ))
    }
}

/// Один output: считает frames и пик, ничего не воспроизводит.
struct CapturingAudioOutput {
    channels: usize,
    clock: Arc<InstantClock>,
    captured: Arc<Mutex<CapturedAudio>>,
}

impl PlayerAudioOutput for CapturingAudioOutput {
    fn write_samples(
        &mut self,
        samples: &[f32],
        _intent: AudioOutputWriteIntent,
    ) -> Result<AudioOutputWriteReport, AudioOutputWriteError> {
        let frames = samples.len() / self.channels;
        let peak = samples
            .iter()
            .fold(0.0_f32, |peak, sample| peak.max(sample.abs()));
        {
            let mut captured = self.captured.lock().expect("captured audio lock");
            captured.pcm_frames += frames;
            captured.peak_amplitude = captured.peak_amplitude.max(peak);
        }
        self.clock.advance(frames);
        AudioOutputWriteReport::try_new(
            AudioOutputInputFrameCount::new(frames),
            AudioOutputStreamFrameCount::new(frames),
            AudioOutputStreamFrameCount::new(frames),
        )
    }

    fn play(&mut self) -> anyhow::Result<()> {
        Ok(())
    }

    fn pause_and_freeze_clock(&mut self) -> anyhow::Result<AudioOutputClockTiming> {
        Ok(self.clock.output_timing())
    }

    fn clear_buffer_for_seek(&mut self, generation: u64) -> anyhow::Result<u64> {
        Ok(generation)
    }

    fn set_volume(&mut self, _volume: f32) {}

    fn buffer_level_ms(&self) -> f64 {
        // Записанное «проиграно» мгновенно: буфер всегда пуст.
        0.0
    }

    fn clock(&self) -> Arc<dyn PlayerAudioClock> {
        self.clock.clone()
    }
}

/// Clock, у которого слышимая позиция равна концу записанного PCM.
struct InstantClock {
    sample_rate: u32,
    written_frames: Mutex<u64>,
}

impl InstantClock {
    fn new(sample_rate: u32) -> Self {
        Self {
            sample_rate,
            written_frames: Mutex::new(0),
        }
    }

    fn advance(&self, frames: usize) {
        let mut written_frames = self.written_frames.lock().expect("clock lock");
        *written_frames += u64::try_from(frames).unwrap_or(u64::MAX);
    }
}

impl PlayerAudioClock for InstantClock {
    fn now(&self) -> Duration {
        let written_frames = *self.written_frames.lock().expect("clock lock");
        Duration::from_secs_f64(written_frames as f64 / f64::from(self.sample_rate.max(1)))
    }

    fn output_timing(&self) -> AudioOutputClockTiming {
        let position = self.now();
        AudioOutputClockTiming::new(position, position)
    }

    fn reset(&self) {
        *self.written_frames.lock().expect("clock lock") = 0;
    }

    fn underrun_callbacks(&self) -> u64 {
        0
    }
}
