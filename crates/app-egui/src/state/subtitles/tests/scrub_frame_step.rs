//! Scrub не портит наблюдаемый шаг кадров: после отпускания анимированная реплика снова
//! получает по картинке на каждый кадр (а не одну широкую картинку на полсекунды).

use std::time::Duration;

use super::super::render_worker::clock_tracker::{ObservedMode, PlaybackObservation};
use super::ass_animation_support::*;
use super::render_thread::{FRAME_MS, GENERATION, HEIGHT, WIDTH, free_running, observe};
use super::*;

/// Движущаяся надпись на всю минуту: 100 пкс/с, то есть 0.1 пкс на миллисекунду.
fn moving_script() -> String {
    animated_script(
        (SCRIPT_WIDTH, SCRIPT_HEIGHT),
        &[(
            "0:00:00.00".to_owned(),
            "0:01:00.00".to_owned(),
            "{\\an5\\move(100,200,6100,200)}MOVING".to_owned(),
        )],
    )
}

/// Левый край движущейся надписи на кадре (`None` - на кадре нет субтитров).
fn moving_left_edge(owner: &mut SubtitleOwner, media: MediaInstanceId, pts_ms: u64) -> Option<i32> {
    let rect = full_rect(WIDTH, HEIGHT);
    owner
        .overlay_for_frame(current(media, GENERATION), &frame(pts_ms, GENERATION, rect))
        .map(left_edges_by_event)
        .and_then(|edges| edges.values().next().copied())
}

#[test]
fn animation_follows_each_frame_right_after_a_scrub_with_large_jumps() {
    let media = instance(1);
    let (mut owner, _dir) = owner_with_script(media, &moving_script());
    free_running(&mut owner);
    // Обычное воспроизведение: шаг кадров 40 мс известен и опорный левый край измерен.
    let mut reference = None;
    for index in 0..8_u64 {
        let pts_ms = 1_000 + index * FRAME_MS;
        let left = moving_left_edge(&mut owner, media, pts_ms);
        assert!(owner.presenter.wait_for_idle());
        reference = reference.or(left.map(|left| (pts_ms, left)));
    }
    let (reference_pts, reference_left) = reference.expect("a reference frame was shown");

    // Scrub: соседние кадры превью отстоят на 0.3..0.9 с.
    for target_ms in [2_000_u64, 2_900, 3_200, 4_100, 4_500] {
        observe(
            &mut owner,
            media,
            ObservedMode::Scrubbing {
                target: Duration::from_millis(target_ms),
            },
        );
        moving_left_edge(&mut owner, media, target_ms);
        assert!(owner.presenter.wait_for_idle());
    }
    owner.observe_playback(
        current(media, GENERATION),
        PlaybackObservation {
            read_run: player_core::SubtitleReadRun::INITIAL.next(),
            ..PlaybackObservation::default()
        },
    );

    // Воспроизведение после отпускания: каждый кадр получает картинку своего времени.
    for index in 1..=6_u64 {
        let pts_ms = 4_500 + index * FRAME_MS;
        let left = moving_left_edge(&mut owner, media, pts_ms).expect("the cue is on screen");
        let expected = f64::from(reference_left) + 0.1 * (pts_ms as f64 - reference_pts as f64);
        assert!(
            (f64::from(left) - expected).abs() <= 2.6,
            "frame {index} after the scrub at {pts_ms} ms: left edge {left}, its own time says {expected:.1}"
        );
        assert!(owner.presenter.wait_for_idle());
    }
}
