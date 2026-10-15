//! Лимиты входа: длина реплики, число реплик в кадре, размер холста.

use super::*;
use crate::overlay::OverlayChange;

fn youtube() -> SubtitleAppearance {
    SubtitleAppearance::youtube_like()
}

fn limits(input: RasterInputLimits) -> RasterLimits {
    RasterLimits {
        cache: ROOMY_LIMITS,
        input,
    }
}

#[test]
fn cue_longer_than_the_limit_is_skipped_whole_and_counted_while_the_others_are_drawn() {
    let mut rasterizer = rasterizer_with_limits(limits(RasterInputLimits {
        max_chars_per_cue: 20,
        ..ROOMY_INPUT
    }));
    let exactly_at_limit = "x".repeat(20);
    let too_long = "y".repeat(21);
    let test_cues = cues(
        1,
        &["Короткая", too_long.as_str(), exactly_at_limit.as_str()],
    );
    let overlay = render_once(&mut rasterizer, &test_cues, &youtube());

    // Рисуются обе допустимые реплики; длинная пропущена целиком, а не обрезана.
    assert_eq!(of_kind(&overlay, SubtitleBitmapKind::Background).len(), 2);
    let statistics = rasterizer.statistics();
    assert_eq!(statistics.rejected_cues_too_long, 1);
    assert_eq!(statistics.failed_cues, 0);
    assert_eq!(statistics.rasterized_cues, 2);

    // Тот же запрос — быстрый путь, длина заново не проверяется и счётчик не растёт.
    let again = render_once(&mut rasterizer, &test_cues, &youtube());
    assert_eq!(again.change(), OverlayChange::Unchanged);
    assert_eq!(rasterizer.statistics().rejected_cues_too_long, 1);
}

#[test]
fn counts_characters_of_all_spans_and_line_breaks_against_the_limit() {
    let mut rasterizer = rasterizer_with_limits(limits(RasterInputLimits {
        max_chars_per_cue: 5,
        ..ROOMY_INPUT
    }));
    // 3 + перевод строки + 3 = 7 символов > 5.
    let overlay = render_once(&mut rasterizer, &cues(1, &["abc\ndef"]), &youtube());
    assert!(overlay.is_empty());
    assert_eq!(rasterizer.statistics().rejected_cues_too_long, 1);
}

#[test]
fn cues_over_the_frame_limit_are_dropped_from_the_end() {
    let mut rasterizer = rasterizer_with_limits(limits(RasterInputLimits {
        max_cues_per_frame: 2,
        ..ROOMY_INPUT
    }));
    let all = cues(1, &["Первая", "Вторая", "Третья", "Четвёртая"]);
    let overlay = render_once(&mut rasterizer, &all, &youtube());
    assert_eq!(of_kind(&overlay, SubtitleBitmapKind::Background).len(), 2);
    let statistics = rasterizer.statistics();
    assert_eq!(statistics.dropped_cues_over_frame_limit, 2);
    assert_eq!(
        statistics.rasterized_cues, 2,
        "dropped cues are never drawn"
    );

    // Те же две первые реплики сами по себе дают тот же кадр: отброшены именно последние.
    let mut reference = rasterizer_with_limits(limits(ROOMY_INPUT));
    let expected = render_once(&mut reference, &all[..2], &youtube());
    assert_eq!(overlay.bitmaps().len(), expected.bitmaps().len());
    for (got, want) in overlay.bitmaps().iter().zip(expected.bitmaps()) {
        assert_eq!(got.origin(), want.origin());
        assert_eq!(got.coverage(), want.coverage());
    }
}

#[test]
fn oversized_canvas_gives_an_empty_overlay_and_replaces_a_visible_one() {
    let mut rasterizer = rasterizer_with_limits(limits(RasterInputLimits {
        max_canvas_side: 1000,
        ..ROOMY_INPUT
    }));
    let test_cues = cues(1, &["Привет"]);
    let small = CanvasSize::new(800, 600).expect("canvas");
    let visible = render_on(&mut rasterizer, &test_cues, &youtube(), small);
    assert!(!visible.is_empty());

    // Сторона ровно по лимиту ещё рисуется.
    let at_limit = CanvasSize::new(1000, 500).expect("canvas");
    assert!(!render_on(&mut rasterizer, &test_cues, &youtube(), at_limit).is_empty());
    assert_eq!(rasterizer.statistics().rejected_canvas, 0);

    let huge = CanvasSize::new(1001, 500).expect("canvas");
    let rejected = render_on(&mut rasterizer, &test_cues, &youtube(), huge);
    assert!(rejected.is_empty());
    assert_eq!(rejected.canvas(), huge);
    assert_eq!(rejected.change(), OverlayChange::Replaced);
    assert!(rejected.revision() > visible.revision());
    assert_eq!(rasterizer.statistics().rejected_canvas, 1);
    assert_eq!(
        rasterizer.statistics().rasterized_cues,
        2,
        "nothing was rasterized on the huge canvas"
    );

    // Высота тоже проверяется.
    let tall = CanvasSize::new(500, 1001).expect("canvas");
    assert!(render_on(&mut rasterizer, &test_cues, &youtube(), tall).is_empty());
    assert_eq!(rasterizer.statistics().rejected_canvas, 2);
}

#[test]
fn oversized_canvas_after_an_empty_frame_stays_unchanged() {
    let mut rasterizer = rasterizer_with_limits(limits(RasterInputLimits {
        max_canvas_side: 100,
        ..ROOMY_INPUT
    }));
    let huge = CanvasSize::new(200, 200).expect("canvas");
    let first = render_on(&mut rasterizer, &cues(1, &["a"]), &youtube(), huge);
    let second = render_on(&mut rasterizer, &cues(2, &["b"]), &youtube(), huge);
    assert!(first.is_empty() && second.is_empty());
    // Было пусто и осталось пусто на том же холсте: перезагружать нечего.
    assert_eq!(second.change(), OverlayChange::Unchanged);
    assert_eq!(second.revision(), first.revision());
}

#[test]
fn very_long_unbreakable_word_stays_inside_the_canvas_without_overflow() {
    let mut rasterizer = rasterizer_with_limits(limits(ROOMY_INPUT));
    let word = "W".repeat(8000);
    let overlay = render_once(&mut rasterizer, &cues(1, &[word.as_str()]), &youtube());
    // Контракт выхода (картинки внутри холста) проверен конструктором оверлея; ошибок нет.
    assert_eq!(rasterizer.statistics().failed_cues, 0);
    for bitmap in overlay.bitmaps() {
        assert!(bitmap.origin().x >= 0 && bitmap.origin().y >= 0);
        assert!(bitmap.origin().x as u32 + bitmap.width() <= WIDTH);
        assert!(bitmap.origin().y as u32 + bitmap.height() <= HEIGHT);
    }
}
