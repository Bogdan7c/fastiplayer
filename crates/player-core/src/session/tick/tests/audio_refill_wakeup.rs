//! Планирование audio refill wakeup-а, пока CPAL callback параллельно расходует буфер.
//!
//! Регрессия бэклога AC-3 #19: на сыром E-AC-3 7.1 (кадры по 256 сэмплов) уровень
//! буфера после refill держится в пределах одного кадра над high-water, поэтому
//! callback почти каждый раз пересекает порог. Planner читал уровень несколько раз
//! за один проход: «есть ли работа прямо сейчас» видел значение чуть выше порога,
//! а «когда будить для refill» — уже чуть ниже. Оба решения отвечали «нет», и worker
//! уходил в резервный coarse wakeup 250 мс при буфере 200 мс → слышимая дыра.

use std::sync::Mutex;

use audio_core::AudioOutputCallbackCadence;

use super::*;

/// High-water mark тестов: как production default `audio.buffer_target_ms`.
const HIGH_WATER_MARK_MS: f64 = 200.0;

/// Резервный coarse interval worker-а (production default — 250 мс).
const COARSE_WAKEUP_INTERVAL: Duration = Duration::from_millis(250);

/// Poll interval decoder readiness; без видео он не участвует, но planner его требует.
const DECODER_READINESS_POLL_INTERVAL: Duration = Duration::from_millis(2);

/// Верхняя оценка числа чтений уровня за один проход planner-а (с запасом).
const MAX_PLANNER_LEVEL_READS: usize = 12;

/// Fake output, чей уровень убывает между чтениями, как при работающем callback-е.
///
/// Каждый вызов `buffer_level_ms` отдаёт следующее значение сценария; последнее
/// значение остаётся «залипшим». Так детерминированно воспроизводится момент, когда
/// callback забирает период ровно между двумя чтениями уровня внутри planner-а.
struct CallbackDrainedAudioOutput {
    /// Clock output boundary; по умолчанию без ритма callback-а.
    clock: Arc<dyn PlayerAudioClock>,

    /// Оставшиеся уровни буфера (мс) в порядке чтения.
    scripted_levels_ms: Mutex<VecDeque<f64>>,

    /// Сколько раз planner прочитал уровень: доказывает, что чтение было.
    level_reads: Arc<AtomicUsize>,
}

impl CallbackDrainedAudioOutput {
    /// Создаёт output со сценарием уровней и общим счётчиком чтений.
    fn new(scripted_levels_ms: &[f64], level_reads: Arc<AtomicUsize>) -> Self {
        Self {
            clock: Arc::new(FixedAudioClock),
            scripted_levels_ms: Mutex::new(scripted_levels_ms.iter().copied().collect()),
            level_reads,
        }
    }

    /// Тот же output, но его clock сообщает ритм callback-а звуковой карты.
    fn with_callback_cadence(
        scripted_levels_ms: &[f64],
        callback_cadence: AudioOutputCallbackCadence,
    ) -> Self {
        Self {
            clock: Arc::new(CadenceReportingAudioClock { callback_cadence }),
            ..Self::new(scripted_levels_ms, Arc::new(AtomicUsize::new(0)))
        }
    }
}

/// Fake clock, который сообщает заданный ритм output callback-а.
struct CadenceReportingAudioClock {
    /// Что «наблюдал» callback: возраст последнего вызова и период.
    callback_cadence: AudioOutputCallbackCadence,
}

impl PlayerAudioClock for CadenceReportingAudioClock {
    /// Позиция clock-а в planner-тестах не участвует.
    fn now(&self) -> Duration {
        Duration::ZERO
    }

    /// Хвост output-а planner refill-а не читает.
    fn output_timing(&self) -> AudioOutputClockTiming {
        AudioOutputClockTiming::new(Duration::ZERO, Duration::ZERO)
    }

    /// Reset не влияет на ритм.
    fn reset(&self) {}

    /// Underrun-ы в planner-тестах не моделируются.
    fn underrun_callbacks(&self) -> u64 {
        0
    }

    /// Отдаёт заданный тестом ритм.
    fn callback_cadence(&self) -> AudioOutputCallbackCadence {
        self.callback_cadence
    }
}

impl PlayerAudioOutput for CallbackDrainedAudioOutput {
    /// Planner-тесты не пишут PCM; запись принимается целиком.
    fn write_samples(
        &mut self,
        samples: &[f32],
        _intent: AudioOutputWriteIntent,
    ) -> std::result::Result<AudioOutputWriteReport, AudioOutputWriteError> {
        Ok(AudioOutputWriteReport::complete(
            AudioOutputInputFrameCount::new(samples.len()),
            AudioOutputStreamFrameCount::new(samples.len()),
        ))
    }

    /// Запуск stream-а не влияет на planner.
    fn play(&mut self) -> anyhow::Result<()> {
        Ok(())
    }

    /// Pause возвращает нейтральный snapshot fake clock-а.
    fn pause_and_freeze_clock(&mut self) -> anyhow::Result<AudioOutputClockTiming> {
        Ok(self.clock.output_timing())
    }

    /// Clear подтверждает запрошенный generation.
    fn clear_buffer_for_seek(&mut self, generation: u64) -> anyhow::Result<u64> {
        Ok(generation)
    }

    /// Громкость не участвует в планировании.
    fn set_volume(&mut self, _volume: f32) {}

    /// Отдаёт следующий уровень сценария («callback» успел забрать период).
    fn buffer_level_ms(&self) -> f64 {
        self.level_reads.fetch_add(1, Ordering::Relaxed);
        let mut scripted_levels_ms = self
            .scripted_levels_ms
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        if scripted_levels_ms.len() > 1 {
            scripted_levels_ms.pop_front().unwrap_or_default()
        } else {
            scripted_levels_ms.front().copied().unwrap_or_default()
        }
    }

    /// Fake clock для audio output boundary.
    fn clock(&self) -> Arc<dyn PlayerAudioClock> {
        Arc::clone(&self.clock)
    }
}

/// Config с известным high-water mark.
fn refill_config() -> PlayerTickConfig {
    PlayerTickConfig {
        audio_buffer_high_water_mark_ms: HIGH_WATER_MARK_MS,
        ..PlayerTickConfig::default()
    }
}

/// Audio-only Playing session с одним pending packet-ом и scripted output-ом.
fn playing_audio_only_session(
    scripted_levels_ms: &[f64],
    level_reads: Arc<AtomicUsize>,
) -> PlayerSession {
    playing_audio_only_session_with_output(CallbackDrainedAudioOutput::new(
        scripted_levels_ms,
        level_reads,
    ))
}

/// Audio-only Playing session с одним pending packet-ом и заданным output-ом.
fn playing_audio_only_session_with_output(output: CallbackDrainedAudioOutput) -> PlayerSession {
    let mut session = PlayerSession::new();
    let audio_track_id = TrackId::new(2);
    install_empty_demuxer(&mut session);
    session
        .dispatch_command(PlayerCommand::Play)
        .expect("Play is accepted for installed media");
    session.pipeline.select_audio_track(audio_track_id);
    session
        .pipeline
        .install_audio_output_for_tests(Box::new(output));
    enqueue_pending_audio_packet(&mut session, audio_track_id);
    session
}

/// Строит wakeup plan с production-like интервалами.
fn plan_wakeup(session: &PlayerSession) -> PlayerWorkerWakeupPlan {
    session.worker_wakeup_plan(
        Instant::now(),
        &refill_config(),
        DECODER_READINESS_POLL_INTERVAL,
        COARSE_WAKEUP_INTERVAL,
    )
}

/// Callback пересёк high-water между чтениями: worker не должен уснуть на coarse 250 мс.
///
/// Planner читает уровень несколько раз за проход, а callback может забрать период
/// между любыми двумя чтениями. Тест перебирает все точки пересечения порога (первые
/// `reads_above_high_water` чтений видят 200.5 мс, остальные — 199.5 мс). До исправления
/// одна из точек (между проверкой «есть ли работа сейчас» и расчётом refill deadline-а)
/// давала `CoarseProgress` 250 мс — дольше, чем весь 200 мс буфер.
#[test]
fn callback_crossing_high_water_mid_planning_does_not_fall_back_to_coarse_wakeup() {
    let level_above_ms = HIGH_WATER_MARK_MS + 0.5;
    let level_below_ms = HIGH_WATER_MARK_MS - 0.5;

    for reads_above_high_water in 0..=MAX_PLANNER_LEVEL_READS {
        let level_reads = Arc::new(AtomicUsize::new(0));
        let mut scripted_levels_ms = vec![level_above_ms; reads_above_high_water];
        scripted_levels_ms.push(level_below_ms);
        let session = playing_audio_only_session(&scripted_levels_ms, Arc::clone(&level_reads));

        let plan = plan_wakeup(&session);

        assert!(level_reads.load(Ordering::Relaxed) >= 1);
        assert_eq!(
            plan.reason,
            WorkerWakeupReason::PipelineWorkReady,
            "crossing after {reads_above_high_water} reads: {plan:?}"
        );
        let planned_delay = plan
            .delay
            .expect("audio refill must have a bounded deadline");
        // Worker обязан проснуться раньше, чем закончится наблюдённый запас звука.
        assert!(
            planned_delay < Duration::from_secs_f64(level_above_ms / 1000.0),
            "crossing after {reads_above_high_water} reads: sleep {planned_delay:?} \
             outlives the {level_above_ms} ms audio buffer"
        );
    }
}

/// Обратный порядок (уровень уже ниже порога) даёт немедленный refill.
#[test]
fn level_at_or_below_high_water_with_pending_audio_wakes_immediately() {
    let level_reads = Arc::new(AtomicUsize::new(0));
    let session = playing_audio_only_session(
        &[HIGH_WATER_MARK_MS - 0.5, HIGH_WATER_MARK_MS - 30.0],
        level_reads,
    );

    let plan = plan_wakeup(&session);

    assert_eq!(plan.reason, WorkerWakeupReason::PipelineWorkReady);
    assert_eq!(plan.delay, Some(Duration::ZERO));
}

/// Стабильный уровень выше порога: deadline = уровень − high-water (прежняя семантика).
#[test]
fn stable_level_above_high_water_schedules_refill_at_threshold_crossing() {
    let level_reads = Arc::new(AtomicUsize::new(0));
    let session = playing_audio_only_session(&[HIGH_WATER_MARK_MS + 40.0], level_reads);

    let plan = plan_wakeup(&session);

    assert_eq!(plan.reason, WorkerWakeupReason::PipelineWorkReady);
    assert_eq!(plan.delay, Some(Duration::from_millis(40)));
}

/// Без audio output (уровня нет) pending packet остаётся немедленной работой.
#[test]
fn absent_audio_output_keeps_pending_audio_as_immediate_work() {
    let mut session = PlayerSession::new();
    let audio_track_id = TrackId::new(2);
    install_empty_demuxer(&mut session);
    session
        .dispatch_command(PlayerCommand::Play)
        .expect("Play is accepted for installed media");
    session.pipeline.select_audio_track(audio_track_id);
    enqueue_pending_audio_packet(&mut session, audio_track_id);

    let plan = plan_wakeup(&session);

    assert_eq!(plan.delay, Some(Duration::ZERO));
}

/// Planner только читает: pending audio и output остаются нетронутыми.
#[test]
fn refill_planning_does_not_consume_pending_audio() {
    let level_reads = Arc::new(AtomicUsize::new(0));
    let session = playing_audio_only_session(
        &[HIGH_WATER_MARK_MS + 0.5, HIGH_WATER_MARK_MS - 0.5],
        level_reads,
    );

    let _plan = plan_wakeup(&session);

    assert_eq!(session.pipeline.pending_audio_packet_len(), 1);
    assert!(session.pipeline.has_audio_output());
}

/// Ритм callback-а известен: planner будит после ступеньки, а не через `уровень − high-water`.
///
/// Уровень 210 мс, callback забирает 40 мс, последний был 5 мс назад. Прежняя формула
/// дала бы 10 мс — за 25 мс до callback-а, когда уровень ещё тот же (пустое пробуждение).
#[test]
fn observed_callback_cadence_schedules_refill_after_the_draining_callback() {
    let session =
        playing_audio_only_session_with_output(CallbackDrainedAudioOutput::with_callback_cadence(
            &[HIGH_WATER_MARK_MS + 10.0],
            AudioOutputCallbackCadence::Observed {
                since_last_callback: Duration::from_millis(5),
                callback_period: Duration::from_millis(40),
            },
        ));

    let plan = plan_wakeup(&session);

    assert_eq!(plan.reason, WorkerWakeupReason::PipelineWorkReady);
    // 35 мс до callback-а + 5 мс (1/8 периода) запаса на дрожание.
    assert_eq!(plan.delay, Some(Duration::from_millis(40)));
}

/// Ритм устарел (callback-и не идут): остаётся прежний дедлайн по уровню.
#[test]
fn stale_callback_cadence_keeps_level_based_refill_deadline() {
    let session =
        playing_audio_only_session_with_output(CallbackDrainedAudioOutput::with_callback_cadence(
            &[HIGH_WATER_MARK_MS + 10.0],
            AudioOutputCallbackCadence::Observed {
                since_last_callback: Duration::from_millis(500),
                callback_period: Duration::from_millis(40),
            },
        ));

    let plan = plan_wakeup(&session);

    assert_eq!(plan.reason, WorkerWakeupReason::PipelineWorkReady);
    assert_eq!(plan.delay, Some(Duration::from_millis(10)));
}

/// Ритм не отменяет немедленный refill: уровень уже у порога — будим сразу.
#[test]
fn observed_callback_cadence_does_not_delay_refill_below_high_water() {
    let session =
        playing_audio_only_session_with_output(CallbackDrainedAudioOutput::with_callback_cadence(
            &[HIGH_WATER_MARK_MS - 1.0],
            AudioOutputCallbackCadence::Observed {
                since_last_callback: Duration::from_millis(5),
                callback_period: Duration::from_millis(40),
            },
        ));

    let plan = plan_wakeup(&session);

    assert_eq!(plan.delay, Some(Duration::ZERO));
    assert_eq!(session.pipeline.pending_audio_packet_len(), 1);
}
