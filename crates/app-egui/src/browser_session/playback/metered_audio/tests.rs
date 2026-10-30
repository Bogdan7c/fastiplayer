//! Счётчик звука: записанное и прозвучавшее идут через обёртку, отказ устройства виден.

use std::ffi::OsStr;
use std::sync::atomic::{AtomicU64, Ordering};

use player_core::{
    AudioChannelLayout, AudioOutputDeviceRoute, AudioOutputInputFrameCount,
    AudioOutputStreamFrameCount,
};

use super::*;

/// Часы фейкового выхода: «прозвучало» задаёт тест (миллисекунды).
#[derive(Default)]
struct ManualClock {
    audible_ms: AtomicU64,
}

impl PlayerAudioClock for ManualClock {
    fn now(&self) -> Duration {
        Duration::from_millis(self.audible_ms.load(Ordering::SeqCst))
    }

    fn output_timing(&self) -> AudioOutputClockTiming {
        AudioOutputClockTiming::new(self.now(), self.now())
    }

    fn reset(&self) {
        self.audible_ms.store(0, Ordering::SeqCst);
    }

    fn underrun_callbacks(&self) -> u64 {
        0
    }
}

/// Фейковый выход: принимает всё, часы — общие с тестом.
struct FakeOutput {
    clock: Arc<ManualClock>,
    channels: usize,
}

impl PlayerAudioOutput for FakeOutput {
    fn write_samples(
        &mut self,
        samples: &[f32],
        _intent: AudioOutputWriteIntent,
    ) -> Result<AudioOutputWriteReport, AudioOutputWriteError> {
        let frames = samples.len() / self.channels;
        Ok(AudioOutputWriteReport::complete(
            AudioOutputInputFrameCount::new(frames),
            AudioOutputStreamFrameCount::new(frames),
        ))
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
        0.0
    }

    fn clock(&self) -> Arc<dyn PlayerAudioClock> {
        self.clock.clone()
    }
}

/// Фабрика фейковых выходов (или всегда отказывающая — «устройства нет»).
struct FakeFactory {
    clock: Arc<ManualClock>,
    device_present: bool,
}

impl AudioOutputFactory for FakeFactory {
    fn create_output(
        &self,
        spec: AudioOutputSpec,
        _device_request: AudioOutputDeviceRequest,
    ) -> anyhow::Result<CreatedAudioOutput> {
        if !self.device_present {
            anyhow::bail!("no audio device");
        }
        Ok(CreatedAudioOutput::new(
            Box::new(FakeOutput {
                clock: self.clock.clone(),
                channels: usize::try_from(spec.channels()).expect("channels"),
            }),
            AudioOutputDeviceRoute::SystemDefault,
        ))
    }
}

/// Стерео 48 кГц.
fn stereo_spec() -> AudioOutputSpec {
    AudioOutputSpec::new(48_000, AudioChannelLayout::stereo())
}

/// Записанное считается по исходному PCM, прозвучавшее — по приросту часов выхода.
#[test]
fn written_and_played_audio_are_counted_through_the_wrapper() {
    let clock = Arc::new(ManualClock::default());
    let inner = Arc::new(FakeFactory {
        clock: clock.clone(),
        device_present: true,
    });
    let (factory, meter) = MeteredAudioOutputFactory::wrap(inner, AudioOutputStatus::Ok);
    assert_eq!(meter.reading().output, AudioOutputStatus::NotOpened);

    let mut output = factory
        .create_output(stereo_spec(), AudioOutputDeviceRequest::SelectedOnly)
        .expect("output opens")
        .output;
    // 0,5 с стерео 48 кГц = 24 000 кадров = 48 000 сэмплов.
    output
        .write_samples(&vec![0.0; 48_000], AudioOutputWriteIntent::DirectDecodedPcm)
        .expect("write");
    clock.audible_ms.store(300, Ordering::SeqCst);
    let first = meter.reading();
    clock.audible_ms.store(450, Ordering::SeqCst);
    let second = meter.reading();

    assert_eq!(first.written, Duration::from_millis(500));
    assert_eq!(first.played, Duration::from_millis(300));
    assert_eq!(second.played, Duration::from_millis(450));
    assert_eq!(second.output, AudioOutputStatus::Ok);
}

/// Сброс часов (перемотка) не вычитает прозвучавшее: счёт продолжается от нового нуля.
#[test]
fn clock_reset_keeps_played_audio_monotonic() {
    let clock = Arc::new(ManualClock::default());
    let inner = Arc::new(FakeFactory {
        clock: clock.clone(),
        device_present: true,
    });
    let (factory, meter) = MeteredAudioOutputFactory::wrap(inner, AudioOutputStatus::Simulated);
    let _output = factory
        .create_output(stereo_spec(), AudioOutputDeviceRequest::SelectedOnly)
        .expect("output opens");
    clock.audible_ms.store(1_000, Ordering::SeqCst);
    assert_eq!(meter.reading().played, Duration::from_millis(1_000));

    clock.reset();
    assert_eq!(meter.reading().played, Duration::from_millis(1_000));
    clock.audible_ms.store(200, Ordering::SeqCst);

    let reading = meter.reading();
    assert_eq!(reading.played, Duration::from_millis(1_200));
    assert_eq!(reading.output, AudioOutputStatus::Simulated);
}

/// Устройства нет: ошибка уходит плееру как есть, журнал видит `unavailable`.
#[test]
fn absent_device_is_reported_unavailable_and_error_passes_through() {
    let inner = Arc::new(FakeFactory {
        clock: Arc::new(ManualClock::default()),
        device_present: false,
    });
    let (factory, meter) = MeteredAudioOutputFactory::wrap(inner, AudioOutputStatus::Ok);

    let result = factory.create_output(stereo_spec(), AudioOutputDeviceRequest::SelectedOnly);

    let error = result.expect_err("the device error reaches the player");
    assert!(error.to_string().contains("no audio device"), "{error}");
    let reading = meter.reading();
    assert_eq!(reading.output, AudioOutputStatus::Unavailable);
    assert_eq!(reading.played, Duration::ZERO);
    assert_eq!(reading.output.log_label(), "unavailable");
}

/// Тестовая переменная: нет — настоящий звук, `simulated` — тихая карта, прочее — отказ.
#[test]
fn test_override_is_parsed_into_a_choice() {
    assert_eq!(
        BrowserAudioOutputChoice::from_test_override(None),
        Ok(BrowserAudioOutputChoice::SystemDevice)
    );
    assert_eq!(
        BrowserAudioOutputChoice::from_test_override(Some(OsStr::new("simulated"))),
        Ok(BrowserAudioOutputChoice::SimulatedRealTime)
    );
    assert_eq!(
        BrowserAudioOutputChoice::from_test_override(Some(OsStr::new("Simulated"))),
        Err(UnknownAudioOverride)
    );
    assert_eq!(
        BrowserAudioOutputChoice::from_test_override(Some(OsStr::new(""))),
        Err(UnknownAudioOverride)
    );
}

/// Тихая карта через сборщик: выход открывается, состояние `simulated`, звук считается.
#[test]
fn simulated_choice_builds_a_metered_silent_sink() {
    let (factory, meter) =
        BrowserAudioOutputChoice::SimulatedRealTime.build_metered_factory("ignored-device");
    let mut output = factory
        .create_output(stereo_spec(), AudioOutputDeviceRequest::SelectedOnly)
        .expect("silent sink opens")
        .output;
    output
        .write_samples(&vec![0.0; 9_600], AudioOutputWriteIntent::DirectDecodedPcm)
        .expect("write");

    let reading = meter.reading();
    assert_eq!(reading.output, AudioOutputStatus::Simulated);
    assert_eq!(reading.written, Duration::from_millis(100));
}
