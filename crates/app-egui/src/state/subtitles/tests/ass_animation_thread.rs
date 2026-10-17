//! Анимация ASS через настоящий поток-рисовальщик: каждый кадр получает картинку СВОЕГО
//! времени, неподвижное не растеризуется повторно, пауза не гоняет работу, тяжёлая сцена не
//! блокирует выбор картинки в UI.

use std::time::{Duration, Instant};

use super::super::SubtitleOwner;
use super::super::render_worker::clock_tracker::ObservedMode;
use super::ass_animation_support::*;
use super::render_thread::{
    FRAME_MS, GENERATION, HEIGHT, WIDTH, free_running, observe, shown_events,
};
use super::*;

/// Событие на всё время теста (минута).
fn whole_minute(text: &str) -> (String, String, String) {
    (
        "0:00:00.00".to_owned(),
        "0:01:00.00".to_owned(),
        text.to_owned(),
    )
}

/// Движущаяся надпись (по 100 пкс/с, т. е. 4 пкс за кадр 40 мс) и неподвижная под ней.
fn moving_and_static_script() -> String {
    // `\move` на всю минуту: x от 100 до 6100 за 60 с = 100 пкс/с.
    animated_script(
        (SCRIPT_WIDTH, SCRIPT_HEIGHT),
        &[
            whole_minute("{\\an5\\move(100,200,6100,200)}MOVING"),
            whole_minute("{\\an5\\pos(640,600)}STATIC"),
        ],
    )
}

/// Один кадр: выбор картинки (как UI) -> левые края реплик.
fn left_edges_at(
    owner: &mut SubtitleOwner,
    media: MediaInstanceId,
    pts_ms: u64,
) -> std::collections::BTreeMap<u32, i32> {
    let rect = full_rect(WIDTH, HEIGHT);
    owner
        .overlay_for_frame(current(media, GENERATION), &frame(pts_ms, GENERATION, rect))
        .map(left_edges_by_event)
        .unwrap_or_default()
}

#[test]
fn steady_playback_of_an_animated_cue_shows_each_frame_its_own_picture_and_draws_static_once() {
    let media = instance(1);
    let (mut owner, _dir) = owner_with_script(media, &moving_and_static_script());
    free_running(&mut owner);
    // Прогрев: самый первый кадр холодный, дальше поток впереди.
    let warm_up_frames = 8_u64;
    let first_ms = 1_000_u64;
    for index in 0..warm_up_frames {
        left_edges_at(&mut owner, media, first_ms + index * FRAME_MS);
        assert!(owner.presenter.wait_for_idle());
    }
    let stats_before = owner.presentation_stats();
    let counters_before = owner.presenter.queue_counters();

    let frames = 100_u64;
    let mut moving_lefts = Vec::new();
    for index in warm_up_frames..warm_up_frames + frames {
        let edges = left_edges_at(&mut owner, media, first_ms + index * FRAME_MS);
        assert_eq!(edges.len(), 2, "both cues are on screen at frame {index}");
        moving_lefts.push((index, edges));
        assert!(owner.presenter.wait_for_idle());
    }

    // Неподвижная реплика стоит на месте на всех кадрах; у движущейся левый край растёт.
    let event_ids: Vec<u32> = moving_lefts[0].1.keys().copied().collect();
    let range_of = |event: u32| {
        let values: Vec<i32> = moving_lefts
            .iter()
            .map(|(_, edges)| edges[&event])
            .collect();
        (
            values.iter().copied().min().unwrap_or(0),
            values.iter().copied().max().unwrap_or(0),
        )
    };
    let (still_event, moving_event) = if range_of(event_ids[0]).1 - range_of(event_ids[0]).0
        < range_of(event_ids[1]).1 - range_of(event_ids[1]).0
    {
        (event_ids[0], event_ids[1])
    } else {
        (event_ids[1], event_ids[0])
    };
    let (still_min, still_max) = range_of(still_event);
    assert_eq!(still_min, still_max, "the static cue never moves");
    // 4 пкс на кадр (100 пкс/с * 40 мс): каждый кадр получил картинку своего времени.
    let (first_index, first_edges) = &moving_lefts[0];
    for (index, edges) in &moving_lefts {
        let expected =
            first_edges[&moving_event] + 4 * i32::try_from(index - first_index).unwrap_or(0);
        assert!(
            (edges[&moving_event] - expected).abs() <= 1,
            "frame {index}: moving cue at {} but its own time says {expected}",
            edges[&moving_event]
        );
    }

    // Ни одного опоздания и ни одного устаревшего повтора: все кадры точные.
    let stats_after = owner.presentation_stats();
    let counters_after = owner.presenter.queue_counters();
    assert_eq!(stats_after.late_pictures, stats_before.late_pictures);
    assert_eq!(
        counters_after.stale_fallbacks,
        counters_before.stale_fallbacks
    );
    assert_eq!(
        counters_after.exact_hits - counters_before.exact_hits,
        frames
    );

    // Поток не крутится вхолостую, а неподвижная надпись растеризована один раз:
    // всего растеризаций = по одной движущейся на каждую нарисованную картинку + одна неподвижная.
    let diagnostics = owner.presenter.worker_diagnostics().expect("worker");
    assert_eq!(diagnostics.repeated_jobs_suppressed, 0);
    let rendered = diagnostics.raster.rendered_frames;
    assert!(
        rendered <= warm_up_frames + frames + 20,
        "bounded work, drew {rendered} pictures for {} frames",
        warm_up_frames + frames
    );
    assert_eq!(
        diagnostics.raster.rasterized_cues,
        rendered + 1,
        "one moving cue per picture and the static cue exactly once"
    );
}

/// Время кадра 23.976 к/с в миллисекундах с округлением pts, как у настоящего видео
/// (0, 42, 83, 125, 167, ...): кадры лежат не на сетке «опорное время + k * шаг».
fn ntsc_pts_ms(first_ms: u64, index: u64) -> u64 {
    first_ms + (index * 1001 + 12) / 24
}

#[test]
fn frames_with_millisecond_rounded_pts_each_show_the_picture_nearest_their_own_time() {
    let media = instance(1);
    let (mut owner, _dir) = owner_with_script(media, &moving_and_static_script());
    free_running(&mut owner);
    let first_ms = 1_000_u64;
    let warm_up_frames = 8_u64;
    for index in 0..warm_up_frames {
        left_edges_at(&mut owner, media, ntsc_pts_ms(first_ms, index));
        assert!(owner.presenter.wait_for_idle());
    }
    let stats_before = owner.presentation_stats();
    let counters_before = owner.presenter.queue_counters();

    // `\move` на 100 пкс/с: 0.1 пкс на миллисекунду. Движущаяся — реплика с бо́льшим размахом.
    let frames = 120_u64;
    let mut samples = Vec::new();
    for index in warm_up_frames..warm_up_frames + frames {
        let pts_ms = ntsc_pts_ms(first_ms, index);
        samples.push((pts_ms, left_edges_at(&mut owner, media, pts_ms)));
        assert!(owner.presenter.wait_for_idle());
    }

    let moving_event = moving_event_of(&samples);
    let (reference_pts, reference_edges) = &samples[0];
    let reference_left = reference_edges[&moving_event];
    let mut previous_left = None;
    for (pts_ms, edges) in &samples {
        let left = edges[&moving_event];
        // Картинка ближайшего времени ошибается не больше чем на полшага (2.1 пкс) плюс округление.
        let expected = f64::from(reference_left) + 0.1 * (*pts_ms as f64 - *reference_pts as f64);
        assert!(
            (f64::from(left) - expected).abs() <= 2.6,
            "frame at {pts_ms} ms: left edge {left}, its own time says {expected:.1} (a lag of one frame is 4.2)"
        );
        if let Some(previous) = previous_left {
            let delta = left - previous;
            assert!(
                (2..=7).contains(&delta),
                "frame at {pts_ms} ms moved by {delta} px: positions must advance by ~4.2 px per frame"
            );
        }
        previous_left = Some(left);
    }
    let stats_after = owner.presentation_stats();
    let counters_after = owner.presenter.queue_counters();
    assert_eq!(stats_after.late_pictures, stats_before.late_pictures);
    assert_eq!(
        counters_after.stale_fallbacks,
        counters_before.stale_fallbacks
    );
    assert_eq!(
        counters_after.exact_hits - counters_before.exact_hits,
        frames
    );
}

#[test]
fn animated_cue_starting_between_frames_never_shows_before_its_start() {
    // Событие начинается на 1.01 с: кадры 960 и 1002 мс ещё до него, 1043 мс — первый с ним.
    let script = animated_script(
        (SCRIPT_WIDTH, SCRIPT_HEIGHT),
        &[(
            "0:00:01.01".to_owned(),
            "0:01:00.00".to_owned(),
            "{\\an5\\move(100,200,6100,200)}LATE".to_owned(),
        )],
    );
    let media = instance(1);
    let (mut owner, _dir) = owner_with_script(media, &script);
    free_running(&mut owner);
    // Кадры идут подряд с шагом ~41.7 мс, чтобы часы наблюдали настоящий шаг.
    let mut shown_by_pts = Vec::new();
    for index in 0..12_u64 {
        let pts_ms = ntsc_pts_ms(960, index);
        shown_by_pts.push((pts_ms, shown_events(&mut owner, media, pts_ms)));
        assert!(owner.presenter.wait_for_idle());
    }
    for (pts_ms, events) in &shown_by_pts {
        if *pts_ms < 1_010 {
            assert!(
                events.is_empty(),
                "frame at {pts_ms} ms is before the cue start"
            );
        } else {
            assert_eq!(events.len(), 1, "frame at {pts_ms} ms shows the cue");
        }
    }
    assert!(
        shown_by_pts.iter().any(|(pts, _)| *pts < 1_010),
        "the test has frames before the start"
    );
    assert_eq!(owner.presentation_stats().late_pictures, 0);
}

/// Событие, левый край которого заметно меняется на кадрах (движущаяся надпись).
fn moving_event_of(samples: &[(u64, std::collections::BTreeMap<u32, i32>)]) -> u32 {
    let spread = |event: u32| {
        let lefts: Vec<i32> = samples.iter().map(|(_, edges)| edges[&event]).collect();
        lefts.iter().max().copied().unwrap_or(0) - lefts.iter().min().copied().unwrap_or(0)
    };
    let events: Vec<u32> = samples[0].1.keys().copied().collect();
    events
        .into_iter()
        .max_by_key(|event| spread(*event))
        .expect("at least one cue is on screen")
}

#[test]
fn paused_animated_cue_causes_no_work_after_its_picture_is_ready() {
    let media = instance(1);
    let (mut owner, _dir) = owner_with_script(media, &moving_and_static_script());
    free_running(&mut owner);
    observe(&mut owner, media, ObservedMode::Paused);
    assert!(owner.presenter.wait_for_idle());

    // Картинка паузы приходит (первый выбор может быть пустым: поток ещё рисует).
    shown_events(&mut owner, media, 2_000);
    assert!(owner.presenter.wait_for_idle());
    owner.poll_jobs();
    assert_eq!(
        shown_events(&mut owner, media, 2_000).len(),
        2,
        "picture is ready"
    );
    let ready = owner.presenter.worker_diagnostics().expect("worker");
    assert_eq!(
        ready.raster.rendered_frames, 1,
        "paused: exactly the current frame is drawn"
    );

    for _ in 0..20 {
        assert_eq!(shown_events(&mut owner, media, 2_000).len(), 2);
        assert!(owner.presenter.wait_for_idle());
    }

    let later = owner.presenter.worker_diagnostics().expect("worker");
    assert_eq!(later.raster.rendered_frames, ready.raster.rendered_frames);
    assert_eq!(later.raster.rasterized_cues, ready.raster.rasterized_cues);
    assert_eq!(later.repeated_jobs_suppressed, 0);
}

/// Число одновременно анимированных событий тяжёлой сцены.
const HEAVY_EVENTS: usize = 50;

/// Тяжёлая сцена: `\move` + `\fad`, часть с `\frz`.
fn heavy_script() -> String {
    let events: Vec<_> = (0..HEAVY_EVENTS)
        .map(|index| {
            let row = i32::try_from(index % 10).unwrap_or(0);
            let column = i32::try_from(index / 10).unwrap_or(0);
            let (x1, y1) = (100 + column * 220, 60 + row * 65);
            let rotation = if index % 3 == 0 { "\\frz25" } else { "" };
            whole_minute(&format!(
                "{{\\an5\\move({x1},{y1},{},{y1}){rotation}\\fad(300,300)}}Event {index}",
                x1 + 400
            ))
        })
        .collect();
    animated_script((SCRIPT_WIDTH, SCRIPT_HEIGHT), &events)
}

/// Итог прогона тяжёлой сцены.
struct HeavyReport {
    /// Самый долгий выбор картинки в UI.
    slowest_select: Duration,
    /// Сколько времени поток в среднем тратил на кадр (растеризация + пересылка).
    average_worker_time_per_frame: Duration,
    slowest_rasterization: Duration,
    dropped_events: u64,
    on_screen_events: usize,
}

/// Играет `frames` кадров тяжёлой сцены: измеряет выбор в UI (без ожидания потока) и,
/// отдельно, время потока на кадр.
fn run_heavy_scene(frames: u64) -> HeavyReport {
    let media = instance(1);
    let (mut owner, _dir) = owner_with_script(media, &heavy_script());
    free_running(&mut owner);
    let first_ms = 1_000_u64;
    for index in 0..8 {
        shown_events(&mut owner, media, first_ms + index * FRAME_MS);
        assert!(owner.presenter.wait_for_idle());
    }
    let mut slowest_select = Duration::ZERO;
    let mut worker_time = Duration::ZERO;
    let mut on_screen_events = 0;
    for index in 8..8 + frames {
        let selecting = Instant::now();
        on_screen_events = shown_events(&mut owner, media, first_ms + index * FRAME_MS).len();
        slowest_select = slowest_select.max(selecting.elapsed());
        let waiting = Instant::now();
        assert!(owner.presenter.wait_for_idle());
        worker_time += waiting.elapsed();
    }
    let diagnostics = owner.presenter.worker_diagnostics().expect("worker");
    assert_eq!(diagnostics.repeated_jobs_suppressed, 0);
    HeavyReport {
        slowest_select,
        average_worker_time_per_frame: worker_time / u32::try_from(frames).unwrap_or(1),
        slowest_rasterization: owner.presentation_stats().slowest_rasterization,
        dropped_events: diagnostics.raster.dropped_ass_events_over_frame_limit,
        on_screen_events,
    }
}

#[test]
fn heavy_animated_scene_never_blocks_picture_selection_in_the_ui() {
    let media = instance(1);
    let (mut owner, _dir) = owner_with_script(media, &heavy_script());
    free_running(&mut owner);
    // Модель занятого потока: каждая растеризация долгая (200 мс), кадры идут по 40 мс.
    owner.presenter.set_render_delay(Duration::from_millis(200));

    let mut slowest_select = Duration::ZERO;
    for index in 0..60_u64 {
        let selecting = Instant::now();
        shown_events(&mut owner, media, 1_000 + index * FRAME_MS);
        slowest_select = slowest_select.max(selecting.elapsed());
    }

    // Поток отстаёт (это его дело), а выбор в UI не ждёт его ни разу.
    assert!(
        slowest_select < Duration::from_millis(50),
        "selection waited for the busy worker: {slowest_select:?}"
    );
    assert!(
        owner.presentation_stats().late_pictures > 0,
        "the worker really was behind"
    );
    assert!(owner.presenter.wait_for_idle());
}

#[test]
fn heavy_animated_scene_is_fully_drawn_and_selection_stays_fast_when_the_worker_keeps_up() {
    let report = run_heavy_scene(40);
    assert_eq!(
        report.dropped_events, 0,
        "all {HEAVY_EVENTS} events fit the limit"
    );
    assert_eq!(
        report.on_screen_events, HEAVY_EVENTS,
        "every event is on screen"
    );
    assert!(
        report.slowest_select < Duration::from_millis(20),
        "slowest selection {:?}",
        report.slowest_select
    );
}

/// Замер для заметок: `cargo test -p app-egui --release --locked -- --ignored
/// heavy_scene_release_measurement --nocapture`.
#[test]
#[ignore = "замер времени растеризации: запускать с --release и --nocapture"]
fn heavy_scene_release_measurement() {
    let report = run_heavy_scene(200);
    eprintln!(
        "heavy scene ({HEAVY_EVENTS} animated events): worker {:.2} ms per frame, slowest rasterization {:.2} ms, slowest UI select {:.3} ms",
        report.average_worker_time_per_frame.as_secs_f64() * 1000.0,
        report.slowest_rasterization.as_secs_f64() * 1000.0,
        report.slowest_select.as_secs_f64() * 1000.0,
    );
    assert!(report.slowest_select < Duration::from_millis(5));
}
