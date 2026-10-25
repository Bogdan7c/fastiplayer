//! Unit-тесты монитора на фальшивом времени: проверяем итоговое решение (событие или нет).

use std::time::{Duration, Instant};

use super::*;

/// Шаг tick-а в тестах (около 60 Гц).
const TICK: Duration = Duration::from_millis(50);

/// Кадровый интервал 25 fps.
const FRAME_INTERVAL: Duration = Duration::from_millis(40);

/// Профиль поведения декодера за tick.
#[derive(Clone, Copy)]
struct Behaviour {
    starvation: PresentQueueStarvation,
    input: DecoderInputWork,
    output: OutputPressure,
    late: u64,
    audio: AudioClockActivity,
}

impl Behaviour {
    /// Декодер успевает: кадры есть.
    const HEALTHY: Self = Self {
        starvation: PresentQueueStarvation::Fed,
        input: DecoderInputWork::Pending,
        output: OutputPressure::Clear,
        late: 0,
        audio: AudioClockActivity::Running,
    };
    /// Декодер отстаёт: очередь пуста при наличии работы.
    const LAGGING: Self = Self {
        starvation: PresentQueueStarvation::Starved,
        ..Self::HEALTHY
    };
}

/// Стенд с фальшивыми часами.
struct Harness {
    monitor: DecoderPaceMonitor,
    thresholds: DecoderPaceThresholds,
    clock: Instant,
    state: PlaybackState,
    seek: SeekActivity,
    instance: Option<MediaInstanceId>,
    generation: u64,
    rate: PlaybackRate,
}

impl Harness {
    fn new() -> Self {
        Self {
            monitor: DecoderPaceMonitor::default(),
            thresholds: DecoderPaceThresholds::default(),
            clock: Instant::now(),
            state: PlaybackState::Playing,
            seek: SeekActivity::Idle,
            instance: instance_id(1),
            generation: 1,
            rate: PlaybackRate::NORMAL,
        }
    }

    /// Один tick с заданным поведением; возвращает событие, если оно сработало.
    fn tick(&mut self, behaviour: Behaviour) -> Option<DecoderPaceEvidence> {
        self.clock += TICK;
        self.monitor.observe(
            DecoderPaceObservation {
                now: self.clock,
                playback_state: self.state,
                seek_activity: self.seek,
                media_instance_id: self.instance,
                render_generation: self.generation,
                playback_rate: self.rate,
                frame_interval: FRAME_INTERVAL,
                late_drops_this_tick: behaviour.late,
                starvation: behaviour.starvation,
                decoder_input: behaviour.input,
                output_pressure: behaviour.output,
                audio_clock: behaviour.audio,
            },
            &self.thresholds,
        )
    }

    /// Гоняет tick-и заданное время и возвращает все события.
    fn run(&mut self, behaviour: Behaviour, duration: Duration) -> Vec<DecoderPaceEvidence> {
        let ticks = duration.as_millis() / TICK.as_millis();
        (0..ticks).filter_map(|_| self.tick(behaviour)).collect()
    }
}

/// Тестовый media instance с явным ненулевым номером.
fn instance_id(raw: u64) -> Option<MediaInstanceId> {
    std::num::NonZeroU64::new(raw).map(MediaInstanceId::from_non_zero)
}

const SECOND: Duration = Duration::from_secs(1);

#[test]
fn sustained_lag_fires_exactly_one_event_after_quiet_and_window() {
    let mut harness = Harness::new();

    // Тишина (2 с) + окно (4 с) с запасом: событие ровно одно.
    let events = harness.run(Behaviour::LAGGING, SECOND * 6 + TICK * 2);

    assert_eq!(events.len(), 1);
    let evidence = events[0];
    assert!(evidence.missed_frame_ratio >= 0.99, "{evidence:?}");
    assert_eq!(evidence.render_generation, 1);
    assert_eq!(evidence.media_instance_id, harness.instance);
    assert!(evidence.expected_frames > 90.0, "{evidence:?}");
}

#[test]
fn no_event_during_quiet_period() {
    let mut harness = Harness::new();

    let events = harness.run(Behaviour::LAGGING, SECOND * 2 + TICK * 4);

    assert!(events.is_empty());
}

#[test]
fn burst_inside_window_followed_by_healthy_playback_does_not_fire() {
    let mut harness = Harness::new();
    harness.run(Behaviour::HEALTHY, SECOND * 2);

    // 0.2 с отставания из 4 с окна = 5% < 10%.
    let mut events = harness.run(Behaviour::LAGGING, Duration::from_millis(200));
    events.extend(harness.run(Behaviour::HEALTHY, SECOND * 8));

    assert!(events.is_empty());
}

#[test]
fn late_drops_alone_fire_with_exact_count() {
    let mut harness = Harness::new();
    let dropping = Behaviour {
        late: 1,
        ..Behaviour::HEALTHY
    };

    let events = harness.run(dropping, SECOND * 6 + TICK * 2);

    assert_eq!(events.len(), 1);
    // 1 late на tick 50 мс = 20 в секунду против 25 ожидаемых.
    assert!(events[0].late_frames >= 70, "{:?}", events[0]);
    assert_eq!(events[0].starved_frames, 0.0);
}

#[test]
fn starvation_without_decoder_input_work_is_network_not_decoder() {
    let mut harness = Harness::new();
    let network_starved = Behaviour {
        input: DecoderInputWork::Absent,
        ..Behaviour::LAGGING
    };

    assert!(harness.run(network_starved, SECOND * 12).is_empty());
}

#[test]
fn starvation_under_output_backpressure_is_consumer_not_decoder() {
    let mut harness = Harness::new();
    let blocked = Behaviour {
        output: OutputPressure::Blocked,
        ..Behaviour::LAGGING
    };

    assert!(harness.run(blocked, SECOND * 12).is_empty());
}

/// Общий сценарий: за 3 с накоплено половина окна отставания, затем срабатывает «сброс».
fn assert_reset_mid_window_prevents_event(reset: impl FnOnce(&mut Harness)) {
    let mut harness = Harness::new();
    let mut events = harness.run(Behaviour::LAGGING, SECOND * 2 + SECOND * 3);
    reset(&mut harness);
    // Дальше здоровая игра: накопленное не должно «доехать» до порога.
    events.extend(harness.run(Behaviour::HEALTHY, SECOND * 10));
    assert!(events.is_empty());
}

#[test]
fn seek_mid_window_resets_accumulation() {
    assert_reset_mid_window_prevents_event(|harness| {
        harness.seek = SeekActivity::Active;
        harness.tick(Behaviour::LAGGING);
        harness.seek = SeekActivity::Idle;
    });
}

#[test]
fn generation_change_mid_window_resets_accumulation() {
    assert_reset_mid_window_prevents_event(|harness| harness.generation += 1);
}

#[test]
fn media_instance_change_mid_window_resets_accumulation() {
    assert_reset_mid_window_prevents_event(|harness| {
        harness.instance = instance_id(2);
    });
}

#[test]
fn rate_change_mid_window_resets_accumulation() {
    assert_reset_mid_window_prevents_event(|harness| {
        harness.rate = PlaybackRate::new(1.5).expect("valid rate");
    });
}

#[test]
fn pause_and_buffering_mid_window_reset_accumulation() {
    for interrupting_state in [
        PlaybackState::Paused,
        PlaybackState::Buffering,
        PlaybackState::Seeking,
    ] {
        assert_reset_mid_window_prevents_event(|harness| {
            harness.state = interrupting_state;
            harness.tick(Behaviour::LAGGING);
            harness.state = PlaybackState::Playing;
        });
    }
}

#[test]
fn lag_resumed_after_reset_fires_only_after_new_quiet_and_window() {
    let mut harness = Harness::new();
    harness.run(Behaviour::LAGGING, SECOND * 5);
    harness.generation += 1;

    // После сброса: 2 с тишины + 4 с окна ещё не прошли.
    assert!(harness.run(Behaviour::LAGGING, SECOND * 5).is_empty());
    assert_eq!(harness.run(Behaviour::LAGGING, SECOND * 2).len(), 1);
}

#[test]
fn double_speed_with_decoder_pulling_only_normal_speed_fires() {
    let mut harness = Harness::new();
    harness.rate = PlaybackRate::new(2.0).expect("valid rate");
    // Декодер даёт 25 fps при 50 fps нужных: каждый второй tick очередь пуста.
    let mut events = Vec::new();
    for tick_index in 0..200 {
        let behaviour = if tick_index % 2 == 0 {
            Behaviour::LAGGING
        } else {
            Behaviour::HEALTHY
        };
        events.extend(harness.tick(behaviour));
    }

    assert_eq!(events.len(), 1);
    assert_eq!(events[0].playback_rate, harness.rate);
    // Ожидание на 2x вдвое больше: 4 с окна x 2 x 25 fps = 200 кадров.
    assert!(events[0].expected_frames > 190.0, "{:?}", events[0]);
    assert!(events[0].missed_frame_ratio > 0.45, "{:?}", events[0]);
}

#[test]
fn falling_behind_is_reported_once_per_run_and_rearmed_by_a_new_run() {
    let mut harness = Harness::new();
    let first = harness.run(Behaviour::LAGGING, SECOND * 6 + TICK * 2);
    assert_eq!(first.len(), 1);

    // Тот же прогон отстаёт дальше: повторного события нет (иначе в режимах без
    // переключения лог получал бы строку каждые 4 с).
    assert!(harness.run(Behaviour::LAGGING, SECOND * 20).is_empty());

    // Пауза и продолжение — тот же прогон: детектор не перевзводится.
    harness.state = PlaybackState::Paused;
    harness.run(Behaviour::LAGGING, SECOND);
    harness.state = PlaybackState::Playing;
    assert!(harness.run(Behaviour::LAGGING, SECOND * 10).is_empty());

    // Новый прогон (смена декодера/seek -> новое поколение) снова взводит детектор.
    harness.generation += 1;
    let rearmed = harness.run(Behaviour::LAGGING, SECOND * 6 + TICK * 2);
    assert_eq!(rearmed.len(), 1);
    assert_eq!(rearmed[0].render_generation, harness.generation);
}

#[test]
fn long_gap_between_ticks_is_clamped() {
    let mut harness = Harness::new();
    harness.run(Behaviour::HEALTHY, SECOND * 2 + TICK * 2);
    harness.clock += SECOND * 30;

    // Один tick после 30-секундного провала не может закрыть окно сам.
    assert!(harness.tick(Behaviour::LAGGING).is_none());
}

#[test]
fn late_drop_burst_in_a_clamped_gap_tick_is_not_counted() {
    let mut harness = Harness::new();
    harness.run(Behaviour::HEALTHY, SECOND * 2 + TICK * 2);
    harness.clock += SECOND * 30;
    // Пачка из 15 устаревших кадров после 30-секундного провала (ожидаемых набежит ~6).
    let burst = Behaviour {
        late: 15,
        ..Behaviour::HEALTHY
    };
    let mut events: Vec<_> = harness.tick(burst).into_iter().collect();
    events.extend(harness.run(Behaviour::HEALTHY, SECOND * 6));

    assert!(events.is_empty(), "{events:?}");
}

#[test]
fn same_late_drops_spread_over_normal_ticks_do_fire() {
    let mut harness = Harness::new();
    harness.run(Behaviour::HEALTHY, SECOND * 2 + TICK * 2);
    // 20 late на 100 ожидаемых кадров окна (каждый четвёртый tick) = 20% > 10%.
    let mut events = Vec::new();
    for tick_index in 0..90 {
        let late = u64::from(tick_index % 4 == 0);
        events.extend(harness.tick(Behaviour {
            late,
            ..Behaviour::HEALTHY
        }));
    }

    assert_eq!(events.len(), 1, "{events:?}");
    assert_eq!(events[0].late_frames, 20);
}

#[test]
fn alternating_network_gap_and_arrival_is_not_decoder_fault() {
    let mut harness = Harness::new();
    let network_idle = Behaviour {
        input: DecoderInputWork::Absent,
        ..Behaviour::LAGGING
    };
    // Пакет пришёл (in-flight), очередь пуста — но прошлый шаг был ожиданием сети.
    let packet_arrived = Behaviour::LAGGING;
    let mut events = Vec::new();
    for _ in 0..200 {
        events.extend(harness.tick(network_idle));
        events.extend(harness.tick(packet_arrived));
    }

    assert!(events.is_empty(), "{events:?}");
}

#[test]
fn continuous_input_work_with_starvation_still_fires() {
    let mut harness = Harness::new();

    assert_eq!(harness.run(Behaviour::LAGGING, SECOND * 7).len(), 1);
}

#[test]
fn stalled_audio_clock_with_busy_decoder_is_not_counted() {
    let mut harness = Harness::new();
    let audio_stalled = Behaviour {
        audio: AudioClockActivity::Stalled,
        ..Behaviour::LAGGING
    };

    assert!(harness.run(audio_stalled, SECOND * 12).is_empty());
}
