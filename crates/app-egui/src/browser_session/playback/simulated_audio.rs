//! Тихая звуковая карта с часами реального времени — ТОЛЬКО для тестов.
//!
//! Включается переменной `FASTIPLAYER_TEST_BROWSER_AUDIO=simulated`
//! (см. [`super::metered_audio::BrowserAudioOutputChoice`]). Принимает настоящий PCM от
//! настоящего декодера и «проигрывает» его в темпе настоящего времени: часы звука
//! ведут позицию плеера точно так же, как у живой карты, но колонки молчат.
//!
//! Модель карты:
//! - играет — «прозвучало» растёт со скоростью частоты дискретизации от момента-якоря;
//! - не обгоняет записанное: если PCM кончился, часы стоят (как при опустошении буфера
//!   у настоящей карты), а при следующей записи якорь переставляется, чтобы простой
//!   не засчитался задним числом;
//! - пауза замораживает часы; перемотка выбрасывает незвучавший хвост.

use std::sync::{Arc, Mutex, MutexGuard, PoisonError};
use std::time::{Duration, Instant};

use player_core::{
    AudioOutputClockTiming, AudioOutputDeviceRequest, AudioOutputDeviceRoute, AudioOutputFactory,
    AudioOutputInputFrameCount, AudioOutputSpec, AudioOutputStreamFrameCount,
    AudioOutputWriteError, AudioOutputWriteIntent, AudioOutputWriteReport, CreatedAudioOutput,
    PlayerAudioClock, PlayerAudioOutput,
};

/// Фабрика тихих карт.
#[derive(Debug, Clone, Copy, Default)]
pub(crate) struct SimulatedRealTimeAudioFactory;

impl AudioOutputFactory for SimulatedRealTimeAudioFactory {
    fn create_output(
        &self,
        spec: AudioOutputSpec,
        _device_request: AudioOutputDeviceRequest,
    ) -> anyhow::Result<CreatedAudioOutput> {
        let channels = usize::try_from(spec.channels())?;
        if channels == 0 || spec.sample_rate == 0 {
            anyhow::bail!("the simulated audio sink needs at least one channel and a sample rate");
        }
        let card = SimulatedCard::new(spec.sample_rate);
        Ok(CreatedAudioOutput::new(
            Box::new(SimulatedOutput { card, channels }),
            AudioOutputDeviceRoute::SystemDefault,
        ))
    }
}

/// Состояние карты (общее у выхода и его часов).
#[derive(Debug)]
struct CardState {
    /// Частота дискретизации, кадров в секунду.
    sample_rate: u32,
    /// Кадров принято от плеера.
    submitted_frames: u64,
    /// Кадров прозвучало к моменту якоря.
    played_frames_at_anchor: u64,
    /// С какого момента карта играет; `None` — стоит (пауза или ещё не запущена).
    playing_since: Option<Instant>,
}

impl CardState {
    /// Сколько кадров прозвучало к моменту `now` (не больше записанного).
    fn played_frames(&self, now: Instant) -> u64 {
        let Some(anchor) = self.playing_since else {
            return self.played_frames_at_anchor;
        };
        let elapsed_frames = now.saturating_duration_since(anchor).as_nanos()
            * u128::from(self.sample_rate)
            / 1_000_000_000;
        let elapsed_frames = u64::try_from(elapsed_frames).unwrap_or(u64::MAX);
        self.played_frames_at_anchor
            .saturating_add(elapsed_frames)
            .min(self.submitted_frames)
    }

    /// Переносит якорь в `now`, зафиксировав прозвучавшее: простой при пустом буфере
    /// не засчитывается задним числом.
    fn reanchor(&mut self, now: Instant) {
        self.played_frames_at_anchor = self.played_frames(now);
        if self.playing_since.is_some() {
            self.playing_since = Some(now);
        }
    }

    /// Кадры → время.
    fn frames_to_time(&self, frames: u64) -> Duration {
        let nanos = u128::from(frames) * 1_000_000_000 / u128::from(self.sample_rate);
        Duration::from_nanos(u64::try_from(nanos).unwrap_or(u64::MAX))
    }

    /// Снимок часов на момент `now`.
    fn timing(&self, now: Instant) -> AudioOutputClockTiming {
        AudioOutputClockTiming::new(
            self.frames_to_time(self.played_frames(now)),
            self.frames_to_time(self.submitted_frames),
        )
    }
}

/// Карта: ручка к общему состоянию.
#[derive(Debug, Clone)]
struct SimulatedCard {
    state: Arc<Mutex<CardState>>,
}

impl SimulatedCard {
    fn new(sample_rate: u32) -> Self {
        Self {
            state: Arc::new(Mutex::new(CardState {
                sample_rate,
                submitted_frames: 0,
                played_frames_at_anchor: 0,
                playing_since: None,
            })),
        }
    }

    /// Замок; внутри только числа, отравление согласованности не рвёт.
    fn lock(&self) -> MutexGuard<'_, CardState> {
        self.state.lock().unwrap_or_else(PoisonError::into_inner)
    }
}

/// Выход тихой карты.
struct SimulatedOutput {
    card: SimulatedCard,
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
        let mut state = self.card.lock();
        state.reanchor(Instant::now());
        state.submitted_frames += frames as u64;
        Ok(AudioOutputWriteReport::complete(
            AudioOutputInputFrameCount::new(frames),
            AudioOutputStreamFrameCount::new(frames),
        ))
    }

    fn play(&mut self) -> anyhow::Result<()> {
        let mut state = self.card.lock();
        if state.playing_since.is_none() {
            state.playing_since = Some(Instant::now());
        }
        Ok(())
    }

    fn pause_and_freeze_clock(&mut self) -> anyhow::Result<AudioOutputClockTiming> {
        let now = Instant::now();
        let mut state = self.card.lock();
        state.played_frames_at_anchor = state.played_frames(now);
        state.playing_since = None;
        Ok(state.timing(now))
    }

    fn clear_buffer_for_seek(&mut self, generation: u64) -> anyhow::Result<u64> {
        let mut state = self.card.lock();
        state.reanchor(Instant::now());
        state.submitted_frames = state.played_frames_at_anchor;
        Ok(generation)
    }

    /// Громкость тихой карте не нужна: она ничего не выводит.
    fn set_volume(&mut self, _volume: f32) {}

    fn buffer_level_ms(&self) -> f64 {
        let timing = self.card.lock().timing(Instant::now());
        timing.submitted_output_tail().as_secs_f64() * 1000.0
    }

    fn clock(&self) -> Arc<dyn PlayerAudioClock> {
        Arc::new(SimulatedClock {
            card: self.card.clone(),
        })
    }
}

/// Часы тихой карты.
struct SimulatedClock {
    card: SimulatedCard,
}

impl PlayerAudioClock for SimulatedClock {
    fn now(&self) -> Duration {
        self.output_timing().audible_output_position()
    }

    fn output_timing(&self) -> AudioOutputClockTiming {
        self.card.lock().timing(Instant::now())
    }

    fn reset(&self) {
        let now = Instant::now();
        let mut state = self.card.lock();
        state.submitted_frames = 0;
        state.played_frames_at_anchor = 0;
        if state.playing_since.is_some() {
            state.playing_since = Some(now);
        }
    }

    /// Тихая карта не опустошается «по-настоящему»: недоборов не бывает.
    fn underrun_callbacks(&self) -> u64 {
        0
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Карта с частотой 1000 Гц и одним каналом: кадр = миллисекунда.
    fn card_output() -> SimulatedOutput {
        SimulatedOutput {
            card: SimulatedCard::new(1_000),
            channels: 1,
        }
    }

    /// Записанное звучит в реальном времени и не обгоняет запись.
    #[test]
    fn played_time_follows_real_time_and_never_passes_written() {
        let mut output = card_output();
        output
            .write_samples(&[0.0; 200], AudioOutputWriteIntent::DirectDecodedPcm)
            .expect("write");
        output.play().expect("play");
        std::thread::sleep(Duration::from_millis(60));
        let played = output.clock().now();
        assert!(played >= Duration::from_millis(50), "played {played:?}");
        assert!(played <= Duration::from_millis(200), "played {played:?}");

        std::thread::sleep(Duration::from_millis(250));
        assert_eq!(
            output.clock().now(),
            Duration::from_millis(200),
            "the card stops at the end of what was written"
        );
    }

    /// Пауза замораживает часы.
    #[test]
    fn pause_freezes_the_clock() {
        let mut output = card_output();
        output
            .write_samples(&[0.0; 1_000], AudioOutputWriteIntent::DirectDecodedPcm)
            .expect("write");
        output.play().expect("play");
        std::thread::sleep(Duration::from_millis(20));
        let frozen = output.pause_and_freeze_clock().expect("pause");
        std::thread::sleep(Duration::from_millis(40));
        assert_eq!(output.clock().now(), frozen.audible_output_position());
    }

    /// Перемотка выбрасывает незвучавший хвост.
    #[test]
    fn seek_clear_drops_the_unplayed_tail() {
        let mut output = card_output();
        output
            .write_samples(&[0.0; 500], AudioOutputWriteIntent::DirectDecodedPcm)
            .expect("write");
        output.clear_buffer_for_seek(7).expect("clear");
        assert_eq!(output.buffer_level_ms(), 0.0);
    }
}
