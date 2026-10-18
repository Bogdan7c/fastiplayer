//! Память столкновений ASS и разрыв времени: после seek/scrub положение реплик в целевом
//! моменте не зависит от того, откуда прыгнули; при непрерывном ходе память libass сохраняется.

use std::collections::BTreeMap;

use super::ass::fresh_source;
use super::*;
use crate::test_ass::{AssTestCue, DEFAULT_STYLE_LINE, ass_file, dialogue, load_ass_cues};

/// Три нижние реплики одного слоя в порядке файла X, Z, Y. По времени: X 0-10 с, Z 4-10 с,
/// Y 2-5 с (время в тестах задаёт набор реплик в запросе).
fn x_z_y() -> Vec<AssTestCue> {
    let file = ass_file(
        (1280, 720),
        true,
        &[DEFAULT_STYLE_LINE],
        &[
            &dialogue(0, r"{\bord0}Line X"),
            &dialogue(0, r"{\bord0}Line Z"),
            &dialogue(0, r"{\bord0}Line Y"),
        ],
    );
    load_ass_cues(&file, fresh_source())
}

/// Рисует выбранные реплики (номера в порядке файла); результат - верхний край картинок
/// каждой реплики по номеру её события.
fn tops(
    rasterizer: &mut SubtitleRasterizer,
    cues: &[AssTestCue],
    numbers: &[usize],
) -> BTreeMap<u32, i32> {
    let list: Vec<RenderableCue<'_>> = numbers
        .iter()
        .filter_map(|number| cues.get(*number))
        .map(AssTestCue::renderable)
        .collect();
    let overlay = rasterizer.render(&RenderRequest {
        moment: subtitle_core::SubtitleTime::ZERO,
        cues: &list,
        canvas: canvas(),
        appearance: &SubtitleAppearance::youtube_like(),
        text_language: crate::fonts::TextLanguagePreference::unknown(),
        ass: crate::ass::AssRenderOptions::default(),
    });
    let mut result: BTreeMap<u32, i32> = BTreeMap::new();
    for bitmap in overlay.bitmaps() {
        if let Some(key) = bitmap.cue_key() {
            let top = bitmap.origin().y;
            result
                .entry(key.event().get())
                .and_modify(|known| *known = (*known).min(top))
                .or_insert(top);
        }
    }
    result
}

/// Непрерывный ход: X одна, затем X и Y (Y выше X), затем X, Z, Y.
fn continuous_play(rasterizer: &mut SubtitleRasterizer, cues: &[AssTestCue]) {
    tops(rasterizer, cues, &[0]);
    tops(rasterizer, cues, &[0, 2]);
}

#[test]
fn a_jump_gives_the_same_positions_wherever_it_came_from() {
    let cues = x_z_y();
    // Прыжок с 1 с (там была только X).
    let mut from_start = rasterizer(ROOMY_LIMITS);
    tops(&mut from_start, &cues, &[0]);
    from_start.forget_layout_history();
    let after_first_jump = tops(&mut from_start, &cues, &[0, 1, 2]);
    // Прыжок с 3 с (непрерывно шли X, затем X и Y).
    let mut from_middle = rasterizer(ROOMY_LIMITS);
    continuous_play(&mut from_middle, &cues);
    from_middle.forget_layout_history();
    let after_second_jump = tops(&mut from_middle, &cues, &[0, 1, 2]);

    assert_eq!(after_first_jump.len(), 3, "all three cues are on screen");
    assert_eq!(after_first_jump, after_second_jump);
}

#[test]
fn continuous_play_keeps_the_libass_style_memory_and_differs_from_a_fresh_start() {
    let cues = x_z_y();
    let mut continuous = rasterizer(ROOMY_LIMITS);
    continuous_play(&mut continuous, &cues);
    let remembered = tops(&mut continuous, &cues, &[0, 1, 2]);
    let mut fresh = rasterizer(ROOMY_LIMITS);
    let from_scratch = tops(&mut fresh, &cues, &[0, 1, 2]);

    assert_ne!(
        remembered, from_scratch,
        "the scenario must expose the history dependence"
    );
}

#[test]
fn forgetting_history_also_discards_a_repeated_identical_frame() {
    let cues = x_z_y();
    let mut rasterizer = rasterizer(ROOMY_LIMITS);
    continuous_play(&mut rasterizer, &cues);
    let remembered = tops(&mut rasterizer, &cues, &[0, 1, 2]);

    rasterizer.forget_layout_history();
    let after_forgetting = tops(&mut rasterizer, &cues, &[0, 1, 2]);

    assert_ne!(
        remembered, after_forgetting,
        "the same request must be laid out again without the old history"
    );
}
