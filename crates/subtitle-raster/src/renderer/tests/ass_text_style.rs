//! Масштаб букв, межбуквенный интервал, синтетические начертания и линии подчёркивания через
//! публичный `SubtitleRasterizer::render`: проверяется то, что получается на холсте.

use subtitle_core::{FontWeight, SpanStyle, StyledText, TextSpan, TextUnderline};

use super::ass::{fresh_source, glyph_ink, render_ass, render_ass_with, single};
use super::*;
use crate::ass::{AssRenderOptions, AssStyleSource};
use crate::test_ass::{DEFAULT_STYLE_LINE, ass_file, dialogue, load_ass_cues};

fn ass_rasterizer() -> SubtitleRasterizer {
    rasterizer(ROOMY_LIMITS)
}

/// Ширина и высота закрашенных пикселей букв реплики `text` стилем автора (без обводки).
fn ink_size(rasterizer: &mut SubtitleRasterizer, text: &str) -> (i32, i32) {
    let overlay = render_ass(rasterizer, &single(text));
    let ink = glyph_ink(&overlay);
    (ink.2 - ink.0, ink.3 - ink.1)
}

/// Среднее значение X закрашенных пикселей букв в строках `rows` (Y холста), взвешенное по маске.
fn mean_ink_x(overlay: &SubtitleOverlay, rows: std::ops::Range<i32>) -> f64 {
    let (mut weighted, mut total) = (0.0, 0.0);
    for bitmap in of_kind(overlay, SubtitleBitmapKind::Glyph) {
        for y in rows.clone() {
            for x in bitmap.origin().x..bitmap.origin().x + bitmap.width() as i32 {
                let value = f64::from(coverage_at(bitmap, x, y));
                weighted += value * f64::from(x);
                total += value;
            }
        }
    }
    assert!(total > 0.0, "no ink in rows {rows:?}");
    weighted / total
}

/// Сумма маски всех картинок вида `kind` (общее «количество чернил»).
fn total_coverage(overlay: &SubtitleOverlay, kind: SubtitleBitmapKind) -> u64 {
    of_kind(overlay, kind)
        .iter()
        .map(|bitmap| {
            (0..bitmap.height())
                .filter_map(|row| bitmap.coverage_row(row))
                .flatten()
                .map(|value| u64::from(*value))
                .sum::<u64>()
        })
        .sum()
}

#[test]
fn fscx_200_doubles_the_width_and_keeps_the_height() {
    let mut rasterizer = ass_rasterizer();
    let (plain_width, plain_height) = ink_size(&mut rasterizer, r"{\bord0}HHHH");
    let (wide_width, wide_height) = ink_size(&mut rasterizer, r"{\bord0\fscx200}HHHH");
    assert!(
        (wide_width - 2 * plain_width).abs() <= 2,
        "plain {plain_width}, doubled {wide_width}"
    );
    assert!((wide_height - plain_height).abs() <= 1);
}

#[test]
fn fscy_50_halves_the_height_and_keeps_the_width() {
    let mut rasterizer = ass_rasterizer();
    let (plain_width, plain_height) = ink_size(&mut rasterizer, r"{\bord0}HHHH");
    let (flat_width, flat_height) = ink_size(&mut rasterizer, r"{\bord0\fscy50}HHHH");
    assert!(
        (flat_height * 2 - plain_height).abs() <= 2,
        "{flat_height} vs {plain_height}"
    );
    assert!((flat_width - plain_width).abs() <= 1);
}

#[test]
fn fsc_without_argument_returns_to_the_style_scale() {
    let mut rasterizer = ass_rasterizer();
    let (plain, _) = ink_size(&mut rasterizer, r"{\bord0}HHHH");
    let (reset, _) = ink_size(&mut rasterizer, r"{\bord0\fscx300\fsc}HHHH");
    assert_eq!(plain, reset, "the reset happens before the first letter");
}

#[test]
fn style_scale_columns_apply_like_the_tags() {
    let scaled_style = "Style: Default,Noto Sans,48,&H00FFFFFF,&H000000FF,&H00000000,&H00000000,0,0,0,0,200,100,0,0,1,0,0,2,40,40,40,1";
    let file = ass_file((1280, 720), true, &[scaled_style], &[&dialogue(0, "HHHH")]);
    let mut rasterizer = ass_rasterizer();
    let overlay = render_ass(&mut rasterizer, &load_ass_cues(&file, fresh_source()));
    let ink = glyph_ink(&overlay);
    let (plain_width, _) = ink_size(&mut rasterizer, r"{\bord0}HHHH");
    assert!((ink.2 - ink.0 - 2 * plain_width).abs() <= 2);
}

#[test]
fn fsp_adds_the_spacing_after_every_letter_but_the_last() {
    let mut rasterizer = ass_rasterizer();
    let (plain, _) = ink_size(&mut rasterizer, r"{\bord0}HHHH");
    let (spaced, _) = ink_size(&mut rasterizer, r"{\bord0\fsp10}HHHH");
    assert!(
        (spaced - (plain + 3 * 10)).abs() <= 2,
        "plain {plain}, spaced {spaced}"
    );
    let (squeezed, _) = ink_size(&mut rasterizer, r"{\bord0\fsp-3}HHHH");
    assert!(
        (squeezed - (plain - 3 * 3)).abs() <= 2,
        "squeezed {squeezed}"
    );
}

#[test]
fn spacing_is_multiplied_by_the_horizontal_scale() {
    let mut rasterizer = ass_rasterizer();
    let (plain, _) = ink_size(&mut rasterizer, r"{\bord0\fscx200}HHHH");
    let (spaced, _) = ink_size(&mut rasterizer, r"{\bord0\fscx200\fsp10}HHHH");
    assert!(
        (spaced - (plain + 3 * 20)).abs() <= 2,
        "plain {plain}, spaced {spaced}"
    );
}

#[test]
fn user_style_ignores_author_scale_and_spacing() {
    let options = AssRenderOptions {
        style_source: AssStyleSource::User,
        ..AssRenderOptions::default()
    };
    let appearance = SubtitleAppearance::outline_and_shadow();
    let mut rasterizer = ass_rasterizer();
    let mut width_of = |text: &str| {
        let overlay = render_ass_with(
            &mut rasterizer,
            &single(text),
            options,
            &appearance,
            canvas(),
        );
        let ink = glyph_ink(&overlay);
        ink.2 - ink.0
    };
    let plain = width_of("HHHH");
    let stretched = width_of(r"{\fscx300\fsp40}HHHH");
    assert!(
        (plain - stretched).abs() <= 1,
        "plain {plain}, with author scale {stretched}"
    );
}

#[test]
fn kerning_header_tightens_kerned_pairs() {
    let render = |kerning: bool| {
        let header_line = if kerning {
            "WrapStyle: 0\nKerning: yes"
        } else {
            "WrapStyle: 0"
        };
        let file = ass_file(
            (1280, 720),
            true,
            &[DEFAULT_STYLE_LINE],
            &[&dialogue(0, r"{\bord0}AVATAR To")],
        )
        .replacen("WrapStyle: 0", header_line, 1);
        let overlay = render_ass(&mut ass_rasterizer(), &load_ass_cues(&file, fresh_source()));
        let ink = glyph_ink(&overlay);
        ink.2 - ink.0
    };
    let (without, with) = (render(false), render(true));
    assert!(
        with + 2 <= without,
        "kerned {with} must be narrower than unkerned {without}"
    );
}

#[test]
fn synthetic_italic_slants_the_top_to_the_right() {
    let mut rasterizer = ass_rasterizer();
    let regular = render_ass(&mut rasterizer, &single(r"{\bord0}HHHH"));
    let italic = render_ass(&mut rasterizer, &single(r"{\bord0\i1}HHHH"));
    let ink = glyph_ink(&regular);
    let (top, bottom) = (ink.1..ink.1 + 4, ink.3 - 4..ink.3);
    let regular_lean = mean_ink_x(&regular, top.clone()) - mean_ink_x(&regular, bottom.clone());
    let ink_italic = glyph_ink(&italic);
    let italic_lean = mean_ink_x(&italic, ink_italic.1..ink_italic.1 + 4)
        - mean_ink_x(&italic, ink_italic.3 - 4..ink_italic.3);
    assert!(
        regular_lean.abs() <= 1.0,
        "upright text does not lean: {regular_lean}"
    );
    assert!(
        italic_lean >= 6.0,
        "italic top is shifted right by {italic_lean} px"
    );
}

#[test]
fn synthetic_bold_thickens_strokes_of_a_regular_only_script() {
    // У армянского Noto только Regular: `\b1` обязан утолстить штрихи, а не остаться прежним.
    let render = |bold: &str| {
        let text = format!("{{\\bord0{bold}}}Բարև աշխարհ");
        let style = "Style: Default,Noto Sans Armenian,56,&H00FFFFFF,&H000000FF,&H00000000,&H00000000,0,0,0,0,100,100,0,0,1,0,0,2,40,40,40,1";
        let file = ass_file((1280, 720), true, &[style], &[&dialogue(0, &text)]);
        render_ass(&mut ass_rasterizer(), &load_ass_cues(&file, fresh_source()))
    };
    let regular = render(r"\b0");
    let bold = render(r"\b1");
    let (regular_ink, bold_ink) = (
        total_coverage(&regular, SubtitleBitmapKind::Glyph),
        total_coverage(&bold, SubtitleBitmapKind::Glyph),
    );
    assert!(
        bold_ink * 100 >= regular_ink * 112,
        "bold coverage {bold_ink} must exceed regular {regular_ink} by 12 %"
    );
    // Метрики не меняются: продвижения те же, поэтому текст не стал шире больше чем на толщину штриха.
    let (regular_box, bold_box) = (glyph_ink(&regular), glyph_ink(&bold));
    assert!((bold_box.2 - bold_box.0) - (regular_box.2 - regular_box.0) <= 6);
}

#[test]
fn bold_face_that_exists_is_not_emboldened_twice() {
    // Noto Sans имеет настоящий Bold: `\b700` и `\b1` дают одну и ту же картинку.
    let mut rasterizer = ass_rasterizer();
    let one = render_ass(&mut rasterizer, &single(r"{\bord0\b1}Hello"));
    let weight = render_ass(&mut rasterizer, &single(r"{\bord0\b700}Hello"));
    assert_eq!(
        total_coverage(&one, SubtitleBitmapKind::Glyph),
        total_coverage(&weight, SubtitleBitmapKind::Glyph)
    );
}

#[test]
fn underline_adds_ink_below_the_baseline_across_spaces() {
    let mut rasterizer = ass_rasterizer();
    let plain = render_ass(&mut rasterizer, &single(r"{\bord0}H H"));
    let underlined = render_ass(&mut rasterizer, &single(r"{\bord0\u1}H H"));
    let (plain_ink, line_ink) = (glyph_ink(&plain), glyph_ink(&underlined));
    assert!(
        line_ink.3 > plain_ink.3 + 2,
        "the line hangs below the letters"
    );
    assert_eq!(line_ink.1, plain_ink.1, "nothing grows upwards");
    // Линия сплошная и под пробелом: в полосе линии нет разрывов от левого до правого края.
    let band_row = line_ink.3 - 2;
    let glyphs = of_kind(&underlined, SubtitleBitmapKind::Glyph);
    for x in line_ink.0 + 2..line_ink.2 - 2 {
        let covered = glyphs
            .iter()
            .any(|bitmap| coverage_at(bitmap, x, band_row) > 200);
        assert!(covered, "gap in the underline at x = {x}");
    }
}

#[test]
fn strike_out_crosses_the_gap_between_words_at_mid_height() {
    let mut rasterizer = ass_rasterizer();
    let plain = render_ass(&mut rasterizer, &single(r"{\bord0}H   H"));
    let struck = render_ass(&mut rasterizer, &single(r"{\bord0\s1}H   H"));
    let ink = glyph_ink(&plain);
    // Линия идёт по всему продвижению (с боковыми зазорами букв), но по высоте не выходит за буквы.
    let struck_ink = glyph_ink(&struck);
    assert_eq!((struck_ink.1, struck_ink.3), (ink.1, ink.3));
    assert!(
        struck_ink.0 < ink.0 && struck_ink.2 > ink.2,
        "the line spans the whole advance"
    );
    // Середина промежутка между буквами: у обычного текста пусто, у зачёркнутого — линия.
    let gap_x = (ink.0 + ink.2) / 2;
    let column_ink = |overlay: &SubtitleOverlay| {
        (ink.1..ink.3)
            .filter(|y| {
                of_kind(overlay, SubtitleBitmapKind::Glyph)
                    .iter()
                    .any(|bitmap| coverage_at(bitmap, gap_x, *y) > 0)
            })
            .count()
    };
    assert_eq!(column_ink(&plain), 0);
    assert!(column_ink(&struck) >= 2, "strike-out line has thickness");
}

#[test]
fn underline_gets_the_same_outline_as_the_letters() {
    let mut rasterizer = ass_rasterizer();
    let bare = render_ass(&mut rasterizer, &single(r"{\bord0\u1}HH"));
    let outlined = render_ass(&mut rasterizer, &single(r"{\bord4\u1}HH"));
    let bare_ink = glyph_ink(&bare);
    let outline_ink = ink_bounds(of_kind(&outlined, SubtitleBitmapKind::Outline)).expect("outline");
    assert!(
        outline_ink.3 >= bare_ink.3 + 3,
        "the border wraps the line too"
    );
}

#[test]
fn srt_underline_span_is_drawn_now() {
    let underlined = StyledText::new(vec![TextSpan::text(
        "HHHH",
        SpanStyle {
            underline: TextUnderline::Single,
            ..SpanStyle::default()
        },
    )]);
    let render_text = |text: StyledText| {
        let test_cue = &mut cues(fresh_source(), &["x"])[0];
        test_cue.cue.text = text;
        let mut rasterizer = ass_rasterizer();
        render_once(
            &mut rasterizer,
            std::slice::from_ref(test_cue),
            &SubtitleAppearance::youtube_like(),
        )
    };
    let plain = render_text(styled("HHHH"));
    let line = render_text(underlined);
    let bottom = |overlay: &SubtitleOverlay| {
        ink_bounds(of_kind(overlay, SubtitleBitmapKind::Glyph))
            .expect("ink")
            .3
    };
    assert!(
        bottom(&line) > bottom(&plain) + 2,
        "underline extends below the letters"
    );
}

#[test]
fn srt_bold_span_on_a_regular_only_script_gets_thicker() {
    let armenian = |weight: FontWeight| {
        StyledText::new(vec![TextSpan::text(
            "Բարև աշխարհ",
            SpanStyle {
                weight,
                ..SpanStyle::default()
            },
        )])
    };
    let render_text = |text: StyledText| {
        let test_cue = &mut cues(fresh_source(), &["x"])[0];
        test_cue.cue.text = text;
        let mut rasterizer = ass_rasterizer();
        render_once(
            &mut rasterizer,
            std::slice::from_ref(test_cue),
            &SubtitleAppearance::youtube_like(),
        )
    };
    let regular = total_coverage(
        &render_text(armenian(FontWeight::Normal)),
        SubtitleBitmapKind::Glyph,
    );
    let bold = total_coverage(
        &render_text(armenian(FontWeight::Bold)),
        SubtitleBitmapKind::Glyph,
    );
    assert!(
        bold * 100 >= regular * 112,
        "bold {bold} vs regular {regular}"
    );
}
