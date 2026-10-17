//! Владелец картинок: каждая картинка кадра знает свою реплику, `retain_cues` режет по ней.

use std::sync::Arc;

use super::ass::{render_ass, single};
use super::*;

fn youtube() -> SubtitleAppearance {
    SubtitleAppearance::youtube_like()
}

/// Ключи владельцев картинок по порядку, подряд идущие повторы схлопнуты.
fn owner_runs(overlay: &SubtitleOverlay) -> Vec<CueKey> {
    let mut runs: Vec<CueKey> = Vec::new();
    for bitmap in overlay.bitmaps() {
        let owner = bitmap
            .cue_key()
            .expect("картинка растеризатора имеет владельца");
        if runs.last() != Some(&owner) {
            runs.push(owner);
        }
    }
    runs
}

#[test]
fn text_bitmaps_are_grouped_by_their_cue_keys() {
    let test_cues = cues(1, &["First line", "Second line"]);
    let overlay = render_once(&mut rasterizer(ROOMY_LIMITS), &test_cues, &youtube());
    assert_eq!(
        owner_runs(&overlay),
        [test_cues[0].key, test_cues[1].key],
        "две реплики: две группы картинок в порядке запроса"
    );
}

#[test]
fn cached_cue_keeps_its_owner_on_the_next_frame() {
    let test_cues = cues(2, &["First", "Second"]);
    let mut rasterizer = rasterizer(ROOMY_LIMITS);
    let _warm_up = render_once(&mut rasterizer, &test_cues, &youtube());
    // Вторая реплика в одиночку: её картинки берутся из кэша и обязаны сохранить владельца.
    let alone = render_once(&mut rasterizer, &test_cues[1..], &youtube());
    assert_eq!(owner_runs(&alone), [test_cues[1].key]);
}

#[test]
fn ass_bitmaps_are_grouped_by_their_cue_keys() {
    let mut ass_cues = single("First");
    ass_cues.extend(single("Second"));
    let overlay = render_ass(&mut rasterizer(ROOMY_LIMITS), &ass_cues);
    let keys: Vec<CueKey> = ass_cues.iter().map(|cue| cue.key).collect();
    let mut owners = owner_runs(&overlay);
    owners.sort_by_key(|key| key.source().get());
    assert_eq!(owners, keys, "обе ASS-реплики узнаются по своим картинкам");
}

#[test]
fn retain_drops_ended_cue_and_shares_masks_of_the_rest() {
    let test_cues = cues(3, &["Staying", "Ending"]);
    let overlay = render_once(&mut rasterizer(ROOMY_LIMITS), &test_cues, &youtube());
    let staying = test_cues[0].key;

    let kept = overlay.retain_cues(|key| key == staying);

    assert_eq!(owner_runs(&kept), [staying]);
    let expected: Vec<&SubtitleBitmap> = overlay
        .bitmaps()
        .iter()
        .filter(|bitmap| bitmap.cue_key() == Some(staying))
        .collect();
    assert_eq!(kept.bitmaps().len(), expected.len());
    for (bitmap, original) in kept.bitmaps().iter().zip(expected) {
        assert!(Arc::ptr_eq(
            bitmap.shared_coverage(),
            original.shared_coverage()
        ));
        assert_eq!(bitmap.origin(), original.origin());
    }
    assert!(kept.bitmaps().len() < overlay.bitmaps().len());
    assert_eq!(kept.canvas(), overlay.canvas());
    assert_eq!(kept.revision(), overlay.revision());
    assert_eq!(
        kept.change(),
        OverlayChange::Replaced,
        "содержимое отличается"
    );
}

#[test]
fn retain_everything_is_identical_and_keeps_change() {
    let test_cues = cues(4, &["One", "Two"]);
    let mut rasterizer = rasterizer(ROOMY_LIMITS);
    let _first = render_once(&mut rasterizer, &test_cues, &youtube());
    let overlay = render_once(&mut rasterizer, &test_cues, &youtube());
    assert_eq!(overlay.change(), OverlayChange::Unchanged);

    let kept = overlay.retain_cues(|_| true);

    assert_eq!(kept.bitmaps().len(), overlay.bitmaps().len());
    assert_eq!(kept.revision(), overlay.revision());
    assert_eq!(kept.change(), OverlayChange::Unchanged);
    for (bitmap, original) in kept.bitmaps().iter().zip(overlay.bitmaps()) {
        assert!(Arc::ptr_eq(
            bitmap.shared_coverage(),
            original.shared_coverage()
        ));
    }
}

#[test]
fn retain_nothing_gives_empty_overlay_on_the_same_canvas() {
    let test_cues = cues(5, &["One", "Two"]);
    let overlay = render_once(&mut rasterizer(ROOMY_LIMITS), &test_cues, &youtube());
    assert!(!overlay.is_empty());

    let kept = overlay.retain_cues(|_| false);

    assert!(kept.is_empty());
    assert_eq!(kept.content_bounds(), None);
    assert_eq!(kept.canvas(), overlay.canvas());
}

#[test]
fn retain_with_keys_absent_from_overlay_changes_nothing() {
    let test_cues = cues(6, &["One", "Two"]);
    let foreign = cues(7, &["Foreign"]);
    let overlay = render_once(&mut rasterizer(ROOMY_LIMITS), &test_cues, &youtube());
    let foreign_key = foreign[0].key;

    // Нужны только чужие реплики: у этого overlay их нет, поэтому не остаётся ничего.
    let only_foreign = overlay.retain_cues(|key| key == foreign_key);
    assert!(only_foreign.is_empty());
    // Чужая реплика в списке «ушедших» ничего не отбрасывает.
    let all_but_foreign = overlay.retain_cues(|key| key != foreign_key);
    assert_eq!(all_but_foreign.bitmaps().len(), overlay.bitmaps().len());
    assert_eq!(all_but_foreign.change(), overlay.change());
}

#[test]
fn retain_does_not_touch_rasterizer_state() {
    let test_cues = cues(8, &["One", "Two"]);
    let mut rasterizer = rasterizer(ROOMY_LIMITS);
    let overlay = render_once(&mut rasterizer, &test_cues, &youtube());
    let before = rasterizer.statistics();
    let _kept = overlay.retain_cues(|_| false);
    let after = rasterizer.statistics();
    assert_eq!(before.rendered_frames, after.rendered_frames);
    assert_eq!(before.rasterized_cues, after.rasterized_cues);
    assert_eq!(before.current_frame_bytes, after.current_frame_bytes);
}

/// Растеризатор и результат уходят в поток-рисовальщик, поэтому обязаны быть `Send`.
#[test]
fn rasterizer_and_overlay_are_send() {
    fn assert_send<T: Send>() {}
    assert_send::<SubtitleRasterizer>();
    assert_send::<SubtitleOverlay>();
}
