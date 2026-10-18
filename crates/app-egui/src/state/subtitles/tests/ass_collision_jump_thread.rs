//! Положение сдвинутых столкновением ASS-реплик после seek не зависит от того, откуда прыгнули
//! (поток забывает историю раскладки на новой цели прыжка).

use std::collections::BTreeMap;
use std::time::Duration;

use super::super::SubtitleOwner;
use super::super::render_worker::clock_tracker::ObservedMode;
use super::ass_animation_support::*;
use super::render_thread::{GENERATION, HEIGHT, WIDTH, observe};
use super::*;

/// Три нижние реплики одного слоя в порядке файла X, Z, Y: X 0-10 с, Z 4-10 с, Y 2-5 с.
fn x_z_y_script() -> String {
    let event = |start: &str, end: &str, text: &str| {
        (start.to_owned(), end.to_owned(), format!("{{\\an2}}{text}"))
    };
    animated_script(
        (SCRIPT_WIDTH, SCRIPT_HEIGHT),
        &[
            event("0:00:00.00", "0:00:10.00", "Line X"),
            event("0:00:04.00", "0:00:10.00", "Line Z"),
            event("0:00:02.00", "0:00:05.00", "Line Y"),
        ],
    )
}

/// Верхний край картинок каждой реплики (по номеру события) на кадре.
fn tops_at(owner: &mut SubtitleOwner, media: MediaInstanceId, pts_ms: u64) -> BTreeMap<u32, i32> {
    let rect = full_rect(WIDTH, HEIGHT);
    let mut tops: BTreeMap<u32, i32> = BTreeMap::new();
    if let Some(overlay) =
        owner.overlay_for_frame(current(media, GENERATION), &frame(pts_ms, GENERATION, rect))
    {
        for bitmap in overlay.bitmaps() {
            if let Some(key) = bitmap.cue_key() {
                tops.entry(key.event().get())
                    .and_modify(|known| *known = (*known).min(bitmap.origin().y))
                    .or_insert(bitmap.origin().y);
            }
        }
    }
    tops
}

/// Играет кадры с `from_ms` по `to_ms` шагом 100 мс, затем прыгает на `target_ms` и показывает
/// первый кадр цели.
fn jump_from(from_ms: u64, to_ms: u64, target_ms: u64) -> BTreeMap<u32, i32> {
    let media = instance(1);
    let (mut owner, _dir) = owner_with_script(media, &x_z_y_script());
    for pts_ms in (from_ms..=to_ms).step_by(100) {
        tops_at(&mut owner, media, pts_ms);
    }
    observe(
        &mut owner,
        media,
        ObservedMode::Seeking {
            target: Duration::from_millis(target_ms),
        },
    );
    assert!(owner.presenter.wait_for_idle());
    observe(&mut owner, media, ObservedMode::Playing);
    tops_at(&mut owner, media, target_ms)
}

#[test]
fn seek_target_layout_does_not_depend_on_where_the_seek_came_from() {
    // С 1 с (на экране только X) и с 3 с (X и Y, Y встал над X) в 4.5 с (X, Z, Y).
    let from_a_quiet_start = jump_from(900, 1_000, 4_500);
    let from_a_busy_moment = jump_from(2_500, 3_000, 4_500);

    assert_eq!(from_a_quiet_start.len(), 3, "X, Z and Y are on screen");
    assert_eq!(from_a_quiet_start, from_a_busy_moment);
}
