//! Режим `main_lines_avoid_controls` (по умолчанию) до пикселей: основной диалог поднимается
//! над панелью, надпись с `\pos` стоит, при скрытой панели кадр равен авторскому, сдвиг
//! пользователя к ASS не применяется, на полосы ничего не выходит. Пропускаются без GPU.

use fastiplayer_config::AssPositionMode;

use super::ass_gpu_support::*;
use super::gpu_chain::Harness;
use super::*;
use crate::state::subtitles::controls_obstruction::BottomControlsObstruction;

macro_rules! gpu_or_skip {
    () => {
        match Harness::with_size(WINDOW_WIDTH, WINDOW_HEIGHT) {
            Some(harness) => harness,
            None => {
                eprintln!("skip: no GPU adapter for the ASS lift test");
                return;
            }
        }
    };
}

fn owner_in_mode(mode: AssPositionMode, vertical_percent: i16) -> SubtitleOwner {
    let config = SubtitlesConfig {
        ass_position_mode: mode,
        position_vertical_percent: vertical_percent,
        ..enabled_config()
    };
    layer1_owner(&config, instance(2))
}

/// Кадр `INSIDE_EVENTS_MILLIS` при заданной панели.
fn render_with_panel(
    owner: &mut SubtitleOwner,
    harness: &mut Harness,
    rect: render_core::PictureRect,
    obstruction: BottomControlsObstruction,
) -> Vec<u8> {
    owner.observe_bottom_controls_obstruction(obstruction);
    render_placed(owner, harness, instance(2), rect, INSIDE_EVENTS_MILLIS)
}

/// Только красные пиксели кадра (остальное закрашено): образ надписи для сравнения «не двигалась».
fn red_pixels_only(pixels: &[u8]) -> Vec<[u8; 4]> {
    pixels
        .as_chunks::<4>()
        .0
        .iter()
        .map(|pixel| if is_red(*pixel) { *pixel } else { [0; 4] })
        .collect()
}

/// Белые и жёлтые пиксели (две нижние реплики фикстуры).
fn is_dialogue(pixel: [u8; 4]) -> bool {
    is_white(pixel) || is_yellow(pixel)
}

#[test]
fn default_config_is_the_lift_mode() {
    assert_eq!(
        SubtitlesConfig::default().ass_position_mode,
        AssPositionMode::MainLinesAvoidControls
    );
}

#[test]
fn panel_lifts_bottom_dialogue_above_it_while_the_sign_stays_pixel_identical() {
    let mut harness = gpu_or_skip!();
    let rect = full_window_picture();
    let mut owner = owner_in_mode(AssPositionMode::MainLinesAvoidControls, 6);
    let baseline = render_with_panel(
        &mut owner,
        &mut harness,
        rect,
        BottomControlsObstruction::Absent,
    );
    let dialogue = bounds_of(&baseline, WINDOW_WIDTH, is_dialogue).expect("dialogue is drawn");
    assert!(
        bounds_of(&baseline, WINDOW_WIDTH, is_red).is_some(),
        "sign is drawn"
    );
    // Панель закрывает нижние ~40 px диалога.
    let panel_top = dialogue.max_y as i32 - 40;

    let with_panel = render_with_panel(&mut owner, &mut harness, rect, controls_from(panel_top));

    let lifted = bounds_of(&with_panel, WINDOW_WIDTH, is_dialogue).expect("dialogue stays");
    assert!(
        (lifted.max_y as i32) < panel_top,
        "dialogue is above the panel: {lifted:?}, panel at {panel_top}"
    );
    assert_eq!(
        lifted.height(),
        dialogue.height(),
        "both lines move as one block"
    );
    assert_eq!(
        red_pixels_only(&with_panel),
        red_pixels_only(&baseline),
        "the \\pos sign is pixel-identical"
    );
}

#[test]
fn hidden_panel_returns_the_frame_to_the_exact_author_placement() {
    let mut harness = gpu_or_skip!();
    let rect = full_window_picture();
    let mut author = owner_in_mode(AssPositionMode::Author, 6);
    let author_frame = render_with_panel(
        &mut author,
        &mut harness,
        rect,
        BottomControlsObstruction::Absent,
    );
    let mut owner = owner_in_mode(AssPositionMode::MainLinesAvoidControls, 6);
    let hidden = render_with_panel(
        &mut owner,
        &mut harness,
        rect,
        BottomControlsObstruction::Absent,
    );
    assert_eq!(hidden, author_frame, "panel hidden: equals author mode");

    // Панель выехала и уехала: конечное состояние снова авторское.
    let dialogue = bounds_of(&author_frame, WINDOW_WIDTH, is_dialogue).expect("dialogue");
    let shown = render_with_panel(
        &mut owner,
        &mut harness,
        rect,
        controls_from(dialogue.max_y as i32 - 40),
    );
    assert_ne!(shown, author_frame, "panel visible moves the dialogue");
    let hidden_again = render_with_panel(
        &mut owner,
        &mut harness,
        rect,
        BottomControlsObstruction::Absent,
    );
    assert_eq!(hidden_again, author_frame, "returns to the author position");
}

#[test]
fn lift_follows_the_panel_edge_smoothly() {
    let mut harness = gpu_or_skip!();
    let rect = full_window_picture();
    let mut owner = owner_in_mode(AssPositionMode::MainLinesAvoidControls, 6);
    let baseline = render_with_panel(
        &mut owner,
        &mut harness,
        rect,
        BottomControlsObstruction::Absent,
    );
    let bottom = bounds_of(&baseline, WINDOW_WIDTH, is_dialogue)
        .expect("dialogue")
        .max_y as i32;
    // Панель выезжает снизу вверх: каждая следующая позиция выше предыдущей на 6 px.
    let bottoms: Vec<u32> = (0..6)
        .map(|step| {
            let panel_top = bottom + 8 + 6 - step * 6;
            let frame = render_with_panel(&mut owner, &mut harness, rect, controls_from(panel_top));
            bounds_of(&frame, WINDOW_WIDTH, is_dialogue)
                .expect("dialogue")
                .max_y
        })
        .collect();
    assert!(
        bottoms.windows(2).all(|pair| pair[1] <= pair[0]),
        "monotonic: {bottoms:?}"
    );
    assert!(bottoms[5] + 20 <= bottoms[0], "really moved: {bottoms:?}");
}

#[test]
fn user_vertical_offset_does_not_move_ass_in_lift_mode() {
    let mut harness = gpu_or_skip!();
    let rect = full_window_picture();
    for obstruction in [BottomControlsObstruction::Absent, controls_from(250)] {
        let mut default_position = owner_in_mode(AssPositionMode::MainLinesAvoidControls, 6);
        let mut moved_position = owner_in_mode(AssPositionMode::MainLinesAvoidControls, 40);
        let expected = render_with_panel(&mut default_position, &mut harness, rect, obstruction);
        let actual = render_with_panel(&mut moved_position, &mut harness, rect, obstruction);
        assert_eq!(actual, expected, "offset ignored with {obstruction:?}");
    }
}

#[test]
fn nothing_is_painted_on_the_bars_even_with_a_huge_panel() {
    let mut harness = gpu_or_skip!();
    let rect = letterboxed_window_picture();
    let picture_top = rect.y.round() as u32;
    let picture_bottom = (rect.y + rect.height).round() as u32;
    for panel_top in [i32::MAX / 2, picture_bottom as i32 - 20, 0] {
        let mut owner = owner_in_mode(AssPositionMode::MainLinesAvoidControls, 6);
        let pixels = render_with_panel(&mut owner, &mut harness, rect, controls_from(panel_top));
        assert!(
            count_of(&pixels, is_dialogue) > 50,
            "ASS drawn at {panel_top}"
        );
        for y in (0..picture_top).chain(picture_bottom..WINDOW_HEIGHT) {
            assert!(
                !row_has_non_black(&pixels, y),
                "row {y} is a bar and must stay black (panel at {panel_top})"
            );
        }
    }
}

#[test]
fn whole_layer_and_author_modes_are_unchanged_by_the_new_default() {
    let mut harness = gpu_or_skip!();
    let rect = full_window_picture();
    let mut whole = owner_in_mode(AssPositionMode::WholeLayer, 6);
    let mut author = owner_in_mode(AssPositionMode::Author, 6);
    let panel = controls_from(250);
    let whole_frame = render_with_panel(&mut whole, &mut harness, rect, panel);
    let author_frame = render_with_panel(&mut author, &mut harness, rect, panel);
    // whole_layer поднимает диалог, author не двигает ничего.
    let dialogue_author = bounds_of(&author_frame, WINDOW_WIDTH, is_dialogue).expect("dialogue");
    let dialogue_whole = bounds_of(&whole_frame, WINDOW_WIDTH, is_dialogue).expect("dialogue");
    assert!(dialogue_whole.max_y < dialogue_author.max_y);
}
