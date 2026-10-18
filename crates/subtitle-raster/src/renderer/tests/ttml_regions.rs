//! IMSC/TTML: регион документа реально определяет, где на кадре рисуется реплика.
//!
//! Сквозной путь: текст TTML -> `load_text_subtitle` -> `SubtitleTimeline` -> растеризатор ->
//! рамка закрашенных пикселей. Проверяем координаты на картинке, а не поля `CuePlacement`.

use subtitle_formats::{
    LegacyEncodingHint, SubtitleParseLimits, TextSubtitleFormat, load_text_subtitle,
};

use super::*;

const IMSC: &str = r#"<?xml version="1.0" encoding="UTF-8"?>
<tt xmlns="http://www.w3.org/ns/ttml" xmlns:tts="http://www.w3.org/ns/ttml#styling" xml:lang="en">
<head><layout>
  <region xml:id="top" tts:origin="10% 5%" tts:extent="80% 20%" tts:displayAlign="before" tts:textAlign="center"/>
  <region xml:id="bottom" tts:origin="10% 75%" tts:extent="80% 20%" tts:displayAlign="after" tts:textAlign="center"/>
  <region xml:id="left" tts:origin="0% 40%" tts:extent="30% 20%" tts:displayAlign="before" tts:textAlign="start"/>
</layout></head>
<body><div>
  <p begin="0s" end="1s" region="top">Top region</p>
  <p begin="2s" end="3s" region="bottom">Bottom region</p>
  <p begin="4s" end="5s" region="left">Left</p>
</div></body></tt>"#;

/// Реплика документа IMSC, активная в момент `at_millis`, как реплика теста.
fn imsc_cue(at_millis: i64) -> TestCue {
    let loaded = load_text_subtitle(
        IMSC.as_bytes(),
        TextSubtitleFormat::Ttml,
        LegacyEncodingHint::Generic,
        &SubtitleParseLimits::new(1 << 20, 1000, 100),
    )
    .expect("IMSC loads");
    let (timeline, _) = SubtitleTimeline::from_events(loaded.document().events().iter().cloned());
    let mut ids = Vec::new();
    timeline.collect_active_at(SubtitleTime::from_millis(at_millis), &mut ids);
    assert_eq!(ids.len(), 1, "one cue expected at {at_millis} ms");
    let SubtitleContent::Text(cue) = timeline.event(ids[0]).expect("event").content() else {
        panic!("text cue expected");
    };
    TestCue {
        key: CueKey::new(CueSourceId::new(1), ids[0]),
        cue: cue.clone(),
    }
}

fn ink_of_cue_at(at_millis: i64) -> (i32, i32, i32, i32) {
    let mut rasterizer = rasterizer(ROOMY_LIMITS);
    let overlay = render_once(
        &mut rasterizer,
        &[imsc_cue(at_millis)],
        &SubtitleAppearance::youtube_like(),
    );
    ink_bounds(overlay.bitmaps()).expect("cue has ink")
}

#[test]
fn top_region_cue_is_drawn_in_the_upper_part_and_bottom_region_cue_in_the_lower_part() {
    let height = HEIGHT as i32;
    let (_, top_y0, _, top_y1) = ink_of_cue_at(500);
    let (_, bottom_y0, _, bottom_y1) = ink_of_cue_at(2500);

    // Верхний регион занимает 5%..25% высоты (36..180 px): картинка целиком в нём.
    assert!(top_y0 >= height * 5 / 100 - 2, "top cue starts at {top_y0}");
    assert!(top_y1 <= height * 25 / 100 + 2, "top cue ends at {top_y1}");
    // Нижний регион занимает 75%..95% (540..684 px).
    assert!(
        bottom_y0 >= height * 75 / 100 - 2,
        "bottom cue starts at {bottom_y0}"
    );
    assert!(
        bottom_y1 <= height * 95 / 100 + 2,
        "bottom cue ends at {bottom_y1}"
    );
    // И они действительно по разные стороны середины кадра.
    assert!(top_y1 < height / 2 && bottom_y0 > height / 2);
}

#[test]
fn start_aligned_region_keeps_the_cue_inside_the_left_column() {
    let (x0, _, x1, _) = ink_of_cue_at(4500);
    let width = WIDTH as i32;
    // Регион 0%..30% по ширине (до 384 px); текст «Left» короткий и прижат к его левому краю.
    assert!(x0 < width / 10, "text must hug the left edge, got {x0}");
    assert!(
        x1 <= width * 30 / 100 + 2,
        "text must stay in the region, got {x1}"
    );
}

#[test]
fn centered_region_cue_is_horizontally_centered_in_the_region() {
    let (x0, _, x1, _) = ink_of_cue_at(500);
    let center = f64::from(x0 + x1) / 2.0;
    assert!((center - 640.0).abs() <= 3.0, "cue center {center}");
}
