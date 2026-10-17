//! Враждебные и пограничные ASS-реплики: ничего не падает, память и время ограничены.

use std::time::{Duration, Instant};

use super::ass::{glyph_ink, render_ass, render_ass_with, single};
use super::*;
use crate::ass::AssRenderOptions;
use crate::test_ass::{DEFAULT_STYLE_LINE, ass_file, dialogue, load_ass_cues};

#[test]
fn comment_only_event_draws_nothing_and_is_not_a_failure() {
    let mut rasterizer = rasterizer(ROOMY_LIMITS);
    let overlay = render_ass(&mut rasterizer, &single(r"{\an8 just a comment}"));
    assert!(overlay.is_empty());
    assert_eq!(rasterizer.statistics().failed_cues, 0);
}

#[test]
fn absurd_font_size_and_border_are_bounded_and_fast() {
    let mut rasterizer = rasterizer(ROOMY_LIMITS);
    let started = Instant::now();
    let overlay = render_ass(
        &mut rasterizer,
        &single(r"{\fs999999999\bord999999999\shad999999999}Big"),
    );
    assert!(
        started.elapsed() < Duration::from_secs(10),
        "huge sizes must not make the renderer crawl"
    );
    // Что бы ни получилось (буква на весь кадр или пропуск реплики), контракт выхода цел:
    // картинки внутри холста, ошибка реплики не паникует.
    for bitmap in overlay.bitmaps() {
        assert!(bitmap.origin().x >= 0 && bitmap.origin().y >= 0);
    }
    let stats = rasterizer.statistics();
    assert_eq!(stats.rasterized_cues + stats.failed_cues, 1);
}

#[test]
fn negative_and_nan_like_numbers_do_not_break_the_frame() {
    let mut rasterizer = rasterizer(ROOMY_LIMITS);
    let overlay = render_ass(
        &mut rasterizer,
        &single(r"{\fs-30\bord-5\shad-5\fsp-50}Still drawn"),
    );
    // Отрицательный размер заменён размером стиля, обводка и \shad зажаты нулём.
    assert!(!overlay.is_empty());
    assert_eq!(rasterizer.statistics().failed_cues, 0);
}

#[test]
fn negative_or_zero_scale_hides_the_letters_without_failing_the_cue() {
    // libass: `\fscx` ниже нуля становится нулём, буква без ширины не рисуется; реплика не
    // ломается, а остальной текст рисуется.
    let mut rasterizer = rasterizer(ROOMY_LIMITS);
    let overlay = render_ass(&mut rasterizer, &single(r"{\fscx-100}Gone"));
    assert!(overlay.is_empty(), "zero-width letters leave no pixels");
    assert_eq!(rasterizer.statistics().failed_cues, 0);
    let overlay = render_ass(&mut rasterizer, &single(r"{\fscy0}Flat{\fscy100}Back"));
    assert!(
        !overlay.is_empty(),
        "the part with normal scale is still drawn"
    );
    assert_eq!(rasterizer.statistics().failed_cues, 0);
}

#[test]
fn too_long_ass_cue_is_rejected_and_counted() {
    let mut rasterizer = rasterizer_with_limits(RasterLimits {
        cache: ROOMY_LIMITS,
        input: RasterInputLimits {
            max_chars_per_cue: 10,
            ..ROOMY_INPUT
        },
    });
    let overlay = render_ass(
        &mut rasterizer,
        &single("This text is longer than ten chars"),
    );
    assert!(overlay.is_empty());
    assert_eq!(rasterizer.statistics().rejected_cues_too_long, 1);
}

#[test]
fn ass_and_text_cues_share_a_frame_text_first() {
    let ass = single(r"{\an8\bord0}From ASS");
    let text = cues(900, &["From SRT"]);
    let mut list = renderables(&text);
    list.extend(ass.iter().map(crate::test_ass::AssTestCue::renderable));
    let mut rasterizer = rasterizer(ROOMY_LIMITS);
    let overlay = rasterizer
        .render(&RenderRequest {
            cues: &list,
            canvas: canvas(),
            appearance: &SubtitleAppearance::youtube_like(),
            text_language: crate::fonts::TextLanguagePreference::unknown(),
            ass: AssRenderOptions::default(),
        })
        .clone();
    // Реплика простого формата идёт первой (плашка стиля YouTube внизу), ASS — после, вверху.
    assert_eq!(overlay.bitmaps()[0].kind(), SubtitleBitmapKind::Background);
    let glyphs = of_kind(&overlay, SubtitleBitmapKind::Glyph);
    assert_eq!(glyphs.len(), 2);
    let text_ink = ink_bounds([glyphs[0]]).expect("text ink");
    let ass_ink = ink_bounds([glyphs[1]]).expect("ass ink");
    assert!(text_ink.1 > 600, "SRT text at the bottom: {text_ink:?}");
    assert!(ass_ink.3 < 200, "\\an8 ASS text at the top: {ass_ink:?}");
}

#[test]
fn many_simultaneous_events_get_a_stack_without_overlaps() {
    let lines: Vec<String> = (0..6)
        .map(|index| dialogue(0, &format!(r"{{\bord0}}Event number {index}")))
        .collect();
    let refs: Vec<&str> = lines.iter().map(String::as_str).collect();
    let file = ass_file((1280, 720), true, &[DEFAULT_STYLE_LINE], &refs);
    let cues = load_ass_cues(&file, 77);
    let overlay = render_ass_with(
        &mut rasterizer(ROOMY_LIMITS),
        &cues,
        AssRenderOptions::default(),
        &SubtitleAppearance::youtube_like(),
        canvas(),
    );
    let mut boxes: Vec<(i32, i32, i32, i32)> = of_kind(&overlay, SubtitleBitmapKind::Glyph)
        .into_iter()
        .map(|bitmap| ink_bounds([bitmap]).expect("ink"))
        .collect();
    assert_eq!(boxes.len(), 6);
    boxes.sort_by_key(|rect| rect.1);
    for pair in boxes.windows(2) {
        assert!(pair[0].3 <= pair[1].1, "overlap: {pair:?}");
    }
    let _ = glyph_ink(&overlay);
}

#[test]
fn extreme_coordinates_and_margins_do_not_panic_and_keep_the_output_contract() {
    let mut rasterizer = rasterizer(ROOMY_LIMITS);
    for tags in [
        r"{\pos(1e300,1e300)}",
        r"{\pos(-99999999999,5)}",
        r"{\an5\pos(1e-300,-1e-300)}",
        r"{\an9\pos(2147483647,2147483647)}",
        r"{\fsp1e300\fscx1e300\fscy1e300\fs1e-300}",
        r"{\fs0.0001\bord1e-9}",
    ] {
        let overlay = render_ass(&mut rasterizer, &single(&format!("{tags}Hello")));
        // Главное — не паника; все картинки обязаны лежать внутри холста (иначе `SubtitleOverlay`
        // стал бы пустым и все картинки пропали бы).
        let stats = rasterizer.statistics();
        assert_eq!(
            stats.rasterized_cues + stats.failed_cues,
            stats.cue_cache.misses,
            "{tags}"
        );
        for bitmap in overlay.bitmaps() {
            assert!(bitmap.width() > 0 && bitmap.height() > 0, "{tags}");
        }
    }
}

#[test]
fn huge_event_margins_and_play_res_do_not_panic() {
    let style = DEFAULT_STYLE_LINE;
    let file = ass_file(
        (2_000_000_000, 2_000_000_000),
        true,
        &[style],
        &["Dialogue: 0,0:00:00.00,0:00:10.00,Default,,2000000000,2000000000,2000000000,,Margins"],
    );
    let overlay = render_ass(&mut rasterizer(ROOMY_LIMITS), &load_ass_cues(&file, 5));
    for bitmap in overlay.bitmaps() {
        assert!(bitmap.width() > 0);
    }
}

/// Событие слоя `layer` с текстом в заданной точке (чтобы события не двигали друг друга).
fn placed_event(layer: i32, x: i32, text: &str) -> String {
    dialogue(layer, &format!(r"{{\an5\pos({x},300)\bord0}}{text}"))
}

#[test]
fn ass_events_over_the_limit_drop_the_highest_layers_not_the_latest_in_the_timeline() {
    let limits = RasterLimits {
        cache: ROOMY_LIMITS,
        input: RasterInputLimits {
            max_ass_events_per_frame: 2,
            ..ROOMY_INPUT
        },
    };
    // Порядок в файле: слой 5 первым; выжить должны два события слоя 0, а не первые два.
    let all_lines = [
        placed_event(5, 200, "TOP"),
        placed_event(0, 500, "LOW1"),
        placed_event(0, 800, "LOW2"),
        placed_event(1, 1100, "MID"),
    ];
    let refs: Vec<&str> = all_lines.iter().map(String::as_str).collect();
    let all = load_ass_cues(
        &ass_file((1280, 720), true, &[DEFAULT_STYLE_LINE], &refs),
        901,
    );
    let mut limited = rasterizer_with_limits(limits);
    let overlay = render_ass(&mut limited, &all);

    let reference_lines = [placed_event(0, 500, "LOW1"), placed_event(0, 800, "LOW2")];
    let reference_refs: Vec<&str> = reference_lines.iter().map(String::as_str).collect();
    let reference_cues = load_ass_cues(
        &ass_file((1280, 720), true, &[DEFAULT_STYLE_LINE], &reference_refs),
        902,
    );
    let expected = render_ass(&mut rasterizer(ROOMY_LIMITS), &reference_cues);

    assert!(!expected.bitmaps().is_empty());
    assert_eq!(overlay.bitmaps().len(), expected.bitmaps().len());
    for (got, want) in overlay.bitmaps().iter().zip(expected.bitmaps()) {
        assert_eq!(got.origin(), want.origin());
        assert_eq!(got.coverage(), want.coverage());
    }
    let statistics = limited.statistics();
    assert_eq!(statistics.dropped_ass_events_over_frame_limit, 2);
    assert_eq!(statistics.dropped_cues_over_frame_limit, 0);
    assert_eq!(
        statistics.rasterized_cues, 2,
        "dropped events are never drawn"
    );
}
