//! Вертикальный сценарий: пробуждения worker-а для audio refill (cpu-parity 02b).
//!
//! Настоящий worker wait loop и настоящий tick доливают звук в fake output, у которого
//! уровень падает так же, как у CPAL: ступенькой целого периода раз в период. Проверяется
//! результат для пользователя (звук не прерывается, буфер держится у high-water) и цена
//! (сколько раз worker просыпался ради подпитки).

use std::sync::atomic::{AtomicU64, AtomicUsize, Ordering};

use audio_core::{
    AudioChannelLayout, AudioDecoder, AudioOutputCallbackCadence, AudioOutputClockTiming,
    AudioOutputInputFrameCount, AudioOutputStreamFrameCount, AudioOutputWriteError,
    AudioOutputWriteIntent, AudioOutputWriteReport, EncodedAudioPacket, PlayerAudioClock,
    PlayerAudioOutput,
};

use super::*;

/// Частота PCM сценария.
const SAMPLE_RATE_HZ: u32 = 48_000;

/// Стерео.
const CHANNEL_COUNT: u32 = 2;

/// Сколько звука даёт один packet (как короткий кадр кодека).
const PACKET_DURATION: Duration = Duration::from_millis(10);

/// Период callback-а: больше packet-а, как у PipeWire-кванта на 48 кГц.
///
/// Именно при «крупном» callback-е прежняя формула `уровень − high-water` будила
/// worker несколько раз до каждой ступеньки.
const CALLBACK_PERIOD: Duration = Duration::from_millis(40);

/// Сколько packet-ов в очереди: заведомо больше, чем успеет уйти за прогон.
const QUEUED_PACKETS: u32 = 120;

/// Длительность прогона реального времени (12 callback-ов).
const RUN_TIME: Duration = Duration::from_millis(480);

/// Сколько callback-ов случится за прогон.
const CALLBACKS_IN_RUN: u64 = 12;

/// Допуск на старт: до первого callback-а ритм ещё неизвестен, и действует прежняя
/// формула по уровню (на этом сценарии ~4 пробуждения).
const STARTUP_LEVEL_BASED_WAKEUPS: u64 = 6;

/// Общий журнал fake output-а: сколько записано и сколько «забрали» callback-и.
struct SteppedDrainLedger {
    /// Момент, от которого идут callback-и (первый — через период).
    started_at: Instant,

    /// Сколько наносекунд звука записал worker.
    written_ns: AtomicU64,

    /// Сколько раз уровень оказался исчерпан после начала записи (дыра в звуке).
    starved_level_reads: AtomicUsize,
}

impl SteppedDrainLedger {
    /// Сколько полных callback-ов прошло к моменту `now`.
    fn callbacks_elapsed(&self, now: Instant) -> u32 {
        let elapsed = now.saturating_duration_since(self.started_at);
        u32::try_from(elapsed.as_nanos() / CALLBACK_PERIOD.as_nanos()).unwrap_or(u32::MAX)
    }

    /// Сколько звука забрали callback-и: ступенька целого периода на каждый callback.
    fn drained(&self, now: Instant) -> Duration {
        CALLBACK_PERIOD * self.callbacks_elapsed(now)
    }

    /// Записанный worker-ом звук.
    fn written(&self) -> Duration {
        Duration::from_nanos(self.written_ns.load(Ordering::Relaxed))
    }

    /// Текущий уровень буфера; отмечает дыру, если записанного не хватило callback-ам.
    fn level(&self, now: Instant) -> Duration {
        let written = self.written();
        let drained = self.drained(now);
        if !written.is_zero() && drained >= written {
            self.starved_level_reads.fetch_add(1, Ordering::Relaxed);
        }
        written.saturating_sub(drained)
    }
}

/// Output, чей уровень падает ступеньками по реальному времени.
struct SteppedDrainAudioOutput {
    /// Общий журнал с тестом.
    ledger: Arc<SteppedDrainLedger>,

    /// Clock с ритмом callback-а (или без него).
    clock: Arc<dyn PlayerAudioClock>,
}

impl PlayerAudioOutput for SteppedDrainAudioOutput {
    /// Принимает PCM целиком и учитывает его длительность.
    fn write_samples(
        &mut self,
        samples: &[f32],
        _intent: AudioOutputWriteIntent,
    ) -> std::result::Result<AudioOutputWriteReport, AudioOutputWriteError> {
        let frames = samples.len() / CHANNEL_COUNT as usize;
        let written = Duration::from_secs_f64(frames as f64 / f64::from(SAMPLE_RATE_HZ));
        self.ledger.written_ns.fetch_add(
            u64::try_from(written.as_nanos()).unwrap_or(u64::MAX),
            Ordering::Relaxed,
        );
        Ok(AudioOutputWriteReport::complete(
            AudioOutputInputFrameCount::new(frames),
            AudioOutputStreamFrameCount::new(frames),
        ))
    }

    /// Callback-и идут с момента создания журнала.
    fn play(&mut self) -> anyhow::Result<()> {
        Ok(())
    }

    /// Пауза в сценарии не используется.
    fn pause_and_freeze_clock(&mut self) -> anyhow::Result<AudioOutputClockTiming> {
        Ok(self.clock.output_timing())
    }

    /// Seek в сценарии не используется.
    fn clear_buffer_for_seek(&mut self, generation: u64) -> anyhow::Result<u64> {
        Ok(generation)
    }

    /// Громкость не участвует.
    fn set_volume(&mut self, _volume: f32) {}

    /// Уровень по журналу на текущий момент.
    fn buffer_level_ms(&self) -> f64 {
        self.ledger.level(Instant::now()).as_secs_f64() * 1000.0
    }

    /// Clock сценария.
    fn clock(&self) -> Arc<dyn PlayerAudioClock> {
        Arc::clone(&self.clock)
    }
}

/// Clock, который знает ритм callback-а из журнала (как `audio::AudioClock`).
struct SteppedDrainClock {
    /// Журнал с моментом старта callback-ов.
    ledger: Arc<SteppedDrainLedger>,

    /// Сообщать ли ритм (`false` — backend без этого знания).
    reports_cadence: bool,
}

impl PlayerAudioClock for SteppedDrainClock {
    /// Позиция не участвует в планировании подпитки.
    fn now(&self) -> Duration {
        Duration::ZERO
    }

    /// Хвост output-а в сценарии не моделируется.
    fn output_timing(&self) -> AudioOutputClockTiming {
        AudioOutputClockTiming::new(Duration::ZERO, Duration::ZERO)
    }

    /// Reset не меняет ритм.
    fn reset(&self) {}

    /// Дыры считает журнал, а не clock.
    fn underrun_callbacks(&self) -> u64 {
        0
    }

    /// Возраст последнего callback-а и период, как их видел бы CPAL callback.
    fn callback_cadence(&self) -> AudioOutputCallbackCadence {
        let now = Instant::now();
        let callbacks_elapsed = self.ledger.callbacks_elapsed(now);
        if !self.reports_cadence || callbacks_elapsed == 0 {
            return AudioOutputCallbackCadence::NotObserved;
        }
        let last_callback_at = self.ledger.started_at + CALLBACK_PERIOD * callbacks_elapsed;
        AudioOutputCallbackCadence::Observed {
            since_last_callback: now.saturating_duration_since(last_callback_at),
            callback_period: CALLBACK_PERIOD,
        }
    }
}

/// Decoder: каждый packet даёт `PACKET_DURATION` тишины стерео 48 кГц.
struct FixedLengthPcmDecoder;

impl AudioDecoder for FixedLengthPcmDecoder {
    fn decode(&mut self, _packet: &EncodedAudioPacket<'_>) -> anyhow::Result<Vec<f32>> {
        let frames = SAMPLE_RATE_HZ as usize * PACKET_DURATION.as_millis() as usize / 1000;
        Ok(vec![0.0; frames * CHANNEL_COUNT as usize])
    }

    fn reset(&mut self) -> anyhow::Result<()> {
        Ok(())
    }

    fn sample_rate(&self) -> u32 {
        SAMPLE_RATE_HZ
    }

    fn channels(&self) -> u32 {
        CHANNEL_COUNT
    }

    fn channel_layout(&self) -> Option<AudioChannelLayout> {
        Some(AudioChannelLayout::stereo())
    }
}

/// Итог прогона: счётчики пробуждений и что стало со звуком.
struct AudioRefillRun {
    /// Оконные счётчики пробуждений worker-а.
    window: super::super::wakeup_counters::WorkerWakeupWindow,

    /// Журнал fake output-а после прогона.
    ledger: Arc<SteppedDrainLedger>,

    /// Момент окончания прогона.
    finished_at: Instant,
}

/// Гоняет настоящий worker wait loop над доигрыванием звука.
fn run_audio_refill_worker(clock_reports_cadence: bool) -> AudioRefillRun {
    let started_at = Instant::now();
    // Handles держим живыми: drop shutdown sender-а worker воспринял бы как shutdown.
    let (mut runtime, _command_tx, _shutdown_tx, _render_client) =
        runtime_for_tests_with_wakeup_handles(started_at);
    let ledger = Arc::new(SteppedDrainLedger {
        started_at,
        written_ns: AtomicU64::new(0),
        starved_level_reads: AtomicUsize::new(0),
    });
    let clock = Arc::new(SteppedDrainClock {
        ledger: Arc::clone(&ledger),
        reports_cadence: clock_reports_cadence,
    });
    runtime.session.install_audio_eof_drain_playback_for_tests(
        Box::new(FixedLengthPcmDecoder),
        Box::new(SteppedDrainAudioOutput {
            ledger: Arc::clone(&ledger),
            clock,
        }),
        QUEUED_PACKETS,
        PACKET_DURATION,
    );
    runtime.wakeup_counters = WorkerWakeupCounters::new(started_at);

    while started_at.elapsed() < RUN_TIME {
        let shutdown_requested = runtime.wait_for_worker_wakeup();
        assert!(!shutdown_requested);
    }

    let finished_at = Instant::now();
    AudioRefillRun {
        window: runtime.wakeup_counters.take_window(finished_at),
        ledger,
        finished_at,
    }
}

/// Звук не прерывается и буфер держится у high-water: подпитка реально работает.
fn assert_audio_kept_flowing(run: &AudioRefillRun) {
    assert_eq!(
        run.ledger.starved_level_reads.load(Ordering::Relaxed),
        0,
        "audio buffer ran dry during playback"
    );
    let final_level = run.ledger.level(run.finished_at);
    // High-water 200 мс минус один callback: ниже — значит подпитка отстаёт.
    assert!(
        final_level >= Duration::from_millis(150),
        "refill fell behind: level {final_level:?}, written {:?}",
        run.ledger.written()
    );
}

#[test]
fn audio_refill_wakes_once_per_draining_callback() {
    let run = run_audio_refill_worker(true);

    assert_audio_kept_flowing(&run);
    let refill_ticks = run
        .window
        .ticks_for_reason(crate::WorkerWakeupReason::PipelineWorkReady);
    // Одна подпитка на callback плюс старт без известного ритма; прежняя формула
    // будила ~4 раза на callback (уровень выше порога на 10 мс, ступенька через 40 мс).
    assert!(
        refill_ticks <= CALLBACKS_IN_RUN + STARTUP_LEVEL_BASED_WAKEUPS,
        "audio refill must wake about once per callback, got {refill_ticks} \
         for {CALLBACKS_IN_RUN} callbacks ({})",
        run.window.reasons_for_log()
    );
    assert!(
        refill_ticks >= CALLBACKS_IN_RUN / 2,
        "refill must still happen on callbacks, got {refill_ticks} ({})",
        run.window.reasons_for_log()
    );
}

#[test]
fn audio_refill_without_callback_cadence_keeps_level_based_wakeups() {
    let run = run_audio_refill_worker(false);

    // Прежняя формула: звук так же не прерывается, но пробуждений заметно больше.
    assert_audio_kept_flowing(&run);
    let refill_ticks = run
        .window
        .ticks_for_reason(crate::WorkerWakeupReason::PipelineWorkReady);
    assert!(
        refill_ticks >= CALLBACKS_IN_RUN * 2,
        "level-based fallback is expected to wake several times per callback, \
         got {refill_ticks} ({})",
        run.window.reasons_for_log()
    );
}
