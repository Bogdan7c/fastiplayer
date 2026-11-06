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
    /// Позиция слышимого звука, на которой часы заморожены паузой. Конец записанного
    /// при этом не замораживается: он всегда считается по живому буферу (иначе после
    /// очистки на паузе уровень остался бы старым).
    frozen_audible: Option<Duration>,
    /// Кадров записано за всё время (не сбрасывается перемоткой и часами).
    written_frames_total: u64,
    /// Кадров «прозвучало» за всё время (не сбрасывается новым выходом и перемоткой).
    played_frames_total: u64,
    /// Начинала ли «карта» играть хоть раз.
    started: bool,
    /// Отрезки тишины после старта: сколько кадров было записано к этому моменту и
    /// сколько времени «карта» молчала (пауза выхода или пустой буфер).
    silences: Vec<SilenceSlice>,
    /// Номер последнего созданного выхода.
    current_output: u64,
    /// Кадров, записанных, но выброшенных вместе с уничтоженным выходом.
    discarded_frames: u64,
    /// Сколько кадров было записано к каждой очистке выхода перемоткой.
    written_frames_at_seek_clears: Vec<u64>,
}

/// Отрезок тишины, которую слышал бы зритель.
#[derive(Debug, Clone, Copy)]
struct SilenceSlice {
    /// Кадров записано к началу отрезка.
    written_frames_before: u64,
    /// Длительность тишины по часам «карты».
    silent_for: Duration,
    /// Та же тишина по настоящим часам: шаг бывает длиннее, чем карте разрешено пройти.
    real_silent_for: Duration,
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
        self.advance_for_real_time(elapsed, elapsed);
    }

    /// То же, но шаг настоящего времени длиной `real_elapsed` даёт карте пройти только `elapsed`.
    ///
    /// Часы видео при этом идут настоящие, поэтому длину дыры сравнивают с
    /// [`Self::mid_stream_silence_real_time`]: по часам карты тишина выходит короче дыры на
    /// столько, на сколько шаги длиннее разрешённого.
    pub fn advance_for_real_time(&self, elapsed: Duration, real_elapsed: Duration) {
        let mut state = self.lock();
        let Some((sample_rate, _)) = state.format else {
            return;
        };
        if !state.started {
            return;
        }
        let written_frames_before = state.written_frames_total;
        if !state.playing {
            state.silences.push(SilenceSlice {
                written_frames_before,
                silent_for: elapsed,
                real_silent_for: real_elapsed,
            });
            return;
        }
        let frames = u64::try_from(elapsed.as_nanos() * u128::from(sample_rate) / 1_000_000_000)
            .unwrap_or(u64::MAX);
        let available = state.submitted_frames - state.played_frames;
        let played = frames.min(available);
        state.played_frames += played;
        state.played_frames_total += played;
        let starved_frames = frames - played;
        if starved_frames > 0 {
            let silent_for = Duration::from_nanos(
                u64::try_from(u128::from(starved_frames) * 1_000_000_000 / u128::from(sample_rate))
                    .unwrap_or(u64::MAX),
            );
            // Голодная доля шага по настоящим часам та же, что и по часам карты.
            let real_silent_for = real_elapsed.mul_f64(starved_frames as f64 / frames as f64);
            state.silences.push(SilenceSlice {
                written_frames_before,
                silent_for,
                real_silent_for,
            });
        }
    }

    /// Сколько тишины зритель слышал посреди звука: после старта и до того, как
    /// плеер записал последний сэмпл. Тишина после конца звука не считается.
    pub fn mid_stream_silence(&self) -> Duration {
        let state = self.lock();
        state
            .silences
            .iter()
            .filter(|silence| silence.written_frames_before < state.written_frames_total)
            .map(|silence| silence.silent_for)
            .sum()
    }

    /// Та же тишина посреди звука, но по настоящим часам, см. [`Self::advance_for_real_time`].
    pub fn mid_stream_silence_real_time(&self) -> Duration {
        let state = self.lock();
        state
            .silences
            .iter()
            .filter(|silence| silence.written_frames_before < state.written_frames_total)
            .map(|silence| silence.real_silent_for)
            .sum()
    }

    /// Сколько звука зритель услышал за всё время: записанное впрок, но не проигранное
    /// (выход встал на паузу), сюда не входит.
    pub fn heard_audio(&self) -> Duration {
        let state = self.lock();
        let Some((sample_rate, _)) = state.format else {
            return Duration::ZERO;
        };
        Duration::from_nanos(
            u64::try_from(
                u128::from(state.played_frames_total) * 1_000_000_000 / u128::from(sample_rate),
            )
            .unwrap_or(u64::MAX),
        )
    }

    /// Сколько записанного звука зритель не услышал: выход уничтожили вместе с буфером.
    pub fn discarded_audio(&self) -> Duration {
        let state = self.lock();
        let Some((sample_rate, _)) = state.format else {
            return Duration::ZERO;
        };
        Duration::from_nanos(
            u64::try_from(
                u128::from(state.discarded_frames) * 1_000_000_000 / u128::from(sample_rate),
            )
            .unwrap_or(u64::MAX),
        )
    }

    /// Номер PCM-кадра в [`Self::written_samples`], с которого пошёл звук после
    /// каждой очистки выхода перемоткой (недоигранное до неё выброшено и не звучало).
    pub fn written_frames_at_seek_clears(&self) -> Vec<usize> {
        self.lock()
            .written_frames_at_seek_clears
            .iter()
            .map(|frames| usize::try_from(*frames).expect("PCM frame index"))
            .collect()
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
            state
                .frozen_audible
                .unwrap_or_else(|| to_time(state.played_frames)),
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
        let output_id = {
            let mut state = self.device.lock();
            state.format = Some((spec.sample_rate, channels));
            state.current_output += 1;
            state.current_output
        };
        Ok(CreatedAudioOutput::new(
            Box::new(SimulatedOutput {
                device: self.device.clone(),
                channels,
                output_id,
            }),
            AudioOutputDeviceRoute::SystemDefault,
        ))
    }
}

/// Выход звука симулятора.
struct SimulatedOutput {
    device: SimulatedAudioDevice,
    channels: usize,
    /// Номер выхода: уничтожение заменённого выхода (хвост при смене формата)
    /// не трогает буфер нового.
    output_id: u64,
}

impl Drop for SimulatedOutput {
    /// Как настоящий поток: уничтоженный текущий выход теряет недоигранный буфер.
    fn drop(&mut self) {
        let mut state = self.device.lock();
        if state.current_output != self.output_id {
            return;
        }
        let unplayed = state.submitted_frames - state.played_frames;
        state.discarded_frames += unplayed;
        state.submitted_frames = state.played_frames;
    }
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
        state.written_frames_total += frames as u64;
        Ok(AudioOutputWriteReport::complete(
            AudioOutputInputFrameCount::new(frames),
            AudioOutputStreamFrameCount::new(frames),
        ))
    }

    fn play(&mut self) -> anyhow::Result<()> {
        let mut state = self.device.lock();
        state.playing = true;
        state.started = true;
        state.frozen_audible = None;
        Ok(())
    }

    fn pause_and_freeze_clock(&mut self) -> anyhow::Result<AudioOutputClockTiming> {
        let timing = self.device.timing();
        let mut state = self.device.lock();
        state.playing = false;
        state.frozen_audible = Some(timing.audible_output_position());
        Ok(timing)
    }

    fn clear_buffer_for_seek(&mut self, generation: u64) -> anyhow::Result<u64> {
        let mut state = self.device.lock();
        state.submitted_frames = state.played_frames;
        let written_frames_total = state.written_frames_total;
        state
            .written_frames_at_seek_clears
            .push(written_frames_total);
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
        state.frozen_audible = None;
    }

    fn underrun_callbacks(&self) -> u64 {
        0
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Открывает выход симулятора: 1000 Гц, моно (1 кадр = 1 мс).
    fn open_output(device: &SimulatedAudioDevice) -> Box<dyn PlayerAudioOutput> {
        let spec = AudioOutputSpec::new(1000, player_core::AudioChannelLayout::mono());
        device
            .factory()
            .create_output(spec, AudioOutputDeviceRequest::SystemDefault)
            .expect("выход симулятора")
            .output
    }

    /// Пишет `frames` кадров звука.
    fn write(output: &mut dyn PlayerAudioOutput, frames: usize) {
        output
            .write_samples(&vec![0.5; frames], AudioOutputWriteIntent::DirectDecodedPcm)
            .expect("запись");
    }

    /// Длинный шаг: карта прошла 5 мс из 20, половину её пути занял звук, половину тишина.
    /// Настоящая тишина — половина настоящего шага, а не те же 2,5 мс.
    #[test]
    fn real_time_silence_keeps_the_part_of_a_long_step_the_card_did_not_walk() {
        let device = SimulatedAudioDevice::default();
        let mut output = open_output(&device);
        output.play().unwrap();
        write(output.as_mut(), 500);
        device.advance(Duration::from_millis(497));
        device.advance_for_real_time(Duration::from_millis(6), Duration::from_millis(20));
        write(output.as_mut(), 1);

        let card_silence = device.mid_stream_silence();
        let real_silence = device.mid_stream_silence_real_time();
        assert!(
            card_silence.abs_diff(Duration::from_millis(3)) < Duration::from_micros(100),
            "по часам карты: {card_silence:?}"
        );
        assert!(
            real_silence.abs_diff(Duration::from_millis(10)) < Duration::from_micros(100),
            "по настоящим часам: {real_silence:?}"
        );
    }

    #[test]
    fn clear_on_pause_empties_level_and_playback_spends_new_audio() {
        let device = SimulatedAudioDevice::default();
        let mut output = open_output(&device);
        output.play().unwrap();
        write(output.as_mut(), 500);
        device.advance(Duration::from_millis(100));
        output.pause_and_freeze_clock().unwrap();
        assert!((output.buffer_level_ms() - 400.0).abs() < 1.0);

        output.clear_buffer_for_seek(1).unwrap();
        assert_eq!(
            output.buffer_level_ms(),
            0.0,
            "после clear на паузе буфер пуст"
        );

        write(output.as_mut(), 300);
        assert!((output.buffer_level_ms() - 300.0).abs() < 1.0);
        output.play().unwrap();
        device.advance(Duration::from_millis(200));
        assert!(
            (output.buffer_level_ms() - 100.0).abs() < 1.0,
            "звук тратится как прежде"
        );
    }
}
