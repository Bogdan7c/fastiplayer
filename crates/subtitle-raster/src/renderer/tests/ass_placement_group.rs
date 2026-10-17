//! Группа размещения картинок: какие реплики показ может поднимать над панелью управления.

use super::ass::{glyph_ink, render_ass, single};
use super::*;
use crate::overlay::OverlayPlacementGroup;

/// Группы всех картинок результата, без повторов.
fn groups_of(overlay: &SubtitleOverlay) -> Vec<OverlayPlacementGroup> {
    let mut groups: Vec<OverlayPlacementGroup> = Vec::new();
    for bitmap in overlay.bitmaps() {
        if !groups.contains(&bitmap.placement_group()) {
            groups.push(bitmap.placement_group());
        }
    }
    groups
}

fn groups_for_ass(text: &str) -> Vec<OverlayPlacementGroup> {
    let overlay = render_ass(&mut rasterizer(ROOMY_LIMITS), &single(text));
    assert!(!overlay.is_empty(), "реплика {text:?} должна нарисоваться");
    groups_of(&overlay)
}

#[test]
fn unpositioned_bottom_dialogue_is_main() {
    assert_eq!(groups_for_ass("Hello"), [OverlayPlacementGroup::Main]);
    assert_eq!(
        groups_for_ass(r"{\an1}Hello"),
        [OverlayPlacementGroup::Main]
    );
    assert_eq!(
        groups_for_ass(r"{\an3}Hello"),
        [OverlayPlacementGroup::Main]
    );
}

#[test]
fn pos_move_and_top_middle_alignment_are_fixed() {
    assert_eq!(
        groups_for_ass(r"{\pos(300,200)}Sign"),
        [OverlayPlacementGroup::Fixed]
    );
    assert_eq!(
        groups_for_ass(r"{\move(100,100,300,300)}Sign"),
        [OverlayPlacementGroup::Fixed]
    );
    assert_eq!(groups_for_ass(r"{\an8}Top"), [OverlayPlacementGroup::Fixed]);
    assert_eq!(groups_for_ass(r"{\an5}Mid"), [OverlayPlacementGroup::Fixed]);
    // Нижнее выравнивание с \pos — всё равно привязка автора.
    assert_eq!(
        groups_for_ass(r"{\an2\pos(640,600)}Sign"),
        [OverlayPlacementGroup::Fixed]
    );
}

#[test]
fn group_bounds_cover_only_their_group_and_union_to_content_bounds() {
    let file = crate::test_ass::ass_file(
        (1280, 720),
        true,
        &[crate::test_ass::DEFAULT_STYLE_LINE],
        &[
            &crate::test_ass::dialogue(0, "Dialogue line"),
            &crate::test_ass::dialogue(0, r"{\pos(200,100)}Sign"),
        ],
    );
    let cues = crate::test_ass::load_ass_cues(&file, super::ass::fresh_source());
    let overlay = super::ass::render_ass(&mut rasterizer(ROOMY_LIMITS), &cues);

    let main = overlay
        .group_bounds(OverlayPlacementGroup::Main)
        .expect("в слое есть основной диалог");
    let fixed = overlay
        .group_bounds(OverlayPlacementGroup::Fixed)
        .expect("в слое есть фиксированная надпись");
    let all = overlay.content_bounds().expect("слой не пуст");
    // Надпись у (200,100) выше диалога у нижнего поля: группы не пересекаются по вертикали.
    assert!(
        fixed.y + fixed.height as i32 <= main.y,
        "{fixed:?} {main:?}"
    );
    assert_eq!(all.y, fixed.y);
    assert_eq!(all.y + all.height as i32, main.y + main.height as i32);
    assert!(glyph_ink(&overlay).3 > 0);
}

#[test]
fn group_bounds_is_none_for_absent_group() {
    let overlay = render_ass(&mut rasterizer(ROOMY_LIMITS), &single("Hello"));
    assert!(overlay.group_bounds(OverlayPlacementGroup::Fixed).is_none());
    assert_eq!(
        overlay.group_bounds(OverlayPlacementGroup::Main),
        overlay.content_bounds()
    );
}

#[test]
fn text_cues_are_main() {
    let test_cues = cues(1, &["Plain SRT line"]);
    let overlay = render_once(
        &mut rasterizer(ROOMY_LIMITS),
        &test_cues,
        &SubtitleAppearance::youtube_like(),
    );
    assert!(!overlay.is_empty());
    assert_eq!(groups_of(&overlay), [OverlayPlacementGroup::Main]);
}
