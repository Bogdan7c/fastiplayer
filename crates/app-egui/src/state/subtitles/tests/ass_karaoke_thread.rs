//! Караоке через настоящий поток-рисовальщик: строка поётся кадр за кадром, каждый кадр получает
//! картинку СВОЕГО времени с границей заливки, которая только продвигается вправо, а буквы
//! растеризуются один раз; допетая строка больше не перерисовывается.

use subtitle_raster::{SubtitleBitmapKind, SubtitleOverlay};

use super::super::SubtitleOwner;
use super::ass_animation_support::*;
use super::render_thread::{FRAME_MS, GENERATION, HEIGHT, WIDTH, free_running};
use super::*;

/// Слог `WIPE` (кегль 150) длится секунду и начинается через 200 мс: за кадр в 40 мс граница
/// проходит около 10 px по буквам шириной ~350 px.
const SING_TEXT: &str =
    "{\\an7\\pos(100,100)\\bord0\\fs150\\1c&H0000FF&\\2c&H00FF00&\\kf20\\kf100}WIPE";

fn script_of(text: &str) -> String {
    animated_script(
        (SCRIPT_WIDTH, SCRIPT_HEIGHT),
        &[(
            "0:00:00.00".to_owned(),
            "0:01:00.00".to_owned(),
            text.to_owned(),
        )],
    )
}

/// Граница заливки картинки: левый край правой (неспетой) половины. `None` — слог ещё не начат
/// или уже допет (картинка одна).
fn split_of(overlay: &SubtitleOverlay) -> Option<i32> {
    let glyphs: Vec<_> = overlay
        .bitmaps()
        .iter()
        .filter(|bitmap| bitmap.kind() == SubtitleBitmapKind::Glyph)
        .collect();
    match glyphs.as_slice() {
        [_left, right] => Some(right.origin().x),
        _ => None,
    }
}

/// Один кадр: выбор картинки как в UI.
fn split_at(owner: &mut SubtitleOwner, media: MediaInstanceId, pts_ms: u64) -> Option<i32> {
    let rect = full_rect(WIDTH, HEIGHT);
    owner
        .overlay_for_frame(current(media, GENERATION), &frame(pts_ms, GENERATION, rect))
        .and_then(split_of)
}

#[test]
fn karaoke_sings_frame_by_frame_with_a_single_rasterization() {
    let media = instance(1);
    let (mut owner, _dir) = owner_with_script(media, &script_of(SING_TEXT));
    free_running(&mut owner);

    // Разогрев: рисовальщик догоняет часы, кадры идут подряд с середины слога.
    let first_ms = 280_u64;
    let warm_up_frames = 6_u64;
    for index in 0..warm_up_frames {
        split_at(&mut owner, media, first_ms + index * FRAME_MS);
        assert!(owner.presenter.wait_for_idle());
    }
    let stats_before = owner.presentation_stats();
    let counters_before = owner.presenter.queue_counters();

    let frames = 14_u64;
    let mut previous: Option<i32> = None;
    for index in warm_up_frames..warm_up_frames + frames {
        let pts_ms = first_ms + index * FRAME_MS;
        let split = split_at(&mut owner, media, pts_ms).expect("mid-syllable frames are split");
        assert!(owner.presenter.wait_for_idle());
        if let Some(previous_split) = previous {
            assert!(
                split > previous_split,
                "frame at {pts_ms} ms shows the split of the frame before: {split}"
            );
            // За 40 мс граница проходит ~14 px (350 px за секунду): картинка не «отстаёт».
            assert!(
                split - previous_split < 40,
                "frame at {pts_ms} ms jumped from {previous_split} to {split}"
            );
        }
        previous = Some(split);
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

    // Картинка на каждый кадр своя, а буквы растеризованы ровно один раз.
    let diagnostics = owner.presenter.worker_diagnostics().expect("worker");
    assert_eq!(
        diagnostics.raster.rasterized_cues, 1,
        "the fill moves, the letters stay in the cue cache"
    );
    assert!(
        diagnostics.raster.rendered_frames >= warm_up_frames + frames,
        "a picture per frame, drew {}",
        diagnostics.raster.rendered_frames
    );
    assert_eq!(diagnostics.repeated_jobs_suppressed, 0);
}

#[test]
fn a_fully_sung_line_stops_being_redrawn() {
    let media = instance(1);
    let (mut owner, _dir) = owner_with_script(media, &script_of(SING_TEXT));
    free_running(&mut owner);

    // Слог допет к 1.2 с; кадры со 2 с — статичная сцена.
    let first_ms = 2_000_u64;
    for index in 0..12_u64 {
        split_at(&mut owner, media, first_ms + index * FRAME_MS);
        assert!(owner.presenter.wait_for_idle());
    }
    let settled = owner.presenter.worker_diagnostics().expect("worker");
    for index in 12..62_u64 {
        let pts_ms = first_ms + index * FRAME_MS;
        assert_eq!(
            split_at(&mut owner, media, pts_ms),
            None,
            "one sung picture"
        );
        assert!(owner.presenter.wait_for_idle());
    }
    let later = owner.presenter.worker_diagnostics().expect("worker");
    assert_eq!(
        later.raster.rendered_frames, settled.raster.rendered_frames,
        "a sung line is a static scene: nothing is drawn any more"
    );
    assert_eq!(later.raster.rasterized_cues, 1);
}

#[test]
fn an_unsung_line_waits_without_redrawing_until_the_syllable_starts() {
    let media = instance(1);
    // Слог начнётся через 6 секунд: до него картинка стоит и не рисуется заново.
    let late_text =
        "{\\an7\\pos(100,100)\\bord0\\fs150\\1c&H0000FF&\\2c&H00FF00&\\kf600\\kf100}WIPE";
    let (mut owner, _dir) = owner_with_script(media, &script_of(late_text));
    free_running(&mut owner);

    let first_ms = 500_u64;
    for index in 0..12_u64 {
        split_at(&mut owner, media, first_ms + index * FRAME_MS);
        assert!(owner.presenter.wait_for_idle());
    }
    let settled = owner.presenter.worker_diagnostics().expect("worker");
    for index in 12..62_u64 {
        split_at(&mut owner, media, first_ms + index * FRAME_MS);
        assert!(owner.presenter.wait_for_idle());
    }
    let later = owner.presenter.worker_diagnostics().expect("worker");
    assert_eq!(
        later.raster.rendered_frames, settled.raster.rendered_frames,
        "before the syllable the picture is valid until its start"
    );
}
