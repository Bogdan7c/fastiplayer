//! Ритм CPAL output callback-а: когда callback последний раз забирал звук и сколько.
//!
//! Уровень буфера (`samples_written − samples_played`) меняется только внутри callback-а,
//! ступенькой на целый период. Планировщику подпитки в player-core нужен read-only факт
//! «когда случится следующая ступенька», иначе он просыпается между callback-ами и видит
//! тот же уровень (сессия cpu-parity 02b).
//!
//! Запись идёт из realtime callback-а, поэтому здесь только атомики: никаких блокировок,
//! аллокаций и системных вызовов, кроме уже взятого вызывающим `Instant`.

use std::sync::atomic::{AtomicU64, Ordering};
use std::time::Instant;

use audio_core::AudioOutputCallbackCadence;

use super::AudioClock;

/// Последний наблюдённый callback, который реально выводил звук.
///
/// Два поля пишутся без seqlock-а: «порванное» чтение (время нового callback-а с размером
/// старого) даёт ошибку не больше разницы периодов соседних callback-ов, а потребитель
/// использует факт только как подсказку момента пробуждения, не как часы A/V-синхрона.
pub(super) struct OutputCallbackCadenceRecorder {
    /// Момент начала последнего callback-а в наносекундах от `AudioClock::origin`.
    ///
    /// Это реальное (не «логическое» без пауз) время: возраст факта должен расти
    /// и во время паузы, чтобы потребитель видел, что ритм устарел.
    last_callback_origin_ns: AtomicU64,

    /// Сколько interleaved сэмплов (PCM + тишина) забрал последний callback.
    ///
    /// Ноль — callback ещё не наблюдался. Пишется последним с `Release`, поэтому
    /// читатель, увидевший ненулевой размер, видит и время того же или более нового callback-а.
    last_callback_output_samples: AtomicU64,
}

impl OutputCallbackCadenceRecorder {
    /// Пустой recorder: ритм ещё не наблюдался.
    pub(super) const fn new() -> Self {
        Self {
            last_callback_origin_ns: AtomicU64::new(0),
            last_callback_output_samples: AtomicU64::new(0),
        }
    }
}

impl AudioClock {
    /// Запоминает ритм одного output callback-а, который выводил звук.
    ///
    /// `callback_output_samples` — весь размер буфера callback-а в interleaved сэмплах
    /// (PCM плюс тишина underrun-а). Удержанные callback-и (пауза, ожидание первого `play`)
    /// сюда не попадают: они не забирают PCM и не двигают уровень буфера.
    pub(super) fn record_callback_cadence(
        &self,
        callback_output_samples: u64,
        callback_observed_at: Instant,
    ) {
        // Пустой буфер ничего не говорит о периоде: прежний факт остаётся в силе.
        if callback_output_samples == 0 {
            return;
        }

        let callback_origin_ns = self.origin_ns_for_instant(callback_observed_at);
        let recorder = &self.callback_cadence;
        recorder
            .last_callback_origin_ns
            .store(callback_origin_ns, Ordering::Relaxed);
        recorder
            .last_callback_output_samples
            .store(callback_output_samples, Ordering::Release);
    }

    /// Возвращает наблюдённый ритм callback-а для нейтрального контракта.
    pub fn callback_cadence(&self) -> AudioOutputCallbackCadence {
        let recorder = &self.callback_cadence;
        let callback_output_samples = recorder
            .last_callback_output_samples
            .load(Ordering::Acquire);
        if callback_output_samples == 0 {
            return AudioOutputCallbackCadence::NotObserved;
        }

        let callback_origin_ns = recorder.last_callback_origin_ns.load(Ordering::Relaxed);
        let since_last_callback_ns = self.current_origin_ns().saturating_sub(callback_origin_ns);

        AudioOutputCallbackCadence::Observed {
            since_last_callback: std::time::Duration::from_nanos(since_last_callback_ns),
            callback_period: self.samples_to_duration(callback_output_samples),
        }
    }

    /// Переводит `Instant` в реальные наносекунды от `origin` (паузы не вычитаются).
    fn origin_ns_for_instant(&self, instant: Instant) -> u64 {
        instant
            .saturating_duration_since(self.origin)
            .as_nanos()
            .min(u64::MAX as u128) as u64
    }
}

#[cfg(test)]
mod tests {
    use std::time::Duration;

    use audio_core::PlayerAudioClock;

    use super::*;

    /// 48 кГц стерео: 480 кадров = 960 interleaved сэмплов = 10 мс звука.
    const TEN_MS_STEREO_SAMPLES: u64 = 960;

    /// Возраст факта в тестах: заметно больше любого планирования между вызовами.
    const CALLBACK_AGE: Duration = Duration::from_millis(30);

    /// Достаёт поля наблюдения или падает с понятным сообщением.
    fn observed(cadence: AudioOutputCallbackCadence) -> (Duration, Duration) {
        match cadence {
            AudioOutputCallbackCadence::Observed {
                since_last_callback,
                callback_period,
            } => (since_last_callback, callback_period),
            AudioOutputCallbackCadence::NotObserved => {
                panic!("callback cadence must be observed after a playing callback")
            }
        }
    }

    #[test]
    fn fresh_clock_reports_not_observed() {
        let clock = AudioClock::new(48_000, 2);

        assert_eq!(
            clock.callback_cadence(),
            AudioOutputCallbackCadence::NotObserved
        );
    }

    #[test]
    fn playing_callback_publishes_period_and_growing_age() {
        let clock = AudioClock::new(48_000, 2);
        let callback_at = Instant::now();

        clock.record_callback_cadence(TEN_MS_STEREO_SAMPLES, callback_at);
        let (first_age, callback_period) = observed(clock.callback_cadence());
        std::thread::sleep(Duration::from_millis(2));
        let (later_age, _) = observed(clock.callback_cadence());

        assert_eq!(callback_period, Duration::from_millis(10));
        // Возраст считается от момента callback-а и растёт, пока новых callback-ов нет.
        assert!(later_age > first_age, "{first_age:?} -> {later_age:?}");
        assert!(later_age >= Duration::from_millis(2));
    }

    #[test]
    fn age_is_measured_from_callback_moment_not_from_record_call() {
        let clock = AudioClock::new(48_000, 2);
        std::thread::sleep(CALLBACK_AGE);
        let callback_at = Instant::now() - CALLBACK_AGE;

        clock.record_callback_cadence(TEN_MS_STEREO_SAMPLES, callback_at);
        let (since_last_callback, _) = observed(clock.callback_cadence());

        assert!(
            since_last_callback >= CALLBACK_AGE,
            "{since_last_callback:?}"
        );
    }

    #[test]
    fn latest_callback_replaces_previous_period() {
        let clock = AudioClock::new(48_000, 1);
        let callback_at = Instant::now();

        clock.record_callback_cadence(480, callback_at);
        clock.record_callback_cadence(1_024, callback_at);
        let (_, callback_period) = observed(clock.callback_cadence());

        // Mono: 1024 сэмпла = 1024 кадра при 48 кГц.
        assert_eq!(callback_period, Duration::from_nanos(21_333_333));
    }

    #[test]
    fn empty_callback_buffer_keeps_previous_fact() {
        let clock = AudioClock::new(48_000, 2);
        let callback_at = Instant::now();
        clock.record_callback_cadence(TEN_MS_STEREO_SAMPLES, callback_at);

        clock.record_callback_cadence(0, Instant::now());
        let (_, callback_period) = observed(clock.callback_cadence());

        assert_eq!(callback_period, Duration::from_millis(10));
    }

    #[test]
    fn seek_reset_keeps_cadence_because_callbacks_keep_running() {
        let clock = AudioClock::new(48_000, 2);
        clock.record_callback_cadence(TEN_MS_STEREO_SAMPLES, Instant::now());

        clock.reset();

        assert!(matches!(
            clock.callback_cadence(),
            AudioOutputCallbackCadence::Observed { .. }
        ));
    }

    #[test]
    fn cadence_reaches_player_through_neutral_clock_contract() {
        let clock = std::sync::Arc::new(AudioClock::new(48_000, 2));
        clock.record_callback_cadence(TEN_MS_STEREO_SAMPLES, Instant::now());
        let neutral_clock: std::sync::Arc<dyn PlayerAudioClock> = clock;

        // Дефолт трейта (`NotObserved`) здесь был бы дырой: player остался бы на старой формуле.
        let (_, callback_period) = observed(neutral_clock.callback_cadence());

        assert_eq!(callback_period, Duration::from_millis(10));
    }

    #[test]
    fn recording_cadence_does_not_touch_buffer_accounting() {
        let clock = AudioClock::new(48_000, 2);
        clock.record_written(4_800);

        clock.record_callback_cadence(TEN_MS_STEREO_SAMPLES, Instant::now());

        assert_eq!(clock.buffer_level(), 4_800);
        assert_eq!(clock.underrun_callbacks(), 0);
    }
}
