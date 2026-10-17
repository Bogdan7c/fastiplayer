//! Эффект `Banner` через настоящий поток-рисовальщик: бегущая строка идёт кадр за кадром, каждый
//! кадр получает картинку СВОЕГО времени, а положение только продвигается влево на ожидаемый шаг.

use subtitle_raster::SubtitleOverlay;

use super::super::SubtitleOwner;
use super::ass_animation_support::*;
use super::render_thread::{FRAME_MS, GENERATION, HEIGHT, WIDTH, free_running};
use super::*;

/// Текст бегущей строки: кегль 100, без обводки (стиль теста), по центру кадра.
const BANNER_TEXT: &str = "{\\fs100}BANNER";

/// 10 мс на пиксель: за кадр в 40 мс строка проходит ровно 4 px.
const BANNER_EFFECT: &str = "Banner;10;0;0";

/// Пикселей за кадр при задержке `BANNER_EFFECT`.
const STEP_PER_FRAME: i32 = 4;

fn script_of(effect: &str, text: &str) -> String {
    animated_script(
        (SCRIPT_WIDTH, SCRIPT_HEIGHT),
        &[(
            "0:00:00.00".to_owned(),
            "0:01:00.00".to_owned(),
            text.to_owned(),
        )],
    )
    // Поле `Effect` — предпоследнее в строке `Dialogue:`.
    .replace("Default,,0,0,0,,", &format!("Default,,0,0,0,{effect},"))
}

/// Левый край картинок реплики в overlay (`None` — картинок нет).
fn left_edge(overlay: &SubtitleOverlay) -> Option<i32> {
    overlay
        .bitmaps()
        .iter()
        .map(|bitmap| bitmap.origin().x)
        .min()
}

/// Один кадр: выбор картинки как в UI.
fn left_edge_at(owner: &mut SubtitleOwner, media: MediaInstanceId, pts_ms: u64) -> Option<i32> {
    let rect = full_rect(WIDTH, HEIGHT);
    owner
        .overlay_for_frame(current(media, GENERATION), &frame(pts_ms, GENERATION, rect))
        .and_then(left_edge)
}

#[test]
fn banner_plays_frame_by_frame_and_every_frame_is_its_own_picture() {
    let media = instance(1);
    let (mut owner, _dir) = owner_with_script(media, &script_of(BANNER_EFFECT, BANNER_TEXT));
    free_running(&mut owner);

    // Разогрев: рисовальщик догоняет часы, кадры идут подряд с первой секунды.
    let first_ms = 1_000_u64;
    let warm_up_frames = 6_u64;
    for index in 0..warm_up_frames {
        left_edge_at(&mut owner, media, first_ms + index * FRAME_MS);
        assert!(owner.presenter.wait_for_idle());
    }
    let stats_before = owner.presentation_stats();
    let counters_before = owner.presenter.queue_counters();

    let frames = 14_u64;
    let mut previous: Option<i32> = None;
    for index in warm_up_frames..warm_up_frames + frames {
        let pts_ms = first_ms + index * FRAME_MS;
        let left = left_edge_at(&mut owner, media, pts_ms).expect("the banner is on screen");
        assert!(owner.presenter.wait_for_idle());
        if let Some(previous_left) = previous {
            // Каждый кадр — картинка своего времени: строка продвинулась ровно на шаг кадра.
            assert_eq!(
                previous_left - left,
                STEP_PER_FRAME,
                "frame at {pts_ms} ms shows the position of the frame before: {left}"
            );
        }
        previous = Some(left);
    }

    // Каждый кадр отдан точной картинкой своего времени, опозданий нет.
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
    let diagnostics = owner.presenter.worker_diagnostics().expect("worker");
    assert!(
        diagnostics.raster.rendered_frames >= warm_up_frames + frames,
        "a picture per frame, drew {}",
        diagnostics.raster.rendered_frames
    );
}

#[test]
fn banner_left_to_right_advances_monotonically_to_the_right() {
    let media = instance(1);
    let script = script_of("Banner;10;1;0", BANNER_TEXT);
    let (mut owner, _dir) = owner_with_script(media, &script);
    free_running(&mut owner);

    // Правый край блока стоит в `shift`; к 3 секундам строка целиком на экране. Разогрев:
    // рисовальщик догоняет часы, первые кадры картинки ещё не имеют.
    let first_ms = 3_000_u64;
    let warm_up_frames = 6_u64;
    for index in 0..warm_up_frames {
        left_edge_at(&mut owner, media, first_ms + index * FRAME_MS);
        assert!(owner.presenter.wait_for_idle());
    }
    let mut previous: Option<i32> = None;
    for index in warm_up_frames..warm_up_frames + 14 {
        let left = left_edge_at(&mut owner, media, first_ms + index * FRAME_MS)
            .expect("the banner is on screen");
        assert!(owner.presenter.wait_for_idle());
        if let Some(previous_left) = previous {
            assert_eq!(left - previous_left, STEP_PER_FRAME, "frame {index}");
        }
        previous = Some(left);
    }
}
