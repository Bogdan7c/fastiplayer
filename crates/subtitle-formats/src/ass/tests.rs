//! Тесты разбора текста реплики и тегов: каждая «ловушка» порядка тегов libass проверяется
//! по реальному результату разбора, а не по вызову функций.

use subtitle_core::{
    AssAlignment, AssAlphaTarget, AssAxes, AssBold, AssColorSlot, AssFontSize, AssHorizontalAlign,
    AssOverride, AssRgb, AssTextItem, AssUnsupportedTag, AssVerticalAlign, AssWrapStyle,
};

use super::limits::AssParseLimits;
use super::report::AssTagStats;
use super::text::parse_event_text;

fn limits(max_tags: usize) -> AssParseLimits {
    AssParseLimits {
        max_header_bytes: 1 << 16,
        max_styles: 16,
        max_override_tags_per_event: max_tags,
        max_tracked_unknown_tag_names: 4,
    }
}

/// Разбирает текст с щедрым лимитом тегов.
fn parse(text: &str) -> (Vec<AssTextItem>, AssTagStats) {
    parse_with_limit(text, 1000)
}

fn parse_with_limit(text: &str, max_tags: usize) -> (Vec<AssTextItem>, AssTagStats) {
    let limits = limits(max_tags);
    let mut stats = AssTagStats::new(&limits);
    let items = parse_event_text(text, max_tags, &mut stats);
    (items, stats)
}

/// Теги единственного блока в начале текста.
fn overrides_of(text: &str) -> Vec<AssOverride> {
    let (items, _) = parse(text);
    match items.into_iter().next() {
        Some(AssTextItem::Overrides(overrides)) => overrides,
        other => panic!("expected an override block first, got {other:?}"),
    }
}

fn single_override(text: &str) -> AssOverride {
    let mut overrides = overrides_of(text);
    assert_eq!(overrides.len(), 1, "expected exactly one tag in {text:?}");
    overrides.remove(0)
}

#[test]
fn fscx_is_scale_not_font_size() {
    assert_eq!(
        single_override(r"{\fscx120}x"),
        AssOverride::ScaleX(Some(1.2))
    );
    assert_eq!(
        single_override(r"{\fscy50}x"),
        AssOverride::ScaleY(Some(0.5))
    );
    // Отрицательный масштаб зажимается нулём.
    assert_eq!(
        single_override(r"{\fscx-10}x"),
        AssOverride::ScaleX(Some(0.0))
    );
    assert_eq!(single_override(r"{\fscx}x"), AssOverride::ScaleX(None));
}

#[test]
fn fsc_resets_both_scales_and_ignores_argument() {
    assert_eq!(single_override(r"{\fsc}x"), AssOverride::ResetScale);
    assert_eq!(single_override(r"{\fsc150}x"), AssOverride::ResetScale);
}

#[test]
fn fsp_is_spacing_and_fs_is_size() {
    assert_eq!(
        single_override(r"{\fsp2.5}x"),
        AssOverride::Spacing(Some(2.5))
    );
    assert_eq!(single_override(r"{\fsp}x"), AssOverride::Spacing(None));
    assert_eq!(
        single_override(r"{\fs30}x"),
        AssOverride::FontSize(AssFontSize::Absolute(30.0))
    );
    assert_eq!(
        single_override(r"{\fs+2}x"),
        AssOverride::FontSize(AssFontSize::Relative(2.0))
    );
    assert_eq!(
        single_override(r"{\fs-3}x"),
        AssOverride::FontSize(AssFontSize::Relative(-3.0))
    );
    assert_eq!(
        single_override(r"{\fs}x"),
        AssOverride::FontSize(AssFontSize::Reset)
    );
}

#[test]
fn bord_is_border_not_bold() {
    assert_eq!(
        single_override(r"{\bord2}x"),
        AssOverride::Border {
            axes: AssAxes::Both,
            value: Some(2.0)
        }
    );
    assert_eq!(
        single_override(r"{\xbord3}x"),
        AssOverride::Border {
            axes: AssAxes::X,
            value: Some(3.0)
        }
    );
    assert_eq!(
        single_override(r"{\ybord-1}x"),
        AssOverride::Border {
            axes: AssAxes::Y,
            // Отрицательный контур зажимается нулём.
            value: Some(0.0)
        }
    );
    assert_eq!(
        single_override(r"{\bord}x"),
        AssOverride::Border {
            axes: AssAxes::Both,
            value: None
        }
    );
}

#[test]
fn shad_is_shadow_not_strikeout_and_clamping_differs_by_axis() {
    assert_eq!(
        single_override(r"{\shad2}x"),
        AssOverride::Shadow {
            axes: AssAxes::Both,
            value: Some(2.0)
        }
    );
    assert_eq!(
        single_override(r"{\shad-2}x"),
        AssOverride::Shadow {
            axes: AssAxes::Both,
            value: Some(0.0)
        }
    );
    // \xshad и \yshad не зажимаются (тень влево/вверх).
    assert_eq!(
        single_override(r"{\xshad-2}x"),
        AssOverride::Shadow {
            axes: AssAxes::X,
            value: Some(-2.0)
        }
    );
    assert_eq!(
        single_override(r"{\yshad-4.5}x"),
        AssOverride::Shadow {
            axes: AssAxes::Y,
            value: Some(-4.5)
        }
    );
    assert_eq!(
        single_override(r"{\s1}x"),
        AssOverride::StrikeOut(Some(true))
    );
}

#[test]
fn an_is_numpad_and_a_is_legacy() {
    let top_center = AssAlignment {
        horizontal: AssHorizontalAlign::Center,
        vertical: AssVerticalAlign::Top,
    };
    assert_eq!(
        single_override(r"{\an8}x"),
        AssOverride::Alignment(Some(top_center))
    );
    // \a6 в старой схеме тоже «верх по центру».
    assert_eq!(
        single_override(r"{\a6}x"),
        AssOverride::Alignment(Some(top_center))
    );
    // \a2 — низ по центру; \an2 — тоже.
    assert_eq!(
        single_override(r"{\a2}x"),
        AssOverride::Alignment(Some(AssAlignment::BOTTOM_CENTER))
    );
    // Недопустимые значения -> None (стиль), но тег остаётся (первый тег выравнивания
    // побеждает даже недопустимый — решает растеризатор).
    assert_eq!(single_override(r"{\an10}x"), AssOverride::Alignment(None));
    assert_eq!(single_override(r"{\a12}x"), AssOverride::Alignment(None));
    assert_eq!(single_override(r"{\an}x"), AssOverride::Alignment(None));
}

#[test]
fn alpha_tag_is_not_confused_with_a() {
    assert_eq!(
        single_override(r"{\alpha&H80&}x"),
        AssOverride::Alpha {
            target: AssAlphaTarget::All,
            value: Some(0x80)
        }
    );
    assert_eq!(
        single_override(r"{\alpha}x"),
        AssOverride::Alpha {
            target: AssAlphaTarget::All,
            value: None
        }
    );
    assert_eq!(
        single_override(r"{\3a&HFF&}x"),
        AssOverride::Alpha {
            target: AssAlphaTarget::Slot(AssColorSlot::Outline),
            value: Some(0xff)
        }
    );
}

#[test]
fn colors_are_bgr_in_text_and_rgb_in_model() {
    assert_eq!(
        single_override(r"{\1c&H0000FF&}x"),
        AssOverride::Color {
            slot: AssColorSlot::Primary,
            value: Some(AssRgb::new(255, 0, 0))
        }
    );
    assert_eq!(
        single_override(r"{\c&HFF8000&}x"),
        AssOverride::Color {
            slot: AssColorSlot::Primary,
            value: Some(AssRgb::new(0x00, 0x80, 0xff))
        }
    );
    assert_eq!(
        single_override(r"{\4c&H112233&}x"),
        AssOverride::Color {
            slot: AssColorSlot::Back,
            value: Some(AssRgb::new(0x33, 0x22, 0x11))
        }
    );
    assert_eq!(
        single_override(r"{\2c}x"),
        AssOverride::Color {
            slot: AssColorSlot::Secondary,
            value: None
        }
    );
    // Мусор вместо числа читается как 0 (чёрный), а не отбрасывается.
    assert_eq!(
        single_override(r"{\c&Hzz&}x"),
        AssOverride::Color {
            slot: AssColorSlot::Primary,
            value: Some(AssRgb::new(0, 0, 0))
        }
    );
}

#[test]
fn font_name_reset_rules() {
    assert_eq!(
        single_override(r"{\fnComic Sans MS}x"),
        AssOverride::FontName(Some("Comic Sans MS".to_owned()))
    );
    assert_eq!(single_override(r"{\fn}x"), AssOverride::FontName(None));
    assert_eq!(single_override(r"{\fn0}x"), AssOverride::FontName(None));
    // Только ровно «0» сбрасывает; «0pt» — имя шрифта.
    assert_eq!(
        single_override(r"{\fn0pt}x"),
        AssOverride::FontName(Some("0pt".to_owned()))
    );
}

#[test]
fn bold_italic_underline_accept_only_valid_values() {
    assert_eq!(
        single_override(r"{\b1}x"),
        AssOverride::Bold(Some(AssBold::Bold))
    );
    assert_eq!(
        single_override(r"{\b0}x"),
        AssOverride::Bold(Some(AssBold::Normal))
    );
    assert_eq!(
        single_override(r"{\b700}x"),
        AssOverride::Bold(Some(AssBold::Weight(700)))
    );
    // 2..99 недопустимо -> шрифт стиля.
    assert_eq!(single_override(r"{\b50}x"), AssOverride::Bold(None));
    assert_eq!(single_override(r"{\b}x"), AssOverride::Bold(None));
    assert_eq!(single_override(r"{\i1}x"), AssOverride::Italic(Some(true)));
    assert_eq!(single_override(r"{\i2}x"), AssOverride::Italic(None));
    assert_eq!(
        single_override(r"{\u0}x"),
        AssOverride::Underline(Some(false))
    );
    assert_eq!(single_override(r"{\u}x"), AssOverride::Underline(None));
}

#[test]
fn position_needs_exactly_two_arguments() {
    assert_eq!(
        single_override(r"{\pos(320, 240.5)}x"),
        AssOverride::Position { x: 320.0, y: 240.5 }
    );
    let (items, stats) = parse(r"{\pos(1)}x");
    assert_eq!(items, vec![AssTextItem::Text("x".to_owned())]);
    assert_eq!(stats.malformed(), 1);
}

#[test]
fn reset_with_and_without_style_name() {
    assert_eq!(single_override(r"{\r}x"), AssOverride::Reset(None));
    assert_eq!(
        single_override(r"{\rSign}x"),
        AssOverride::Reset(Some("Sign".to_owned()))
    );
}

#[test]
fn wrap_style_and_encoding_tags() {
    assert_eq!(
        single_override(r"{\q2}x"),
        AssOverride::WrapStyle(Some(AssWrapStyle::NoWrap))
    );
    assert_eq!(single_override(r"{\q7}x"), AssOverride::WrapStyle(None));
    assert_eq!(
        single_override(r"{\fe204}x"),
        AssOverride::FontEncoding(Some(204))
    );
    assert_eq!(single_override(r"{\fe}x"), AssOverride::FontEncoding(None));
}

#[test]
fn tag_order_inside_one_block_is_preserved() {
    let tags = overrides_of(r"{\an8\bord2\fscx120\i1}x");
    assert_eq!(tags.len(), 4);
    assert!(matches!(tags[0], AssOverride::Alignment(Some(_))));
    assert!(matches!(tags[1], AssOverride::Border { .. }));
    assert_eq!(tags[2], AssOverride::ScaleX(Some(1.2)));
    assert_eq!(tags[3], AssOverride::Italic(Some(true)));
}

#[test]
fn spaces_after_backslash_and_around_arguments_are_tolerated() {
    assert_eq!(
        single_override(r"{\ fs 30 }x"),
        AssOverride::FontSize(AssFontSize::Absolute(30.0))
    );
    assert_eq!(
        single_override(r"{\pos( 1 , 2 )}x"),
        AssOverride::Position { x: 1.0, y: 2.0 }
    );
}

#[test]
fn layer_two_and_three_tags_are_unsupported_not_unknown() {
    let (items, stats) =
        parse(r"{\frz30\move(1,2,3,4)\t(0,100,\fs50)\clip(0,0,5,5)\blur2\be1\k20\p1}x");
    assert_eq!(items.len(), 2);
    for tag in [
        AssUnsupportedTag::Frz,
        AssUnsupportedTag::Move,
        AssUnsupportedTag::Transform,
        AssUnsupportedTag::Clip,
        AssUnsupportedTag::Blur,
        AssUnsupportedTag::Be,
        AssUnsupportedTag::Karaoke,
        AssUnsupportedTag::Drawing,
    ] {
        assert_eq!(stats.unsupported_count(tag), 1, "{tag:?}");
    }
    assert_eq!(stats.unknown_total(), 0);
    // Вложенный \fs внутри \t(...) не стал самостоятельным тегом.
    let flat = overrides_of(r"{\t(0,100,\fs50)}x");
    assert_eq!(
        flat,
        vec![AssOverride::Unsupported(AssUnsupportedTag::Transform)]
    );
}

#[test]
fn prefix_traps_of_unsupported_tags_do_not_leak_into_supported_ones() {
    // \be не должен стать \b, \blur — \bord/\b, \kf/\ko — \k, \fax — не \fa..., \pbo — не \p.
    let (_, stats) =
        parse(r"{\be1\blur3\kf10\ko5\pbo2\iclip(1,2,3,4)\org(1,2)\fad(1,2)\fade(1,2,3,4,5,6,7)}x");
    assert_eq!(stats.unsupported_count(AssUnsupportedTag::Be), 1);
    assert_eq!(stats.unsupported_count(AssUnsupportedTag::Blur), 1);
    assert_eq!(stats.unsupported_count(AssUnsupportedTag::Karaoke), 2);
    assert_eq!(
        stats.unsupported_count(AssUnsupportedTag::DrawingBaselineOffset),
        1
    );
    assert_eq!(stats.unsupported_count(AssUnsupportedTag::Drawing), 0);
    assert_eq!(stats.unsupported_count(AssUnsupportedTag::InverseClip), 1);
    assert_eq!(stats.unsupported_count(AssUnsupportedTag::Org), 1);
    assert_eq!(stats.unsupported_count(AssUnsupportedTag::Fade), 2);
    assert_eq!(stats.unknown_total(), 0);
}

#[test]
fn unknown_tags_are_counted_and_do_not_hide_text_or_neighbours() {
    let (items, stats) = parse(r"{\zzz5\fs20\jjj}Hello");
    assert_eq!(stats.unknown_total(), 2);
    assert_eq!(stats.unknown_names().get("zzz5"), Some(&1));
    assert_eq!(
        items,
        vec![
            AssTextItem::Overrides(vec![AssOverride::FontSize(AssFontSize::Absolute(20.0))]),
            AssTextItem::Text("Hello".to_owned())
        ]
    );
}

#[test]
fn tag_names_are_case_sensitive() {
    let (_, stats) = parse(r"{\FS20\Bord2}x");
    assert_eq!(stats.unknown_total(), 2);
}

#[test]
fn tags_beyond_per_event_limit_are_dropped_and_counted() {
    let (items, stats) = parse_with_limit(r"{\i1\b1\u1}x{\s1\an8}y", 2);
    assert_eq!(
        items,
        vec![
            AssTextItem::Overrides(vec![
                AssOverride::Italic(Some(true)),
                AssOverride::Bold(Some(AssBold::Bold)),
            ]),
            AssTextItem::Text("xy".to_owned()),
        ]
    );
    assert_eq!(stats.over_limit(), 3);
}

#[test]
fn text_escapes_follow_get_next_char() {
    let (items, _) = parse("a\\Nb\\nc\\hd\te\\{f\\}g\\xh");
    assert_eq!(
        items,
        vec![
            AssTextItem::Text("a".to_owned()),
            AssTextItem::HardBreak,
            AssTextItem::Text("b".to_owned()),
            AssTextItem::SoftBreak,
            // \h -> NBSP, табуляция -> пробел, \{ \} -> литералы, неизвестный \x остаётся.
            AssTextItem::Text("c\u{a0}d e{f}g\\xh".to_owned()),
        ]
    );
}

#[test]
fn unmatched_open_brace_is_literal_text_and_lone_close_brace_too() {
    let (items, stats) = parse(r"a{\b1 b } c");
    // `{` с парой `}` — блок; проверим отдельно без пары.
    assert_eq!(stats.unknown_total(), 0);
    assert_eq!(
        items,
        vec![
            AssTextItem::Text("a".to_owned()),
            AssTextItem::Overrides(vec![AssOverride::Bold(Some(AssBold::Bold))]),
            AssTextItem::Text(" c".to_owned()),
        ]
    );
    let (literal, _) = parse(r"x{\b1 y");
    assert_eq!(literal, vec![AssTextItem::Text(r"x{\b1 y".to_owned())]);
    let (lone_close, _) = parse("x}y");
    assert_eq!(lone_close, vec![AssTextItem::Text("x}y".to_owned())]);
}

#[test]
fn comment_blocks_without_tags_produce_no_override_item() {
    let (items, stats) = parse("a{just a comment}b");
    assert_eq!(items, vec![AssTextItem::Text("ab".to_owned())]);
    assert_eq!(stats.unknown_total(), 0);
}

#[test]
fn text_is_cut_at_nul_like_a_c_string() {
    let (items, _) = parse("abc\0def");
    assert_eq!(items, vec![AssTextItem::Text("abc".to_owned())]);
}

#[test]
fn many_arguments_are_capped_without_panicking() {
    let (items, _) = parse(r"{\pos(1,2,3,4,5,6,7,8,9,10,11,12)}x");
    // 12 аргументов -> не 2 -> тег игнорируется как неверный; текст на месте.
    assert_eq!(items, vec![AssTextItem::Text("x".to_owned())]);
}

#[test]
fn drawing_tag_keeps_its_argument_and_only_enabled_mode_is_counted() {
    use subtitle_core::AssDrawingMode;
    assert_eq!(
        overrides_of(r"{\p1}x"),
        vec![AssOverride::Drawing(AssDrawingMode::On { scale: 1 })]
    );
    assert_eq!(
        overrides_of(r"{\p4}x"),
        vec![AssOverride::Drawing(AssDrawingMode::On { scale: 4 })]
    );
    // `\p0`, `\p` без аргумента и отрицательное значение выключают режим (libass: max(0, n)).
    for off in [r"{\p0}x", r"{\p}x", r"{\p-3}x"] {
        assert_eq!(
            overrides_of(off),
            vec![AssOverride::Drawing(AssDrawingMode::Off)],
            "{off}"
        );
    }
    let (_, enabled) = parse(r"{\p1}m 0 0 l 5 5{\p0}");
    assert_eq!(enabled.unsupported_count(AssUnsupportedTag::Drawing), 1);
    let (_, disabled) = parse(r"{\p0}x");
    assert_eq!(disabled.unsupported_count(AssUnsupportedTag::Drawing), 0);
    // `\pbo` по-прежнему отдельный (нерисуемый) тег, а не режим рисунка.
    assert_eq!(
        overrides_of(r"{\pbo5}x"),
        vec![AssOverride::Unsupported(
            AssUnsupportedTag::DrawingBaselineOffset
        )]
    );
}

#[test]
fn many_unmatched_open_braces_parse_in_linear_time_as_literal_text() {
    let text = "{".repeat(1_000_000);
    let started = std::time::Instant::now();
    let (items, _) = parse(&text);
    assert!(
        started.elapsed() < std::time::Duration::from_secs(2),
        "разбор `{{` без `}}` должен быть линейным"
    );
    assert_eq!(items, vec![AssTextItem::Text(text)]);
}

#[test]
fn open_brace_after_last_close_is_literal_but_earlier_block_still_parses() {
    let (items, _) = parse("{\\b1}a{{");
    assert_eq!(items.len(), 2);
    assert!(matches!(items[0], AssTextItem::Overrides(_)));
    assert_eq!(items[1], AssTextItem::Text("a{{".to_owned()));
}

#[test]
fn move_keeps_its_arguments_orders_times_and_is_still_reported_as_unsupported() {
    let tags = overrides_of(r"{\move(1,2,3,4,500,100)}x");
    assert_eq!(
        tags,
        vec![AssOverride::Move(subtitle_core::AssMove {
            from_x: 1.0,
            from_y: 2.0,
            to_x: 3.0,
            to_y: 4.0,
            start_ms: 100,
            end_ms: 500,
        })]
    );
    let (_, stats) = parse(r"{\move(1,2,3,4)}x");
    assert_eq!(stats.unsupported_count(AssUnsupportedTag::Move), 1);
    // Без времени и с неположительным временем: «на всю реплику».
    let whole_event = overrides_of(r"{\move(1,2,3,4,-5,0)}x");
    assert!(matches!(
        whole_event.as_slice(),
        [AssOverride::Move(movement)] if movement.start_ms == 0 && movement.end_ms == 0
    ));
}

#[test]
fn move_with_a_wrong_argument_count_is_ignored_and_counted_as_malformed() {
    let (items, stats) = parse(r"{\move(1,2,3)}x");
    assert_eq!(items, vec![AssTextItem::Text("x".to_owned())]);
    assert_eq!(stats.unsupported_count(AssUnsupportedTag::Move), 0);
    assert_eq!(stats.malformed(), 1);
}
