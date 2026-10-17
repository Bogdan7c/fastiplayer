use subtitle_core::{
    AssAlignment, AssAlphaTarget, AssBold, AssColorSlot, AssCue, AssDrawingMode, AssEventMargins,
    AssFontSize, AssOverride, AssRgb, AssTextItem, AssUnsupportedTag, AssWrapStyle,
};

use super::*;
use crate::ass::options::{AssStorageSize, AssTextScale};
use crate::ass::test_script::{cue_with, default_style, script_with, tags, text};
use crate::fonts::FontWeightClass;

fn canvas(width: u32, height: u32) -> CanvasSize {
    CanvasSize::new(width, height).expect("canvas")
}

fn plan_with(cue: &AssCue, canvas: CanvasSize, options: &AssRenderOptions) -> AssEventPlan {
    plan_ass_event(
        cue,
        canvas,
        options,
        &SubtitleAppearance::outline_and_shadow(),
    )
}

fn plan(items: Vec<AssTextItem>) -> AssEventPlan {
    let script = script_with(vec![default_style()]);
    plan_with(
        &cue_with(&script, items),
        canvas(1280, 720),
        &AssRenderOptions::default(),
    )
}

fn flat(plan: &AssEventPlan) -> String {
    plan.source.chars.iter().map(|source| source.ch).collect()
}

#[test]
fn tags_split_the_text_into_looks_and_equal_looks_share_an_index() {
    let result = plan(vec![
        text("a"),
        tags(vec![AssOverride::FontSize(AssFontSize::Absolute(80.0))]),
        text("b"),
        tags(vec![AssOverride::FontSize(AssFontSize::Absolute(40.0))]),
        text("c"),
    ]);
    assert_eq!(flat(&result), "abc");
    let looks: Vec<usize> = result.source.chars.iter().map(|c| c.look).collect();
    // Первая и третья буква — один вид (размер вернулся), вторая — другой.
    assert_eq!(looks[0], looks[2]);
    assert_ne!(looks[0], looks[1]);
    assert_eq!(result.source.looks.len(), 2);
    assert_eq!(result.source.looks[looks[1]].font_size, 80.0);
}

#[test]
fn soft_break_depends_on_the_wrap_style_at_that_point_of_the_text() {
    let nowrap_first = plan(vec![
        tags(vec![AssOverride::WrapStyle(Some(AssWrapStyle::NoWrap))]),
        text("a"),
        AssTextItem::SoftBreak,
        text("b"),
    ]);
    assert_eq!(flat(&nowrap_first), "a\nb");
    // `\q2` после `\n`: в момент `\n` режим ещё обычный, поэтому пробел.
    let nowrap_later = plan(vec![
        text("a"),
        AssTextItem::SoftBreak,
        tags(vec![AssOverride::WrapStyle(Some(AssWrapStyle::NoWrap))]),
        text("b"),
    ]);
    assert_eq!(flat(&nowrap_later), "a b");
    // Но режим переноса реплики — итоговый (последний `\q`).
    assert_eq!(nowrap_later.style.wrap_style, WrapStyle::NoWrap);
    // Жёсткий перевод строки не зависит от режима.
    let hard = plan(vec![text("a"), AssTextItem::HardBreak, text("b")]);
    assert_eq!(flat(&hard), "a\nb");
}

#[test]
fn drawing_mode_skips_text_and_counts_it() {
    let result = plan(vec![
        tags(vec![AssOverride::Drawing(AssDrawingMode::On { scale: 1 })]),
        text("m 0 0 l 10 10"),
        AssTextItem::HardBreak,
        tags(vec![AssOverride::Drawing(AssDrawingMode::Off)]),
        text("Hi"),
    ]);
    assert_eq!(flat(&result), "Hi");
    assert_eq!(result.skipped_drawing_runs, 1);
}

#[test]
fn first_alignment_wins_and_event_margins_override_style_margins() {
    let script = script_with(vec![default_style()]);
    let mut cue = cue_with(
        &script,
        vec![
            tags(vec![
                AssOverride::Alignment(Some(AssAlignment::from_numpad(9))),
                AssOverride::Alignment(Some(AssAlignment::from_numpad(1))),
            ]),
            text("x"),
        ],
    );
    cue.margins = AssEventMargins {
        left: 100,
        right: 0,
        vertical: 30,
    };
    let result = plan_with(&cue, canvas(1280, 720), &AssRenderOptions::default());
    assert_eq!(
        result.style.alignment,
        subtitle_core::ScreenAnchor::TopRight
    );
    let EventPlacement::Exact(exact) = result.style.placement else {
        panic!("ASS must use exact placement");
    };
    // PlayRes = холст: масштаб 1. Левое поле события 100, правое — из стиля (10), вертикальное 30.
    assert_eq!(exact.text_left, 100.0);
    assert_eq!(exact.text_width, 1280.0 - 10.0 - 100.0);
    assert_eq!(exact.vertical_margin, 30.0);
    assert_eq!(exact.anchor, BlockAnchor::ByAlignment);
    // Верхний якорь сдвигает столкновения вниз.
    assert_eq!(result.collision.direction, CollisionDirection::Down);
    assert!(result.collision.detect);
}

#[test]
fn margins_come_from_the_style_of_the_last_reset() {
    let mut sign = default_style();
    sign.name = "Sign".to_owned();
    sign.margin_left = 300;
    let script = script_with(vec![default_style(), sign]);
    let cue = cue_with(
        &script,
        vec![
            tags(vec![AssOverride::Reset(Some("Sign".to_owned()))]),
            text("x"),
        ],
    );
    let result = plan_with(&cue, canvas(1280, 720), &AssRenderOptions::default());
    let EventPlacement::Exact(exact) = result.style.placement else {
        panic!("exact placement");
    };
    assert_eq!(exact.text_left, 300.0);
}

#[test]
fn coordinates_scale_from_play_res_to_the_canvas_and_pos_disables_collisions() {
    let script = script_with(vec![default_style()]);
    let cue = cue_with(
        &script,
        vec![
            tags(vec![AssOverride::Position { x: 640.0, y: 360.0 }]),
            text("x"),
        ],
    );
    // Сценарий 1280x720 на холсте 640x360: всё вдвое меньше.
    let result = plan_with(&cue, canvas(640, 360), &AssRenderOptions::default());
    let EventPlacement::Exact(exact) = result.style.placement else {
        panic!("exact placement");
    };
    assert_eq!(exact.anchor, BlockAnchor::AtPoint { x: 320.0, y: 180.0 });
    assert_eq!(exact.text_left, 5.0);
    assert_eq!(exact.vertical_margin, 5.0);
    assert!(!result.collision.detect, "\\pos events do not collide");
    // Размер шрифта тоже вдвое меньше.
    assert_eq!(result.source.looks[0].font_size, 20.0);
    assert_eq!(result.source.looks[0].border.horizontal, 1.0);
}

#[test]
fn org_and_transform_tags_switch_collision_detection_off() {
    for tag in [AssUnsupportedTag::Org, AssUnsupportedTag::Transform] {
        let result = plan(vec![tags(vec![AssOverride::Unsupported(tag)]), text("x")]);
        assert!(!result.collision.detect, "{tag:?}");
    }
    assert!(plan(vec![text("x")]).collision.detect);
}

#[test]
fn author_text_scale_applies_to_normal_events_but_not_to_explicit_ones() {
    let script = script_with(vec![default_style()]);
    let options = AssRenderOptions {
        text_scale: AssTextScale::new(1.5).expect("scale"),
        ..AssRenderOptions::default()
    };
    let normal = plan_with(
        &cue_with(&script, vec![text("x")]),
        canvas(1280, 720),
        &options,
    );
    assert_eq!(normal.source.looks[0].font_size, 60.0);
    assert_eq!(normal.source.looks[0].border.vertical, 3.0);
    // «Явность» берётся из сырого текста при разборе: парсер пометил бы `\pos` как Present.
    let mut positioned_cue = cue_with(
        &script,
        vec![
            tags(vec![AssOverride::Position { x: 1.0, y: 1.0 }]),
            text("x"),
        ],
    );
    positioned_cue.hard_overrides = subtitle_core::AssHardOverrides::Present;
    let positioned = plan_with(&positioned_cue, canvas(1280, 720), &options);
    assert_eq!(positioned.source.looks[0].font_size, 40.0);
    assert_eq!(positioned.source.looks[0].border.vertical, 2.0);
    // Масштаб не двигает поля: положение остаётся авторским.
    let EventPlacement::Exact(scaled_exact) = normal.style.placement else {
        panic!("exact placement");
    };
    assert_eq!(scaled_exact.text_left, 10.0);
}

#[test]
fn user_style_overrides_visual_tags_but_keeps_attributes_and_position() {
    let script = script_with(vec![default_style()]);
    let cue = cue_with(
        &script,
        vec![
            tags(vec![
                AssOverride::Color {
                    slot: AssColorSlot::Primary,
                    value: Some(AssRgb::new(1, 2, 3)),
                },
                AssOverride::Alpha {
                    target: AssAlphaTarget::All,
                    value: Some(0x80),
                },
                AssOverride::FontSize(AssFontSize::Absolute(200.0)),
                AssOverride::FontName(Some("Arial".to_owned())),
                AssOverride::Border {
                    axes: subtitle_core::AssAxes::Both,
                    value: Some(30.0),
                },
                AssOverride::Bold(Some(AssBold::Bold)),
                AssOverride::Underline(Some(true)),
                AssOverride::Alignment(Some(AssAlignment::from_numpad(8))),
            ]),
            text("x"),
        ],
    );
    let appearance = SubtitleAppearance::outline_and_shadow();
    let options = AssRenderOptions {
        style_source: AssStyleSource::User,
        text_scale: AssTextScale::new(3.0).expect("scale"),
        ..AssRenderOptions::default()
    };
    let result = plan_ass_event(&cue, canvas(1280, 720), &options, &appearance);
    let look = &result.source.looks[0];
    assert_eq!(look.font_family, None);
    assert_eq!(look.colors.primary, appearance.text_color);
    // Размер из пользовательского стиля (1/18 высоты), масштаб текста в этом режиме не нужен.
    assert!((look.font_size - 40.0).abs() < 1e-3);
    assert!((look.border.horizontal - 2.4).abs() < 1e-3);
    // Атрибуты и выравнивание остаются авторскими.
    assert_eq!(look.weight, FontWeightClass::Bold);
    assert!(look.decorations.underline);
    assert_eq!(
        result.style.alignment,
        subtitle_core::ScreenAnchor::TopCenter
    );
    assert!(result.plate.is_none(), "outline preset has no plate");
    let boxed = plan_ass_event(
        &cue,
        canvas(1280, 720),
        &options,
        &SubtitleAppearance::youtube_like(),
    );
    assert!(boxed.plate.is_some());
    assert_eq!(boxed.source.looks[0].border.horizontal, 0.0);
}

#[test]
fn author_look_reads_tags_and_style_values() {
    let script = script_with(vec![default_style()]);
    let result = plan_with(
        &cue_with(
            &script,
            vec![
                tags(vec![
                    AssOverride::Color {
                        slot: AssColorSlot::Primary,
                        value: Some(AssRgb::new(10, 20, 30)),
                    },
                    AssOverride::Alpha {
                        target: AssAlphaTarget::Slot(AssColorSlot::Primary),
                        value: Some(0x40),
                    },
                    AssOverride::Bold(Some(AssBold::Weight(900))),
                    AssOverride::FontName(Some("@Noto Sans".to_owned())),
                ]),
                text("x"),
            ],
        ),
        canvas(1280, 720),
        &AssRenderOptions::default(),
    );
    let look = &result.source.looks[0];
    assert_eq!(look.colors.primary.red, 10);
    assert_eq!(look.colors.primary.alpha, 0xBF);
    assert_eq!(look.weight_value, 900);
    assert_eq!(look.weight, FontWeightClass::Bold);
    assert_eq!(look.font_family.as_deref(), Some("Noto Sans"));
    // Контур стиля 2 и обводка по осям в пикселях, тень 0.
    assert_eq!(look.border.horizontal, 2.0);
    assert_eq!(look.shadow.horizontal, 0.0);
}

#[test]
fn absurd_sizes_are_clamped_to_a_few_canvases() {
    let result = plan(vec![
        tags(vec![
            AssOverride::FontSize(AssFontSize::Absolute(1.0e9)),
            AssOverride::Border {
                axes: subtitle_core::AssAxes::Both,
                value: Some(1.0e9),
            },
            AssOverride::Shadow {
                axes: subtitle_core::AssAxes::X,
                value: Some(-1.0e9),
            },
        ]),
        text("x"),
    ]);
    let look = &result.source.looks[0];
    assert_eq!(look.font_size, 4.0 * 1280.0);
    assert_eq!(look.border.horizontal, 1280.0);
    assert_eq!(look.shadow.horizontal, -1280.0);
}

#[test]
fn banner_effect_forces_no_wrap_and_encoding_minus_one_means_auto_direction() {
    let mut style = default_style();
    style.encoding = -1;
    let script = script_with(vec![style]);
    let mut cue = cue_with(&script, vec![text("x")]);
    cue.effect = "Banner;50;0;80".to_owned();
    let result = plan_with(&cue, canvas(1280, 720), &AssRenderOptions::default());
    assert_eq!(result.style.wrap_style, WrapStyle::NoWrap);
    assert_eq!(result.style.base_direction, BaseDirection::Auto);
    assert!(!result.collision.detect);
    let plain = plan(vec![text("x")]);
    assert_eq!(plain.style.wrap_style, WrapStyle::Smart);
    assert_eq!(plain.style.base_direction, BaseDirection::LeftToRight);
}

#[test]
fn known_storage_size_changes_border_scale_and_pixel_aspect() {
    let script = script_with(vec![default_style()]);
    let cue = cue_with(&script, vec![text("x")]);
    let options = AssRenderOptions {
        storage_size: AssStorageSize::from_pixels(1024, 768),
        ..AssRenderOptions::default()
    };
    let result = plan_with(&cue, canvas(1280, 720), &options);
    let EventPlacement::Exact(exact) = result.style.placement else {
        panic!("exact placement");
    };
    // Холст 16:9 при хранении 4:3: пиксель растянут на (16/9) / (4/3) = 4/3.
    assert!((exact.pixel_aspect_x - 4.0 / 3.0).abs() < 1e-9);
}

#[test]
fn move_places_the_event_at_its_start_point_and_a_later_pos_is_ignored() {
    let movement = AssOverride::Move(subtitle_core::AssMove {
        from_x: 100.0,
        from_y: 50.0,
        to_x: 300.0,
        to_y: 250.0,
        start_ms: 0,
        end_ms: 0,
    });
    let result = plan(vec![
        tags(vec![movement, AssOverride::Position { x: 9.0, y: 9.0 }]),
        text("x"),
    ]);
    let EventPlacement::Exact(exact) = result.style.placement else {
        panic!("\\move must give exact placement");
    };
    assert_eq!(exact.anchor, BlockAnchor::AtPoint { x: 100.0, y: 50.0 });
    assert!(!result.collision.detect, "\\move events do not collide");
}
