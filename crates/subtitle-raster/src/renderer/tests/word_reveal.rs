//! Слова с `TextSpan::reveal_at`: до момента появления не рисуются и не занимают места.

use super::*;
use crate::overlay::{AnimationChange, OverlayChange};

/// Реплика «Привет» + « мир», где «мир» появляется в `reveal_millis` (`None` — сразу).
fn greeting_with_reveal(source: u64, reveal_millis: Option<i64>) -> TestCue {
    let mut test_cue = cues(source, &["x"]).remove(0);
    let mut second = TextSpan::text(" мир", SpanStyle::default());
    second.reveal_at = reveal_millis.map(SubtitleTime::from_millis);
    test_cue.cue.text =
        StyledText::new(vec![TextSpan::text("Привет", SpanStyle::default()), second]);
    test_cue
}

/// Рисует реплику на момент `moment_millis` в уже существующем растеризаторе.
fn render_at(
    rasterizer: &mut SubtitleRasterizer,
    test_cue: &TestCue,
    moment_millis: i64,
) -> SubtitleOverlay {
    let list = renderables(std::slice::from_ref(test_cue));
    rasterizer
        .render(&RenderRequest {
            moment: SubtitleTime::from_millis(moment_millis),
            cues: &list,
            canvas: canvas(),
            appearance: &SubtitleAppearance::youtube_like(),
            text_language: crate::fonts::TextLanguagePreference::unknown(),
            ass: crate::ass::AssRenderOptions::default(),
        })
        .clone()
}

fn ink_width(overlay: &SubtitleOverlay) -> i32 {
    let (left, _, right, _) = ink_bounds(overlay.bitmaps()).expect("cue has ink");
    right - left
}

#[test]
fn unrevealed_word_is_not_drawn_and_takes_no_layout_space() {
    let mut rasterizer = rasterizer(ROOMY_LIMITS);
    let test_cue = greeting_with_reveal(1, Some(1000));

    let before = render_at(&mut rasterizer, &test_cue, 0);
    let after = render_at(&mut rasterizer, &test_cue, 1500);

    // Одно слово заметно уже, чем два.
    assert!(
        ink_width(&before) < ink_width(&after),
        "before reveal {} px, after {} px",
        ink_width(&before),
        ink_width(&after)
    );
    // Текст центрирован: когда слово появилось, строка выросла в обе стороны.
    let (left_before, ..) = ink_bounds(before.bitmaps()).expect("ink");
    let (left_after, ..) = ink_bounds(after.bitmaps()).expect("ink");
    assert!(left_after < left_before, "the line grows around its center");
}

#[test]
fn fully_revealed_cue_is_drawn_exactly_like_a_cue_without_reveal_times() {
    let with_reveal = greeting_with_reveal(1, Some(1000));
    let plain = greeting_with_reveal(1, None);
    let mut first = rasterizer(ROOMY_LIMITS);
    let mut second = rasterizer(ROOMY_LIMITS);

    let revealed = render_at(&mut first, &with_reveal, 1000);
    let expected = render_at(&mut second, &plain, 0);

    assert_eq!(revealed.bitmaps().len(), expected.bitmaps().len());
    for (got, want) in revealed.bitmaps().iter().zip(expected.bitmaps()) {
        assert_eq!(got.origin(), want.origin());
        assert_eq!(got.coverage(), want.coverage());
    }
}

#[test]
fn frame_stays_valid_until_next_reveal_and_redraws_exactly_there() {
    let mut rasterizer = rasterizer(ROOMY_LIMITS);
    let test_cue = greeting_with_reveal(1, Some(1000));

    let first = render_at(&mut rasterizer, &test_cue, 0);
    assert_eq!(
        first.animation_change(),
        AnimationChange::At(SubtitleTime::from_millis(1000)),
        "the picture is valid exactly until the word appears"
    );
    let drawn_before = rasterizer.statistics().rasterized_cues;

    // До появления слова кадр переиспользуется без работы.
    let still = render_at(&mut rasterizer, &test_cue, 999);
    assert_eq!(still.change(), OverlayChange::Unchanged);
    assert_eq!(rasterizer.statistics().rasterized_cues, drawn_before);

    // В момент появления картинка меняется и больше ни от чего не зависит.
    let revealed = render_at(&mut rasterizer, &test_cue, 1000);
    assert_eq!(revealed.change(), OverlayChange::Replaced);
    assert_eq!(revealed.animation_change(), AnimationChange::None);
    assert!(ink_width(&revealed) > ink_width(&first));

    // Перемотка назад скрывает слово обратно (а не отдаёт прежний кадр).
    let rewound = render_at(&mut rasterizer, &test_cue, 0);
    assert_eq!(ink_width(&rewound), ink_width(&first));
}

#[test]
fn hidden_line_break_and_words_leave_no_blank_line() {
    let mut rasterizer = rasterizer(ROOMY_LIMITS);
    let mut test_cue = cues(1, &["x"]).remove(0);
    let mut line_break = TextSpan::line_break();
    line_break.reveal_at = Some(SubtitleTime::from_millis(1000));
    let mut second_line = TextSpan::text("вторая", SpanStyle::default());
    second_line.reveal_at = Some(SubtitleTime::from_millis(1000));
    test_cue.cue.text = StyledText::new(vec![
        TextSpan::text("первая", SpanStyle::default()),
        line_break,
        second_line,
    ]);

    let one_line = render_at(&mut rasterizer, &test_cue, 0);
    let two_lines = render_at(&mut rasterizer, &test_cue, 1000);

    let height = |overlay: &SubtitleOverlay| {
        let (_, top, _, bottom) = ink_bounds(overlay.bitmaps()).expect("ink");
        bottom - top
    };
    assert!(height(&two_lines) > height(&one_line) * 3 / 2);
}
