//! Тесты планировщика: результат — какое время рисовать следующим.

use super::*;

fn ms(value: i64) -> SubtitleTime {
    SubtitleTime::from_millis(value)
}

fn step_ms(value: i64) -> FrameCadence {
    FrameCadence::Known(FrameStep::from_micros(value * 1_000).expect("положительный шаг"))
}

fn clock(current_ms: i64, step: FrameCadence, mode: PresentationMode) -> PresentationClock {
    PresentationClock {
        current: ms(current_ms),
        cadence: step,
        rate: PlaybackRate::NORMAL,
        mode,
    }
}

fn interval(from_ms: i64, until_ms: Option<i64>) -> ValidityInterval {
    ValidityInterval::new(ms(from_ms), until_ms.map(ms))
}

fn render_at(time_ms: i64) -> ScheduleDecision {
    ScheduleDecision::Render {
        lookup_time: ms(time_ms),
    }
}

fn plan(clock: &PresentationClock, frames: u32, prepared: &[ValidityInterval]) -> ScheduleDecision {
    plan_next_render(clock, PrerenderFrames::new(frames), prepared)
}

/// Прогоняет планировщик как поток-рисовальщик: каждое решение «рисуем» добавляет интервал
/// до ближайшей смены `next_change`. Возвращает времена рисования по порядку.
fn drain(
    clock: &PresentationClock,
    frames: u32,
    next_change: impl Fn(i64) -> Option<i64>,
    prepared: &mut Vec<ValidityInterval>,
) -> Vec<i64> {
    let mut rendered = Vec::new();
    while let ScheduleDecision::Render { lookup_time } = plan(clock, frames, prepared) {
        let at = lookup_time.as_micros() / 1_000;
        rendered.push(at);
        prepared.push(ValidityInterval::new(lookup_time, next_change(at).map(ms)));
        assert!(rendered.len() < 100, "планировщик зациклился");
    }
    rendered
}

#[test]
fn playing_without_prepared_renders_current_frame_first() {
    let clock = clock(1_000, step_ms(40), PresentationMode::Playing);
    assert_eq!(plan(&clock, 4, &[]), render_at(1_000));
}

#[test]
fn playing_fills_horizon_frame_by_frame_on_the_frame_grid() {
    let clock = clock(1_000, step_ms(40), PresentationMode::Playing);
    // Каждая картинка годна ровно один кадр (анимированная сцена).
    let mut prepared = Vec::new();
    let rendered = drain(&clock, 4, |at| Some(at + 40), &mut prepared);
    assert_eq!(rendered, [1_000, 1_040, 1_080, 1_120, 1_160]);
}

#[test]
fn static_interval_covering_horizon_means_idle() {
    let clock = clock(1_000, step_ms(40), PresentationMode::Playing);
    let prepared = [interval(900, Some(5_000))];
    assert_eq!(plan(&clock, 4, &prepared), ScheduleDecision::Idle);
    // Открытый конец (до конца дорожки) тоже покрывает.
    assert_eq!(
        plan(&clock, 4, &[interval(0, None)]),
        ScheduleDecision::Idle
    );
}

#[test]
fn static_scene_is_rasterized_once_for_a_hundred_frames() {
    let mut prepared: Vec<ValidityInterval> = Vec::new();
    let mut rendered_count = 0;
    for frame in 0..100 {
        let clock = clock(1_000 + frame * 40, step_ms(40), PresentationMode::Playing);
        while let ScheduleDecision::Render { lookup_time } = plan(&clock, 4, &prepared) {
            rendered_count += 1;
            prepared.push(ValidityInterval::new(lookup_time, Some(ms(10_000))));
        }
    }
    assert_eq!(rendered_count, 1);
}

#[test]
fn cue_boundary_inside_horizon_renders_first_frame_after_it() {
    let clock = clock(1_000, step_ms(40), PresentationMode::Playing);
    // Реплика заканчивается на 1_100: кадры 1_000, 1_040, 1_080 покрыты, 1_120 — нет.
    let prepared = [interval(1_000, Some(1_100))];
    assert_eq!(plan(&clock, 4, &prepared), render_at(1_120));
}

#[test]
fn boundary_exactly_on_a_grid_frame_renders_that_frame() {
    let clock = clock(1_000, step_ms(40), PresentationMode::Playing);
    // Конец интервала не включён: кадр 1_080 уже требует новой картинки.
    let prepared = [interval(1_000, Some(1_080))];
    assert_eq!(plan(&clock, 4, &prepared), render_at(1_080));
}

#[test]
fn boundary_beyond_horizon_is_not_rendered_yet() {
    let clock = clock(1_000, step_ms(40), PresentationMode::Playing);
    // Горизонт 4 кадра: последний кадр 1_160; смена на 1_161 за горизонтом.
    let prepared = [interval(1_000, Some(1_161))];
    assert_eq!(plan(&clock, 4, &prepared), ScheduleDecision::Idle);
}

#[test]
fn two_adjacent_intervals_cover_a_gap_free_horizon() {
    let clock = clock(1_000, step_ms(40), PresentationMode::Playing);
    let prepared = [interval(1_000, Some(1_100)), interval(1_100, Some(2_000))];
    assert_eq!(plan(&clock, 4, &prepared), ScheduleDecision::Idle);
}

#[test]
fn gap_between_intervals_is_found() {
    let clock = clock(1_000, step_ms(40), PresentationMode::Playing);
    let prepared = [interval(1_000, Some(1_050)), interval(1_100, Some(2_000))];
    // 1_000, 1_040 покрыты первым; 1_080 — дыра.
    assert_eq!(plan(&clock, 4, &prepared), render_at(1_080));
}

#[test]
fn double_speed_uses_the_grown_observed_step() {
    // 25 fps на 2×: соседние показанные кадры отстоят на 80 мс времени субтитров.
    let clock = PresentationClock {
        current: ms(1_000),
        cadence: step_ms(80),
        rate: PlaybackRate::from_thousandths(2_000),
        mode: PresentationMode::Playing,
    };
    let mut prepared = Vec::new();
    let rendered = drain(&clock, 4, |at| Some(at + 80), &mut prepared);
    assert_eq!(rendered, [1_000, 1_080, 1_160, 1_240, 1_320]);
}

#[test]
fn zero_or_negative_step_is_not_a_frame_step() {
    assert!(FrameStep::from_micros(0).is_none());
    assert!(FrameStep::from_micros(-40_000).is_none());
}

#[test]
fn unknown_cadence_renders_only_the_current_frame() {
    let clock = clock(1_000, FrameCadence::Unknown, PresentationMode::Playing);
    let mut prepared = Vec::new();
    let rendered = drain(&clock, 4, |at| Some(at + 1), &mut prepared);
    assert_eq!(rendered, [1_000]);
}

#[test]
fn paused_renders_only_the_current_frame() {
    let clock = clock(1_000, step_ms(40), PresentationMode::Paused);
    let mut prepared = Vec::new();
    let rendered = drain(&clock, 4, |at| Some(at + 40), &mut prepared);
    assert_eq!(rendered, [1_000]);
    // Готово — работы нет.
    assert_eq!(plan(&clock, 4, &prepared), ScheduleDecision::Idle);
}

#[test]
fn stopped_or_reversed_rate_has_no_forward_horizon() {
    for thousandths in [0, -1_000] {
        let clock = PresentationClock {
            current: ms(1_000),
            cadence: step_ms(40),
            rate: PlaybackRate::from_thousandths(thousandths),
            mode: PresentationMode::Playing,
        };
        let mut prepared = Vec::new();
        assert_eq!(drain(&clock, 4, |at| Some(at + 40), &mut prepared), [1_000]);
    }
}

#[test]
fn seeking_renders_target_first_then_horizon_after_it() {
    let clock = clock(
        1_000,
        step_ms(40),
        PresentationMode::Seeking { target: ms(60_000) },
    );
    let mut prepared = Vec::new();
    let rendered = drain(&clock, 3, |at| Some(at + 40), &mut prepared);
    assert_eq!(rendered, [60_000, 60_040, 60_080, 60_120]);
}

#[test]
fn seeking_ignores_old_current_frame_even_if_it_is_unprepared() {
    let clock = clock(
        1_000,
        step_ms(40),
        PresentationMode::Seeking { target: ms(60_000) },
    );
    // Старый кадр на 1_000 не покрыт, но цель важнее: первым идёт цель.
    assert_eq!(plan(&clock, 3, &[]), render_at(60_000));
}

#[test]
fn seeking_with_prepared_target_continues_with_horizon() {
    let clock = clock(
        1_000,
        step_ms(40),
        PresentationMode::Seeking { target: ms(60_000) },
    );
    let prepared = [interval(60_000, Some(60_050))];
    assert_eq!(plan(&clock, 3, &prepared), render_at(60_080));
}

#[test]
fn scrubbing_latest_target_wins() {
    let mut prepared = vec![interval(10_000, Some(10_040))];
    let first = clock(
        0,
        step_ms(40),
        PresentationMode::Scrubbing { target: ms(10_000) },
    );
    // Цель 10_000 уже готова, дальше идёт её горизонт.
    assert_eq!(plan(&first, 2, &prepared), render_at(10_040));
    // Пользователь утащил ползунок дальше: планируется только новая цель, старый горизонт забыт.
    let latest = clock(
        0,
        step_ms(40),
        PresentationMode::Scrubbing { target: ms(90_000) },
    );
    assert_eq!(plan(&latest, 2, &prepared), render_at(90_000));
    prepared.push(interval(90_000, Some(95_000)));
    assert_eq!(plan(&latest, 2, &prepared), ScheduleDecision::Idle);
}

#[test]
fn scrubbing_with_unknown_cadence_renders_only_the_target() {
    let clock = clock(
        0,
        FrameCadence::Unknown,
        PresentationMode::Scrubbing { target: ms(5_000) },
    );
    let mut prepared = Vec::new();
    assert_eq!(drain(&clock, 4, |at| Some(at + 40), &mut prepared), [5_000]);
}

#[test]
fn zero_horizon_means_only_the_anchor() {
    let clock = clock(1_000, step_ms(40), PresentationMode::Playing);
    let mut prepared = Vec::new();
    assert_eq!(drain(&clock, 0, |at| Some(at + 40), &mut prepared), [1_000]);
}

#[test]
fn huge_horizon_from_config_is_capped() {
    assert_eq!(PrerenderFrames::new(u32::MAX).get(), MAX_PRERENDER_FRAMES);
}

#[test]
fn time_arithmetic_near_i64_limits_does_not_overflow() {
    let clock = PresentationClock {
        current: SubtitleTime::from_micros(i64::MAX - 10),
        cadence: step_ms(40),
        rate: PlaybackRate::NORMAL,
        mode: PresentationMode::Playing,
    };
    assert_eq!(
        plan(&clock, 4, &[]),
        ScheduleDecision::Render {
            lookup_time: SubtitleTime::from_micros(i64::MAX - 10)
        }
    );
}

#[test]
fn planning_anchor_is_the_target_while_seeking_or_scrubbing_and_the_frame_otherwise() {
    let seeking = clock(
        6_000,
        step_ms(40),
        PresentationMode::Seeking { target: ms(500) },
    );
    let scrubbing = clock(
        6_000,
        step_ms(40),
        PresentationMode::Scrubbing { target: ms(300) },
    );
    let playing = clock(6_000, step_ms(40), PresentationMode::Playing);
    let paused = clock(6_000, step_ms(40), PresentationMode::Paused);

    assert_eq!(seeking.planning_anchor(), ms(500));
    assert_eq!(scrubbing.planning_anchor(), ms(300));
    assert_eq!(playing.planning_anchor(), ms(6_000));
    assert_eq!(paused.planning_anchor(), ms(6_000));
}

#[test]
fn planning_window_matches_what_the_planner_wants_covered() {
    let horizon = PrerenderFrames::new(4);
    let playing = clock(1_000, step_ms(40), PresentationMode::Playing);
    let paused = clock(1_000, step_ms(40), PresentationMode::Paused);
    let seeking = clock(
        9_000,
        step_ms(40),
        PresentationMode::Seeking { target: ms(500) },
    );
    let unknown_step = clock(1_000, FrameCadence::Unknown, PresentationMode::Playing);

    assert_eq!(playing.planning_window(horizon), (ms(1_000), ms(1_160)));
    assert_eq!(paused.planning_window(horizon), (ms(1_000), ms(1_000)));
    assert_eq!(seeking.planning_window(horizon), (ms(500), ms(660)));
    assert_eq!(
        unknown_step.planning_window(horizon),
        (ms(1_000), ms(1_000))
    );
}
