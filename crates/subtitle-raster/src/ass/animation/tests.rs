//! Тесты анимации ASS: интерполяция `\move`, затухание, классификация изменений.

use subtitle_core::{AssCue, AssFade, AssMove, AssOverride, AssTextItem, SubtitleTime};

use super::*;
use crate::ass::options::AssStorageSize;
use crate::ass::test_script::{cue_with, default_style, script_with, tags, text};
use crate::overlay::CanvasSize;

/// Масштаб кадра сценария 1280x720 на холсте 1280x720: единичный.
fn unit_frame(cue: &AssCue) -> AssFrameScale {
    let canvas = CanvasSize::new(1280, 720).expect("canvas");
    AssFrameScale::new(cue.script.info(), canvas, AssStorageSize::Unknown)
}

/// `evaluate_animation` на единичном масштабе кадра (задержка прокрутки не пересчитывается).
fn evaluate_animation(cue: &AssCue, clock: &AssEventClock) -> AssAnimation {
    super::evaluate_animation(cue, clock, &unit_frame(cue))
}

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
    // Ни положения, ни затухания; часы реплики передаются дальше для `\t`.
    assert_eq!(animation.state.position, None);
    assert_eq!(animation.state.fade, AssAnimationState::STILL.fade);
    assert_eq!(animation.state.clock, Some(clock(500, 2000)));
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

fn size_transform(t1_ms: i32, t2_ms: i32) -> AssOverride {
    use crate::ass::test_script::transform_range;
    transform_range(
        t1_ms,
        t2_ms,
        vec![AssOverride::FontSize(subtitle_core::AssFontSize::Absolute(
            80.0,
        ))],
    )
}

fn clip_transform(t1_ms: i32, t2_ms: i32) -> AssOverride {
    use crate::ass::test_script::transform_range;
    transform_range(
        t1_ms,
        t2_ms,
        vec![AssOverride::Clip(subtitle_core::AssClip {
            mode: subtitle_core::AssClipMode::Inside,
            shape: subtitle_core::AssClipShape::Rect {
                x0: 0,
                y0: 0,
                x1: 10,
                y1: 10,
            },
        })],
    )
}

#[test]
fn raster_transform_is_sampled_by_weight_and_reports_its_phases() {
    let cue = cue_of(vec![size_transform(500, 1500)]);
    let animation = |ms: i64| evaluate_animation(&cue, &clock(ms, 4000));
    assert!(!animation(100).is_time_free());
    assert_eq!(animation(100).change, at(501));
    assert_eq!(animation(1000).change, AnimationChange::EveryFrame);
    assert_eq!(animation(1500).change, AnimationChange::None);
    // До `t1` вес один и тот же (0): тот же ключ кэша, картинка не рисуется заново.
    assert_eq!(animation(100).cache_key(), animation(300).cache_key());
    // Внутри — каждый вес свой ключ; после `t2` вес 1 навсегда.
    assert_ne!(animation(800).cache_key(), animation(900).cache_key());
    assert_eq!(animation(1500).cache_key(), animation(3000).cache_key());
    assert_ne!(animation(100).cache_key(), animation(1500).cache_key());
}

#[test]
fn clip_only_transform_is_time_dependent_but_keeps_the_cache_key_time_free() {
    let cue = cue_of(vec![clip_transform(0, 1000)]);
    let during = evaluate_animation(&cue, &clock(500, 4000));
    assert!(!during.is_time_free());
    assert_eq!(during.change, AnimationChange::EveryFrame);
    assert_eq!(during.cache_key(), AssAnimationKey::TimeFree);
    let after = evaluate_animation(&cue, &clock(1000, 4000));
    assert_eq!(after.change, AnimationChange::None);
    assert!(!after.is_time_free());
}

#[test]
fn transform_and_move_combine_to_the_earliest_change() {
    let cue = cue_of(vec![movement(1000, 1500), size_transform(200, 600)]);
    let change = |ms: i64| evaluate_animation(&cue, &clock(ms, 4000)).change;
    assert_eq!(change(0), at(201));
    assert_eq!(change(300), AnimationChange::EveryFrame);
    assert_eq!(change(700), at(1001));
}

#[test]
fn move_and_fade_inside_a_transform_follow_first_wins_in_tag_order() {
    use crate::ass::test_script::transform_range;
    // `\move` внутри `\t` применяется сразу, как и вне его (положение анимирует сам `\move`).
    let cue = cue_of(vec![transform_range(0, 100, vec![movement(0, 0)])]);
    assert_eq!(position_of(&cue, 1000, 2000), Some((200.0, 300.0)));
    // `\pos` раньше (внутри `\t`) отключает последующий `\move` вне его.
    let pos_first = cue_of(vec![
        transform_range(0, 100, vec![AssOverride::Position { x: 5.0, y: 6.0 }]),
        movement(0, 0),
    ]);
    assert_eq!(position_of(&pos_first, 1000, 2000), None);
}

/// Реплика с полем `Effect`.
fn effect_cue(effect: &str) -> AssCue {
    let mut cue = cue_of(Vec::new());
    cue.effect = effect.to_owned();
    cue
}

#[test]
fn scroll_effect_makes_the_cue_sampled_and_changes_every_frame() {
    let cue = effect_cue("Banner;20");
    let animation = evaluate_animation(&cue, &clock(500, 4000));
    assert!(!animation.is_time_free());
    assert_eq!(animation.change, AnimationChange::EveryFrame);
    // 500 мс при 20 мс на пиксель -> 25 px.
    assert_eq!(animation.state.scroll_shift, Some(25.0));
}

#[test]
fn scroll_shift_is_a_fraction_not_a_step_per_delay() {
    let cue = effect_cue("Scroll up;0;720;40");
    let shift = |ms: i64| {
        evaluate_animation(&cue, &clock(ms, 4000))
            .state
            .scroll_shift
    };
    assert_eq!(shift(0), Some(0.0));
    assert_eq!(shift(10), Some(0.25));
    assert_eq!(shift(41), Some(1.025));
}

#[test]
fn scroll_cache_key_differs_between_moments_and_matches_for_equal_ones() {
    let cue = effect_cue("Banner;20");
    let key = |ms: i64| evaluate_animation(&cue, &clock(ms, 4000)).cache_key();
    assert_ne!(key(100), key(101));
    assert_eq!(key(100), key(100));
}

#[test]
fn cue_without_a_scroll_effect_has_no_shift() {
    for effect in ["", "Karaoke", "Scroll up;1;2", "Scroll sideways;1;2;3"] {
        let cue = effect_cue(effect);
        let animation = evaluate_animation(&cue, &clock(500, 4000));
        assert_eq!(animation.state.scroll_shift, None, "{effect:?}");
        assert!(animation.is_time_free(), "{effect:?}");
    }
}

#[test]
fn scroll_delay_is_rescaled_by_layout_resolution() {
    // PlayRes 1280x720, холст 1280x720, хранение 640x360: scale = 0.5.
    let cue = effect_cue("Banner;0");
    let canvas = CanvasSize::new(1280, 720).expect("canvas");
    let frame = AssFrameScale::new(
        cue.script.info(),
        canvas,
        AssStorageSize::from_pixels(640, 360),
    );
    let shift = super::evaluate_animation(&cue, &clock(100, 4000), &frame)
        .state
        .scroll_shift;
    // delay 0 -> max(0/0.5, 1) = 1 -> 1 * 0.5 = 0.5 мс на пиксель PlayRes: 100 / 0.5.
    assert_eq!(shift, Some(200.0));
}
