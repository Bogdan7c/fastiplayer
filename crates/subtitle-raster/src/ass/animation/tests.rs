//! Тесты анимации ASS: интерполяция `\move`, затухание, классификация изменений.

use subtitle_core::{AssCue, AssFade, AssMove, AssOverride, AssTextItem, SubtitleTime};

use super::*;
use crate::ass::test_script::{cue_with, default_style, script_with, tags, text};

/// Реплика с заданными тегами в одном блоке.
fn cue_of(overrides: Vec<AssOverride>) -> AssCue {
    let script = script_with(vec![default_style()]);
    cue_with(&script, vec![tags(overrides), text("x")])
}

/// Часы реплики длиной `duration_ms`, начавшейся в нуле, в момент `elapsed_ms`.
fn clock(elapsed_ms: i64, duration_ms: i64) -> AssEventClock {
    AssEventClock::new(
        SubtitleTime::from_millis(elapsed_ms),
        AssEventTiming::new(SubtitleTime::ZERO, SubtitleTime::from_millis(duration_ms)),
    )
}

fn movement(start_ms: i32, end_ms: i32) -> AssOverride {
    AssOverride::Move(AssMove {
        from_x: 100.0,
        from_y: 200.0,
        to_x: 300.0,
        to_y: 400.0,
        start_ms,
        end_ms,
    })
}

fn position_of(cue: &AssCue, at_ms: i64, duration_ms: i64) -> Option<(f64, f64)> {
    evaluate_animation(cue, &clock(at_ms, duration_ms))
        .state
        .position
        .map(|point| (point.x, point.y))
}

fn fade_of(cue: &AssCue, at_ms: i64, duration_ms: i64) -> i32 {
    evaluate_animation(cue, &clock(at_ms, duration_ms))
        .state
        .fade
        .value()
}

fn at(ms: i64) -> AnimationChange {
    AnimationChange::At(SubtitleTime::from_millis(ms))
}

#[test]
fn text_without_time_tags_is_time_free_and_static() {
    let cue = cue_of(vec![AssOverride::Italic(Some(true))]);
    let animation = evaluate_animation(&cue, &clock(500, 2000));
    assert!(animation.is_time_free());
    assert_eq!(animation.change, AnimationChange::None);
    assert_eq!(animation.state, AssAnimationState::STILL);
    assert_eq!(animation.cache_key(), AssAnimationKey::TimeFree);
}

#[test]
fn move_over_the_whole_event_is_linear() {
    let cue = cue_of(vec![movement(0, 0)]);
    assert_eq!(position_of(&cue, 0, 2000), Some((100.0, 200.0)));
    assert_eq!(position_of(&cue, 1000, 2000), Some((200.0, 300.0)));
    assert_eq!(position_of(&cue, 2000, 2000), Some((300.0, 400.0)));
    // До начала события и после конца: края.
    assert_eq!(position_of(&cue, -50, 2000), Some((100.0, 200.0)));
    assert_eq!(position_of(&cue, 5000, 2000), Some((300.0, 400.0)));
}

#[test]
fn move_with_times_holds_the_endpoints_outside_the_interval() {
    let cue = cue_of(vec![movement(500, 1500)]);
    assert_eq!(position_of(&cue, 200, 4000), Some((100.0, 200.0)));
    assert_eq!(position_of(&cue, 500, 4000), Some((100.0, 200.0)));
    assert_eq!(position_of(&cue, 1000, 4000), Some((200.0, 300.0)));
    assert_eq!(position_of(&cue, 1500, 4000), Some((300.0, 400.0)));
    assert_eq!(position_of(&cue, 3000, 4000), Some((300.0, 400.0)));
}

#[test]
fn move_with_reversed_times_is_swapped_like_libass() {
    let cue = cue_of(vec![movement(1500, 500)]);
    assert_eq!(position_of(&cue, 1000, 4000), Some((200.0, 300.0)));
}

#[test]
fn move_after_pos_is_ignored_and_pos_after_move_is_ignored() {
    let pos_first = cue_of(vec![
        AssOverride::Position { x: 5.0, y: 6.0 },
        movement(0, 0),
    ]);
    let animation = evaluate_animation(&pos_first, &clock(1000, 2000));
    assert!(animation.is_time_free(), "\\pos wins, nothing moves");
    assert_eq!(animation.state.position, None);

    let move_first = cue_of(vec![
        movement(0, 0),
        AssOverride::Position { x: 5.0, y: 6.0 },
    ]);
    assert_eq!(position_of(&move_first, 1000, 2000), Some((200.0, 300.0)));
}

#[test]
fn only_the_first_move_counts() {
    let cue = cue_of(vec![
        movement(0, 0),
        AssOverride::Move(AssMove {
            from_x: 0.0,
            from_y: 0.0,
            to_x: 1.0,
            to_y: 1.0,
            start_ms: 0,
            end_ms: 0,
        }),
    ]);
    assert_eq!(position_of(&cue, 1000, 2000), Some((200.0, 300.0)));
}

#[test]
fn motionless_move_is_time_free() {
    let cue = cue_of(vec![AssOverride::Move(AssMove {
        from_x: 7.0,
        from_y: 8.0,
        to_x: 7.0,
        to_y: 8.0,
        start_ms: 0,
        end_ms: 0,
    })]);
    let animation = evaluate_animation(&cue, &clock(100, 2000));
    assert!(animation.is_time_free());
    assert_eq!(animation.change, AnimationChange::None);
}

#[test]
fn move_change_report_covers_before_during_and_after() {
    let cue = cue_of(vec![movement(500, 1500)]);
    let change = |ms: i64| evaluate_animation(&cue, &clock(ms, 4000)).change;
    // До начала: статично до `t1 + 1 мс`.
    assert_eq!(change(100), at(501));
    assert_eq!(change(500), at(501));
    // Внутри: каждый кадр.
    assert_eq!(change(501), AnimationChange::EveryFrame);
    assert_eq!(change(1499), AnimationChange::EveryFrame);
    // После: статично навсегда, но реплика остаётся времязависимой для перемотки назад.
    assert_eq!(change(1500), AnimationChange::None);
    assert!(!evaluate_animation(&cue, &clock(1500, 4000)).is_time_free());
}

#[test]
fn simple_fade_rewrites_times_through_the_event_duration() {
    // \fad(300, 400) на реплике 2000 мс: вход 0..300, выход 1600..2000.
    let cue = cue_of(vec![AssOverride::Fade(AssFade::Simple {
        fade_in_ms: 300,
        fade_out_ms: 400,
    })]);
    assert_eq!(fade_of(&cue, 0, 2000), 255);
    assert_eq!(fade_of(&cue, 150, 2000), 127);
    assert_eq!(fade_of(&cue, 300, 2000), 0);
    assert_eq!(fade_of(&cue, 1000, 2000), 0);
    assert_eq!(fade_of(&cue, 1600, 2000), 0);
    assert_eq!(fade_of(&cue, 1800, 2000), 127);
    assert_eq!(fade_of(&cue, 2000, 2000), 255);
}

#[test]
fn full_fade_uses_given_alphas_and_times() {
    let cue = cue_of(vec![AssOverride::Fade(AssFade::Full {
        alphas: [255, 100, 200],
        times_ms: [100, 300, 700, 900],
    })]);
    assert_eq!(fade_of(&cue, 0, 2000), 255);
    assert_eq!(fade_of(&cue, 200, 2000), 177);
    assert_eq!(fade_of(&cue, 500, 2000), 100);
    assert_eq!(fade_of(&cue, 800, 2000), 150);
    assert_eq!(fade_of(&cue, 1500, 2000), 200);
}

#[test]
fn only_the_first_fade_counts() {
    let cue = cue_of(vec![
        AssOverride::Fade(AssFade::Simple {
            fade_in_ms: 1000,
            fade_out_ms: 0,
        }),
        AssOverride::Fade(AssFade::Simple {
            fade_in_ms: 0,
            fade_out_ms: 0,
        }),
    ]);
    assert_eq!(fade_of(&cue, 0, 2000), 255);
}

#[test]
fn fade_change_report_follows_the_phases() {
    let cue = cue_of(vec![AssOverride::Fade(AssFade::Simple {
        fade_in_ms: 300,
        fade_out_ms: 400,
    })]);
    let change = |ms: i64| evaluate_animation(&cue, &clock(ms, 2000)).change;
    // Вход: каждый кадр. Середина: статично до начала выхода. Выход: каждый кадр. Конец: нет.
    assert_eq!(change(100), AnimationChange::EveryFrame);
    assert_eq!(change(300), at(1600));
    assert_eq!(change(1000), at(1600));
    assert_eq!(change(1700), AnimationChange::EveryFrame);
    assert_eq!(change(2000), AnimationChange::None);
}

#[test]
fn fade_and_move_reports_combine_to_the_earliest() {
    let cue = cue_of(vec![
        movement(1000, 1500),
        AssOverride::Fade(AssFade::Simple {
            fade_in_ms: 300,
            fade_out_ms: 400,
        }),
    ]);
    // Момент 500: затухание в середине (статично до 1600), движение статично до 1001.
    assert_eq!(evaluate_animation(&cue, &clock(500, 2000)).change, at(1001));
    // Момент 100: затухание идёт.
    assert_eq!(
        evaluate_animation(&cue, &clock(100, 2000)).change,
        AnimationChange::EveryFrame
    );
}

#[test]
fn cache_key_differs_between_states_and_matches_for_equal_ones() {
    let cue = cue_of(vec![movement(0, 0)]);
    let key = |ms: i64| evaluate_animation(&cue, &clock(ms, 2000)).cache_key();
    assert_eq!(key(500), key(500));
    assert_ne!(key(500), key(600));
    assert_ne!(key(500), AssAnimationKey::TimeFree);
    // Края интервала дают один и тот же ключ (положение застыло).
    assert_eq!(key(2000), key(9000));
}

#[test]
fn clock_uses_floor_milliseconds_and_never_panics_on_extreme_moments() {
    let timing = AssEventTiming::new(SubtitleTime::ZERO, SubtitleTime::from_millis(1000));
    let extreme = AssEventClock::new(SubtitleTime::from_micros(i64::MAX), timing);
    let cue = cue_of(vec![movement(0, 0)]);
    assert_eq!(
        evaluate_animation(&cue, &extreme)
            .state
            .position
            .map(|p| p.x),
        Some(300.0)
    );
    let before = AssEventClock::new(SubtitleTime::from_micros(i64::MIN), timing);
    assert_eq!(
        evaluate_animation(&cue, &before)
            .state
            .position
            .map(|p| p.x),
        Some(100.0)
    );
    // 999 мкс до начала — это -1 мс (вниз), а не 0.
    let slightly_before = AssEventClock::new(SubtitleTime::from_micros(-1), timing);
    assert_eq!(slightly_before.elapsed_ms, -1);
}

#[test]
fn text_items_without_overrides_are_ignored_by_the_scan() {
    let script = script_with(vec![default_style()]);
    let cue = cue_with(
        &script,
        vec![AssTextItem::Text("a".to_owned()), AssTextItem::HardBreak],
    );
    assert!(evaluate_animation(&cue, &clock(0, 100)).is_time_free());
}
