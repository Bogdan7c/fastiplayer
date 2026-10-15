use subtitle_core::{
    CueLine, CueLineAlign, CuePlacement, CuePosition, CuePositionAlign, PercentOfVideo,
    ScreenAnchor, SubtitleColor,
};

use super::*;
use crate::overlay::CanvasSize;

fn canvas() -> CanvasSize {
    CanvasSize::new(1280, 720).expect("canvas")
}

fn percent(value: f32) -> PercentOfVideo {
    PercentOfVideo::new(value).expect("percent")
}

#[test]
fn fractions_reject_out_of_range_and_non_finite_values() {
    assert!(FontSizeFraction::new(0.05).is_ok());
    assert!(FontSizeFraction::new(0.0).is_err());
    assert!(FontSizeFraction::new(0.3).is_err());
    assert!(FontSizeFraction::new(f32::NAN).is_err());
    assert!(CanvasFraction::new(0.0).is_ok());
    assert!(CanvasFraction::new(0.51).is_err());
    assert!(LineWidthFraction::new(1.0).is_ok());
    assert!(LineWidthFraction::new(0.05).is_err());
    assert!(EmSize::new(f32::INFINITY).is_err());
    let error = EmSize::new(3.0).expect_err("above range");
    assert_eq!((error.value, error.min, error.max), (3.0, 0.0, 2.0));
}

#[test]
fn negative_zero_equals_positive_zero_including_hash() {
    use std::hash::{BuildHasher, RandomState};
    let hasher = RandomState::new();
    let negative = EmSize::new(-0.0).expect("zero");
    let positive = EmSize::new(0.0).expect("zero");
    assert_eq!(negative, positive);
    assert_eq!(hasher.hash_one(negative), hasher.hash_one(positive));
}

#[test]
fn presets_stay_inside_validated_ranges() {
    // `preset` ограничивает значение диапазоном; совпадение с исходной константой доказывает,
    // что константы лежат внутри диапазонов.
    let youtube = SubtitleAppearance::youtube_like();
    assert_eq!(
        youtube.font_size.get(),
        SubtitleAppearance::PRESET_FONT_SIZE
    );
    assert_eq!(
        youtube.bottom_margin.get(),
        SubtitleAppearance::PRESET_BOTTOM_MARGIN
    );
    assert_eq!(
        youtube.side_margin.get(),
        SubtitleAppearance::PRESET_SIDE_MARGIN
    );
    assert_eq!(
        youtube.max_line_width.get(),
        SubtitleAppearance::PRESET_MAX_LINE_WIDTH
    );
    let TextDecoration::BackgroundBox(plate) = youtube.decoration else {
        panic!("youtube_like must use a background box");
    };
    assert_eq!(
        plate.color,
        SubtitleColor {
            red: 8,
            green: 8,
            blue: 8,
            alpha: 191
        }
    );
    assert_eq!(plate.horizontal_padding_em.get(), 0.25);
    assert_eq!(plate.vertical_padding_em.get(), 0.06);
    assert_eq!(plate.corner_radius_em.get(), 0.15);

    let outline = SubtitleAppearance::outline_and_shadow();
    let TextDecoration::OutlineAndShadow(decoration) = outline.decoration else {
        panic!("outline_and_shadow must use outline and shadow");
    };
    assert_eq!(decoration.outline_width_em.get(), 0.06);
    assert_eq!(decoration.shadow_offset_em.get(), 0.04);
    assert_ne!(youtube, outline);
}

#[test]
fn default_placement_is_bottom_centered_with_plate_padding_added_to_margins() {
    let style = cue_style(
        &SubtitleAppearance::youtube_like(),
        &CuePlacement::default(),
        canvas(),
    );
    // Ячейка 720 / 18 = 40 px.
    assert!((style.event.font_size - 40.0).abs() < 1e-3);
    assert_eq!(style.event.alignment, ScreenAnchor::BottomCenter);
    // Внешний край плашки стоит на 6 % высоты: 43,2 + вертикальный отступ 2,4 px.
    assert_eq!(style.event.margins.vertical, 46);
    // Блок 90 % ширины = 1152 px, слева 64 px; отступ плашки 10 px.
    assert_eq!(
        (style.event.margins.left, style.event.margins.right),
        (74, 74)
    );
    assert_eq!(style.collision, CollisionDirection::Up);
    let plate = style.plate.expect("plate");
    assert!((plate.horizontal_padding - 10.0).abs() < 1e-3);
    assert!((plate.vertical_padding - 2.4).abs() < 1e-3);
    assert!((plate.corner_radius - 6.0).abs() < 1e-3);
    assert!(!style.event.border.is_nonzero() && !style.event.shadow.is_nonzero());
}

#[test]
fn outline_preset_has_border_and_shadow_but_no_plate() {
    let style = cue_style(
        &SubtitleAppearance::outline_and_shadow(),
        &CuePlacement::default(),
        canvas(),
    );
    assert!(style.plate.is_none());
    assert!((style.event.border.horizontal - 2.4).abs() < 1e-3);
    assert!((style.event.shadow.vertical - 1.6).abs() < 1e-3);
    assert_eq!(style.event.colors.outline, SubtitleColor::opaque(0, 0, 0));
    assert_eq!(style.event.margins.vertical, 43);
    assert_eq!(
        (style.event.margins.left, style.event.margins.right),
        (64, 64)
    );
}

#[test]
fn numpad_anchor_selects_alignment_and_collision_direction() {
    let placement = CuePlacement {
        anchor: Some(ScreenAnchor::TopRight),
        ..CuePlacement::default()
    };
    let style = cue_style(&SubtitleAppearance::youtube_like(), &placement, canvas());
    assert_eq!(style.event.alignment, ScreenAnchor::TopRight);
    assert_eq!(style.collision, CollisionDirection::Down);
    // Блок всегда шириной `max_line_width` (по центру), текст в нём прижат вправо.
    assert_eq!(
        (style.event.margins.left, style.event.margins.right),
        (74, 74)
    );

    let middle = CuePlacement {
        anchor: Some(ScreenAnchor::MiddleCenter),
        ..CuePlacement::default()
    };
    let style = cue_style(&SubtitleAppearance::youtube_like(), &middle, canvas());
    assert_eq!(style.event.alignment, ScreenAnchor::MiddleCenter);
    assert_eq!(style.event.margins.vertical, 0);
}

#[test]
fn webvtt_size_position_and_line_map_to_block_geometry() {
    let placement = CuePlacement {
        size: percent(50.0),
        position: CuePosition::Percent(percent(25.0)),
        position_align: CuePositionAlign::LineLeft,
        line: CueLine::Percent(percent(10.0)),
        line_align: CueLineAlign::Start,
        ..CuePlacement::default()
    };
    let style = cue_style(&SubtitleAppearance::youtube_like(), &placement, canvas());
    // Блок 50 % от 1152 = 576 px начинается на 25 % ширины (320 px).
    assert_eq!(style.event.margins.left, 330);
    assert_eq!(style.event.margins.right, 394);
    // Верх блока на 10 % высоты (72 px) плюс отступ плашки 2,4 px.
    // Выравнивание текста в блоке остаётся центральным (`text_align` по умолчанию).
    assert_eq!(style.event.alignment, ScreenAnchor::TopCenter);
    assert_eq!(style.event.margins.vertical, 74);
}

#[test]
fn negative_line_number_counts_cells_up_from_the_bottom() {
    let placement = CuePlacement {
        line: CueLine::LineNumber(-2),
        ..CuePlacement::default()
    };
    let style = cue_style(&SubtitleAppearance::youtube_like(), &placement, canvas());
    assert_eq!(style.event.alignment, ScreenAnchor::BottomCenter);
    // Нижний зазор 45,6 + одна ячейка 40.
    assert_eq!(style.event.margins.vertical, 86);
}

#[test]
fn block_is_kept_inside_the_canvas_for_extreme_positions() {
    let placement = CuePlacement {
        size: percent(100.0),
        position: CuePosition::Percent(percent(100.0)),
        position_align: CuePositionAlign::LineLeft,
        ..CuePlacement::default()
    };
    let style = cue_style(&SubtitleAppearance::youtube_like(), &placement, canvas());
    assert!(style.event.margins.left >= 0 && style.event.margins.right >= 0);
    assert!(
        i64::from(style.event.margins.left) + i64::from(style.event.margins.right) < 1280,
        "block must keep positive width: {:?}",
        style.event.margins
    );
}
