//! Режим позиции ASS до пикселей: `author` не двигает слой ни панелью, ни позицией пользователя;
//! `whole_layer` двигает целиком, но не выпускает за картинку. Правка на видео (сессия 05) с
//! ASS в режиме `author` двигает только образец. Пропускаются без GPU adapter-а.

use fastiplayer_config::AssPositionMode;

use super::ass_gpu_support::*;
use super::gpu_chain::Harness;
use super::*;
use crate::state::subtitles::appearance_edit::{AppearanceEdit, AppearanceEditBegin};

macro_rules! gpu_or_skip {
    () => {
        match Harness::with_size(WINDOW_WIDTH, WINDOW_HEIGHT) {
            Some(harness) => harness,
            None => {
                eprintln!("skip: no GPU adapter for the ASS position test");
                return;
            }
        }
    };
}

fn config_with(mode: AssPositionMode, vertical_percent: i16) -> SubtitlesConfig {
    SubtitlesConfig {
        ass_position_mode: mode,
        position_vertical_percent: vertical_percent,
        ..enabled_config()
    }
}

/// Пиксели на кадре `pts` с заданным препятствием.
fn render_with_controls(
    owner: &mut SubtitleOwner,
    harness: &mut Harness,
    rect: render_core::PictureRect,
    obstruction: crate::state::subtitles::controls_obstruction::BottomControlsObstruction,
) -> Vec<u8> {
    owner.observe_bottom_controls_obstruction(obstruction);
    render_placed(owner, harness, instance(2), rect, INSIDE_EVENTS_MILLIS)
}

#[test]
fn author_mode_ignores_the_controls_panel_and_the_user_vertical_offset() {
    use crate::state::subtitles::controls_obstruction::BottomControlsObstruction;
    let mut harness = gpu_or_skip!();
    let rect = full_window_picture();
    let mut owner = layer1_owner(&config_with(AssPositionMode::Author, 6), instance(2));
    let baseline = render_with_controls(
        &mut owner,
        &mut harness,
        rect,
        BottomControlsObstruction::Absent,
    );
    assert!(
        bounds_of(&baseline, WINDOW_WIDTH, is_white).is_some(),
        "ASS is drawn"
    );

    let with_panel = render_with_controls(&mut owner, &mut harness, rect, controls_from(240));
    assert_eq!(
        with_panel, baseline,
        "the panel does not lift ASS in author mode"
    );

    let mut moved_owner = layer1_owner(&config_with(AssPositionMode::Author, 40), instance(2));
    let with_offset =
        render_with_controls(&mut moved_owner, &mut harness, rect, controls_from(240));
    assert_eq!(
        with_offset, baseline,
        "the user offset does not move ASS in author mode"
    );
}

#[test]
fn whole_layer_mode_lifts_ass_above_the_controls_and_keeps_it_in_the_picture() {
    use crate::state::subtitles::controls_obstruction::BottomControlsObstruction;
    let mut harness = gpu_or_skip!();
    let rect = full_window_picture();
    let mut owner = layer1_owner(&config_with(AssPositionMode::WholeLayer, 6), instance(2));
    let baseline = render_with_controls(
        &mut owner,
        &mut harness,
        rect,
        BottomControlsObstruction::Absent,
    );
    let baseline_line = bounds_of(&baseline, WINDOW_WIDTH, is_white).expect("line");

    let lifted = render_with_controls(&mut owner, &mut harness, rect, controls_from(300));

    let lifted_line = bounds_of(&lifted, WINDOW_WIDTH, is_white).expect("line");
    assert!(
        lifted_line.max_y + 8 <= baseline_line.max_y,
        "dialogue moves up: {baseline_line:?} -> {lifted_line:?}"
    );
    let lifted_sign = bounds_of(&lifted, WINDOW_WIDTH, is_red).expect("sign");
    assert!(
        lifted_sign.min_y as f32 >= rect.y,
        "the whole layer stays inside the picture: {lifted_sign:?}"
    );
}

#[test]
fn whole_layer_mode_never_paints_on_the_bars_even_with_an_extreme_offset() {
    let mut harness = gpu_or_skip!();
    let rect = letterboxed_window_picture();
    let picture_top = rect.y.round() as u32;
    let picture_bottom = (rect.y + rect.height).round() as u32;
    for vertical_percent in [-40, 95] {
        let mut owner = layer1_owner(
            &config_with(AssPositionMode::WholeLayer, vertical_percent),
            instance(2),
        );
        let pixels = render_placed(
            &mut owner,
            &mut harness,
            instance(2),
            rect,
            INSIDE_EVENTS_MILLIS,
        );
        assert!(
            count_of(&pixels, is_white) > 50,
            "ASS is drawn at {vertical_percent} %"
        );
        for y in (0..picture_top).chain(picture_bottom..WINDOW_HEIGHT) {
            assert!(
                !row_has_non_black(&pixels, y),
                "row {y} is a bar and must stay black at {vertical_percent} %"
            );
        }
    }
}

#[test]
fn edit_mode_in_author_mode_moves_the_sample_but_never_the_ass_layer() {
    let mut harness = gpu_or_skip!();
    let rect = full_window_picture();
    let mut owner = layer1_owner(&config_with(AssPositionMode::Author, 6), instance(2));
    let ass_before = render_placed(
        &mut owner,
        &mut harness,
        instance(2),
        rect,
        INSIDE_EVENTS_MILLIS,
    );
    let sample_before = render_placed(
        &mut owner,
        &mut harness,
        instance(2),
        rect,
        OUTSIDE_EVENTS_MILLIS,
    );
    assert_eq!(owner.begin_appearance_edit(), AppearanceEditBegin::Started);
    let sample_before = {
        // Образец появляется в режиме правки там, где реплик нет.
        let _ = sample_before;
        render_placed(
            &mut owner,
            &mut harness,
            instance(2),
            rect,
            OUTSIDE_EVENTS_MILLIS,
        )
    };
    assert!(
        count_of(&sample_before, is_white) > 50,
        "the sample is shown where no cue is active"
    );

    owner.apply_appearance_edit(AppearanceEdit::MoveBy {
        horizontal_fraction: 0.0,
        vertical_fraction: 0.3,
    });

    let ass_after = render_placed(
        &mut owner,
        &mut harness,
        instance(2),
        rect,
        INSIDE_EVENTS_MILLIS,
    );
    assert_eq!(
        ass_after, ass_before,
        "dragging does not move the ASS layer"
    );
    let sample_after = render_placed(
        &mut owner,
        &mut harness,
        instance(2),
        rect,
        OUTSIDE_EVENTS_MILLIS,
    );
    let before = bounds_of(&sample_before, WINDOW_WIDTH, is_white).expect("sample");
    let after = bounds_of(&sample_after, WINDOW_WIDTH, is_white).expect("sample");
    assert!(
        after.max_y + 20 < before.max_y,
        "the sample moved up: {before:?} -> {after:?}"
    );
}
