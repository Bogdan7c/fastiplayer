use subtitle_core::{
    AssAlignment, AssAlphaTarget, AssAxes, AssBold, AssColor, AssColorSlot, AssDrawingMode,
    AssFontSize, AssOverride, AssRgb, AssWrapStyle,
};

use super::*;
use crate::ass::test_script::{default_style, script_with};

fn sign_style() -> AssStyle {
    AssStyle {
        name: "Sign".to_owned(),
        font_name: "Noto Sans Hebrew".to_owned(),
        font_size: 20.0,
        primary_color: AssColor::new(1, 2, 3, 0x10),
        outline: 5.0,
        shadow: 4.0,
        bold: AssBold::Bold,
        italic: true,
        margin_vertical: 99,
        ..default_style()
    }
}

#[test]
fn initial_state_mirrors_event_style() {
    let script = script_with(vec![default_style()]);
    let style = script.style(script.lookup_style("Default"));
    let state = RenderState::new(&script, style, AssWrapStyle::Smart);
    assert_eq!(state.font_name, "Noto Sans");
    assert_eq!(state.font_size, 40.0);
    assert_eq!(state.border, AxisValues::uniform(2.0));
    assert_eq!(state.colors[0], AssColor::new(255, 255, 255, 0));
    assert_eq!(state.alignment, AssAlignment::BOTTOM_CENTER);
    assert_eq!(state.position, None);
}

#[test]
fn colors_keep_their_own_transparency_and_alpha_keeps_rgb() {
    let script = script_with(vec![default_style()]);
    let style = script.style(script.lookup_style("Default"));
    let mut state = RenderState::new(&script, style, AssWrapStyle::Smart);
    state.apply(&AssOverride::Alpha {
        target: AssAlphaTarget::Slot(AssColorSlot::Primary),
        value: Some(0x40),
    });
    state.apply(&AssOverride::Color {
        slot: AssColorSlot::Primary,
        value: Some(AssRgb::new(9, 8, 7)),
    });
    // `\c` не трогает прозрачность, выставленную `\1a`.
    assert_eq!(state.colors[0], AssColor::new(9, 8, 7, 0x40));
    // `\alpha` действует на все четыре слота.
    state.apply(&AssOverride::Alpha {
        target: AssAlphaTarget::All,
        value: Some(0x22),
    });
    assert!(state.colors.iter().all(|color| color.transparency == 0x22));
    // `\1a` без аргумента возвращает прозрачность стиля (0), RGB остаётся.
    state.apply(&AssOverride::Alpha {
        target: AssAlphaTarget::Slot(AssColorSlot::Primary),
        value: None,
    });
    assert_eq!(state.colors[0], AssColor::new(9, 8, 7, 0));
    // `\c` без аргумента возвращает RGB стиля, прозрачность остаётся.
    state.apply(&AssOverride::Alpha {
        target: AssAlphaTarget::Slot(AssColorSlot::Primary),
        value: Some(0x70),
    });
    state.apply(&AssOverride::Color {
        slot: AssColorSlot::Primary,
        value: None,
    });
    assert_eq!(state.colors[0], AssColor::new(255, 255, 255, 0x70));
}

#[test]
fn font_size_absolute_relative_and_reset() {
    let script = script_with(vec![default_style()]);
    let style = script.style(script.lookup_style("Default"));
    let mut state = RenderState::new(&script, style, AssWrapStyle::Smart);
    state.apply(&AssOverride::FontSize(AssFontSize::Absolute(60.0)));
    assert_eq!(state.font_size, 60.0);
    // `\fs+5`: ×1.5 от ТЕКУЩЕГО размера.
    state.apply(&AssOverride::FontSize(AssFontSize::Relative(5.0)));
    assert_eq!(state.font_size, 90.0);
    // Итог <= 0 заменяется размером стиля.
    state.apply(&AssOverride::FontSize(AssFontSize::Relative(-10.0)));
    assert_eq!(state.font_size, 40.0);
    state.apply(&AssOverride::FontSize(AssFontSize::Absolute(-3.0)));
    assert_eq!(state.font_size, 40.0);
    state.apply(&AssOverride::FontSize(AssFontSize::Absolute(25.0)));
    state.apply(&AssOverride::FontSize(AssFontSize::Reset));
    assert_eq!(state.font_size, 40.0);
}

#[test]
fn border_and_shadow_axes_are_independent_and_fall_back_to_style() {
    let script = script_with(vec![default_style()]);
    let style = script.style(script.lookup_style("Default"));
    let mut state = RenderState::new(&script, style, AssWrapStyle::Smart);
    state.apply(&AssOverride::Border {
        axes: AssAxes::X,
        value: Some(7.0),
    });
    assert_eq!(state.border, AxisValues { x: 7.0, y: 2.0 });
    state.apply(&AssOverride::Shadow {
        axes: AssAxes::Y,
        value: Some(-3.0),
    });
    assert_eq!(state.shadow, AxisValues { x: 0.0, y: -3.0 });
    state.apply(&AssOverride::Border {
        axes: AssAxes::Both,
        value: None,
    });
    assert_eq!(state.border, AxisValues::uniform(2.0));
}

#[test]
fn first_alignment_and_first_position_win() {
    let script = script_with(vec![default_style()]);
    let style = script.style(script.lookup_style("Default"));
    let mut state = RenderState::new(&script, style, AssWrapStyle::Smart);
    state.apply(&AssOverride::Alignment(Some(AssAlignment::from_numpad(7))));
    state.apply(&AssOverride::Alignment(Some(AssAlignment::from_numpad(3))));
    assert_eq!(state.alignment, AssAlignment::from_numpad(7));
    state.apply(&AssOverride::Position { x: 1.0, y: 2.0 });
    state.apply(&AssOverride::Position { x: 9.0, y: 9.0 });
    assert_eq!(state.position, Some(ScriptPoint { x: 1.0, y: 2.0 }));

    // Недопустимый первый `\an` тоже «занимает» место и возвращает стиль.
    let mut state = RenderState::new(&script, style, AssWrapStyle::Smart);
    state.apply(&AssOverride::Alignment(None));
    state.apply(&AssOverride::Alignment(Some(AssAlignment::from_numpad(9))));
    assert_eq!(state.alignment, AssAlignment::BOTTOM_CENTER);
}

#[test]
fn wrap_style_last_wins_and_invalid_value_means_script_mode() {
    let script = script_with(vec![default_style()]);
    let style = script.style(script.lookup_style("Default"));
    let mut state = RenderState::new(&script, style, AssWrapStyle::Smart);
    state.apply(&AssOverride::WrapStyle(Some(AssWrapStyle::NoWrap)));
    state.apply(&AssOverride::WrapStyle(Some(AssWrapStyle::EndOfLine)));
    assert_eq!(state.wrap_style, AssWrapStyle::EndOfLine);
    state.apply(&AssOverride::WrapStyle(None));
    assert_eq!(state.wrap_style, AssWrapStyle::Smart);
}

#[test]
fn reset_restores_style_values_but_keeps_event_wide_state() {
    let script = script_with(vec![default_style(), sign_style()]);
    let style = script.style(script.lookup_style("Default"));
    let mut state = RenderState::new(&script, style, AssWrapStyle::Smart);
    state.apply(&AssOverride::Alignment(Some(AssAlignment::from_numpad(8))));
    state.apply(&AssOverride::Position { x: 5.0, y: 6.0 });
    state.apply(&AssOverride::WrapStyle(Some(AssWrapStyle::NoWrap)));
    state.apply(&AssOverride::Drawing(AssDrawingMode::On { scale: 1 }));
    state.apply(&AssOverride::FontSize(AssFontSize::Absolute(99.0)));
    state.apply(&AssOverride::Bold(Some(AssBold::Bold)));

    state.apply(&AssOverride::Reset(None));
    assert_eq!(state.font_size, 40.0);
    assert_eq!(state.bold, AssBold::Normal);
    // Всё это `\r` не трогает.
    assert_eq!(state.alignment, AssAlignment::from_numpad(8));
    assert_eq!(state.position, Some(ScriptPoint { x: 5.0, y: 6.0 }));
    assert_eq!(state.wrap_style, AssWrapStyle::NoWrap);
    assert!(state.drawing.is_on());

    // `\rSign` берёт значения именованного стиля, и теги без аргумента теперь возвращают ЕГО.
    state.apply(&AssOverride::Reset(Some("Sign".to_owned())));
    assert_eq!(state.font_name, "Noto Sans Hebrew");
    assert_eq!(state.font_size, 20.0);
    assert!(state.italic);
    assert_eq!(state.style().margin_vertical, 99);
    state.apply(&AssOverride::FontSize(AssFontSize::Absolute(70.0)));
    state.apply(&AssOverride::FontSize(AssFontSize::Reset));
    assert_eq!(state.font_size, 20.0);

    // Неизвестное имя — стиль реплики (не стиль последнего сброса).
    state.apply(&AssOverride::Reset(Some("Nope".to_owned())));
    assert_eq!(state.font_name, "Noto Sans");
    assert_eq!(state.style().margin_vertical, 10);
}

#[test]
fn attribute_tags_without_valid_value_use_the_reset_style() {
    let script = script_with(vec![default_style(), sign_style()]);
    let style = script.style(script.lookup_style("Default"));
    let mut state = RenderState::new(&script, style, AssWrapStyle::Smart);
    state.apply(&AssOverride::Reset(Some("Sign".to_owned())));
    state.apply(&AssOverride::Bold(Some(AssBold::Normal)));
    state.apply(&AssOverride::Italic(Some(false)));
    assert_eq!(state.bold, AssBold::Normal);
    assert!(!state.italic);
    state.apply(&AssOverride::Bold(None));
    state.apply(&AssOverride::Italic(None));
    assert_eq!(state.bold, AssBold::Bold);
    assert!(state.italic);
}

// --- `\t`: смешивание старого значения с новым (ass_parse.c, `pwr`) ---

fn timed_state<'script>(
    script: &'script AssScript,
    style: &'script AssStyle,
    elapsed_ms: i64,
) -> RenderState<'script> {
    use subtitle_core::SubtitleTime;
    let clock = crate::ass::AssEventClock::new(
        SubtitleTime::from_millis(elapsed_ms),
        crate::renderer::AssEventTiming::new(SubtitleTime::ZERO, SubtitleTime::from_millis(2000)),
    );
    RenderState::new(script, style, AssWrapStyle::Smart).with_event_clock(Some(clock))
}

fn quarter_way(nested: Vec<AssOverride>) -> AssOverride {
    // 0..1000 мс, момент 250: вес 0.25 (линейно).
    crate::ass::test_script::transform_range(0, 1000, nested)
}

#[test]
fn transform_blends_sizes_scales_spacing_and_angles_with_the_weight() {
    let script = script_with(vec![default_style()]);
    let style = script.style(script.lookup_style("Default"));
    let mut state = timed_state(&script, style, 250);
    state.apply(&quarter_way(vec![
        AssOverride::FontSize(AssFontSize::Absolute(80.0)),
        AssOverride::ScaleX(Some(2.0)),
        AssOverride::ScaleY(Some(0.0)),
        AssOverride::Spacing(Some(8.0)),
        AssOverride::Rotation {
            axis: subtitle_core::AssRotationAxis::Z,
            degrees: Some(40.0),
        },
        AssOverride::Rotation {
            axis: subtitle_core::AssRotationAxis::X,
            degrees: Some(-20.0),
        },
        AssOverride::Shear {
            axis: subtitle_core::AssShearAxis::X,
            factor: Some(1.0),
        },
    ]));
    assert_eq!(state.font_size, 50.0);
    assert_eq!(state.scale, AxisValues { x: 1.25, y: 0.75 });
    assert_eq!(state.spacing, 2.0);
    assert_eq!(state.rotation.z_degrees, 10.0);
    assert_eq!(state.rotation.x_degrees, -5.0);
    assert_eq!(state.shear.horizontal, 0.25);
}

#[test]
fn transform_relative_font_size_multiplies_the_current_size() {
    let script = script_with(vec![default_style()]);
    let style = script.style(script.lookup_style("Default"));
    let mut state = timed_state(&script, style, 250);
    // `\fs+10` при весе 0.25: 40 * (1 + 0.25 * 10 / 10) = 50.
    state.apply(&quarter_way(vec![AssOverride::FontSize(
        AssFontSize::Relative(10.0),
    )]));
    assert_eq!(state.font_size, 50.0);
}

#[test]
fn transform_blends_border_and_shadow_per_axis_and_clamps_after_blending() {
    let script = script_with(vec![default_style()]);
    let style = script.style(script.lookup_style("Default"));
    let mut state = timed_state(&script, style, 250);
    state.apply(&AssOverride::Border {
        axes: AssAxes::X,
        value: Some(10.0),
    });
    // `\bord` смешивается от СВОЕГО старого значения каждой оси: x = 10, y = 2 (стиль).
    state.apply(&quarter_way(vec![AssOverride::Border {
        axes: AssAxes::Both,
        value: Some(18.0),
    }]));
    assert_eq!(state.border, AxisValues { x: 12.0, y: 6.0 });
    // `\shad` зажимается снизу нулём после смеси, `\xshad` — нет.
    state.apply(&quarter_way(vec![
        AssOverride::Shadow {
            axes: AssAxes::Both,
            value: Some(-8.0),
        },
        AssOverride::Shadow {
            axes: AssAxes::X,
            value: Some(-40.0),
        },
    ]));
    // Сначала `\shad -8`: 0 * 0.75 - 8 * 0.25 = -2 -> 0; затем `\xshad -40`: 0 - 10 = -10.
    assert_eq!(state.shadow, AxisValues { x: -10.0, y: 0.0 });
}

#[test]
fn transform_blends_colors_per_channel_and_alpha_separately() {
    let script = script_with(vec![default_style()]);
    let style = script.style(script.lookup_style("Default"));
    let mut state = timed_state(&script, style, 250);
    state.apply(&AssOverride::Color {
        slot: AssColorSlot::Primary,
        value: Some(AssRgb::new(0, 0, 0)),
    });
    state.apply(&quarter_way(vec![
        AssOverride::Color {
            slot: AssColorSlot::Primary,
            value: Some(AssRgb::new(255, 100, 3)),
        },
        AssOverride::Alpha {
            target: AssAlphaTarget::Slot(AssColorSlot::Primary),
            value: Some(0xFF),
        },
    ]));
    // 0.25 * 255 = 63.75 -> 63; 0.25 * 100 = 25; 0.25 * 3 = 0.75 -> 0; альфа 63.75 -> 63.
    assert_eq!(state.colors[0], AssColor::new(63, 25, 0, 63));
}

#[test]
fn tags_without_animation_inside_a_transform_apply_immediately() {
    let script = script_with(vec![default_style()]);
    let style = script.style(script.lookup_style("Default"));
    // Момент до начала `\t`: вес 0, но `\i`, `\pos` и `\b` применяются сразу и целиком.
    let mut state = timed_state(&script, style, 0);
    state.apply(&crate::ass::test_script::transform_range(
        500,
        1000,
        vec![
            AssOverride::Italic(Some(true)),
            AssOverride::Position { x: 7.0, y: 8.0 },
            AssOverride::FontSize(AssFontSize::Absolute(90.0)),
        ],
    ));
    assert!(state.italic);
    assert_eq!(state.position, Some(ScriptPoint { x: 7.0, y: 8.0 }));
    // А анимируемый размер при весе 0 остался прежним.
    assert_eq!(state.font_size, 40.0);
}

#[test]
fn tags_after_a_transform_in_the_block_keep_full_weight_and_override_it() {
    let script = script_with(vec![default_style()]);
    let style = script.style(script.lookup_style("Default"));
    let mut state = timed_state(&script, style, 250);
    state.apply(&quarter_way(vec![AssOverride::FontSize(
        AssFontSize::Absolute(80.0),
    )]));
    assert_eq!(state.font_size, 50.0);
    // Внешний разбор после `\t` снова работает с весом 1.
    state.apply(&AssOverride::FontSize(AssFontSize::Absolute(30.0)));
    assert_eq!(state.font_size, 30.0);
}

#[test]
fn transform_blends_blur_and_edge_blur_and_clamps_afterwards() {
    let script = script_with(vec![default_style()]);
    let style = script.style(script.lookup_style("Default"));
    let mut state = timed_state(&script, style, 250);
    state.apply(&quarter_way(vec![
        AssOverride::Blur(Some(8.0)),
        AssOverride::EdgeBlur(Some(7.0)),
    ]));
    assert_eq!(state.filters.blur_radius(), 2.0);
    // be = dtoi32(0 * 0.75 + 7 * 0.25 + 0.5) = dtoi32(2.25) = 2.
    assert_eq!(state.filters.edge_blur(), 2);
    // Размытие ограничивается ПОСЛЕ смеси: 2 * 0.75 + 1000 * 0.25 = 251.5 -> 100.
    state.apply(&quarter_way(vec![AssOverride::Blur(Some(1000.0))]));
    assert_eq!(state.filters.blur_radius(), 100.0);
}

#[test]
fn transform_without_a_clock_applies_tags_in_full() {
    let script = script_with(vec![default_style()]);
    let style = script.style(script.lookup_style("Default"));
    let mut state = RenderState::new(&script, style, AssWrapStyle::Smart);
    state.apply(&quarter_way(vec![AssOverride::FontSize(
        AssFontSize::Absolute(80.0),
    )]));
    assert_eq!(state.font_size, 80.0);
}

#[test]
fn negative_static_values_are_clamped_by_the_state_not_the_parser() {
    // Парсер хранит значения как написано (для `\t`); без `\t` итог обязан быть прежним.
    let script = script_with(vec![default_style()]);
    let style = script.style(script.lookup_style("Default"));
    let mut state = RenderState::new(&script, style, AssWrapStyle::Smart);
    state.apply(&AssOverride::Border {
        axes: AssAxes::Both,
        value: Some(-3.0),
    });
    state.apply(&AssOverride::ScaleX(Some(-0.5)));
    state.apply(&AssOverride::Shadow {
        axes: AssAxes::Both,
        value: Some(-2.0),
    });
    assert_eq!(state.border, AxisValues::uniform(0.0));
    assert_eq!(state.scale.x, 0.0);
    assert_eq!(state.shadow, AxisValues::uniform(0.0));
    // `\yshad` не зажимается.
    state.apply(&AssOverride::Shadow {
        axes: AssAxes::Y,
        value: Some(-2.0),
    });
    assert_eq!(state.shadow, AxisValues { x: 0.0, y: -2.0 });
}

#[test]
fn transform_blends_before_clamping_negative_targets() {
    // libass: bord 6 -> -9 при весе 0.25 даёт 6 * 0.75 - 9 * 0.25 = 2.25 (а не зажатие цели в 0
    // до смеси, которое дало бы 4.5).
    let script = script_with(vec![default_style()]);
    let style = script.style(script.lookup_style("Default"));
    let mut state = timed_state(&script, style, 250);
    state.apply(&AssOverride::Border {
        axes: AssAxes::Both,
        value: Some(6.0),
    });
    state.apply(&quarter_way(vec![AssOverride::Border {
        axes: AssAxes::Both,
        value: Some(-9.0),
    }]));
    assert_eq!(state.border, AxisValues::uniform(2.25));
}

/// Прозрачность после `\t(0,1000,\alpha<значение>)` на половине пути (вес 0.5), от нуля.
fn alpha_after_half_transform(raw_alpha: i32) -> u8 {
    let script = script_with(vec![default_style()]);
    let style = script.style(script.lookup_style("Default"));
    let mut state = timed_state(&script, style, 500);
    state.apply(&crate::ass::test_script::transform_range(
        0,
        1000,
        vec![AssOverride::Alpha {
            target: AssAlphaTarget::All,
            value: Some(raw_alpha),
        }],
    ));
    state.colors[0].transparency
}

/// libass `change_alpha`: смесь считается над сырым `int32` (как `uint32`), младший байт
/// берётся ПОСЛЕ неё. Усечь значение до байта при разборе нельзя (результаты разные).
#[test]
fn transform_blends_raw_alpha_and_masks_after_interpolation() {
    // 0x1FF * 0.5 = 255.5 -> 255; при усечении до байта при разборе вышло бы 127.
    assert_eq!(alpha_after_half_transform(0x1FF), 0xFF);
    // 0x100 * 0.5 = 128; усечённое при разборе значение 0 дало бы 0.
    assert_eq!(alpha_after_half_transform(0x100), 0x80);
    // -1 как uint32 = 4294967295; * 0.5 = 2147483647.5 -> 2147483647 -> байт 0xFF.
    assert_eq!(alpha_after_half_transform(-1), 0xFF);
}

/// Результат смеси вне `int32` превращается в `INT32_MIN` (`dtoi32`), байт которого 0.
#[test]
fn transform_alpha_out_of_int32_range_collapses_to_zero_like_libass() {
    let script = script_with(vec![default_style()]);
    let style = script.style(script.lookup_style("Default"));
    // Вес 1 (после конца `\t`): -1 -> 4294967295.0 >= 2^31 -> INT32_MIN -> байт 0.
    let mut state = timed_state(&script, style, 5000);
    state.apply(&AssOverride::Alpha {
        target: AssAlphaTarget::All,
        value: Some(0x40),
    });
    state.apply(&crate::ass::test_script::transform_range(
        0,
        1000,
        vec![AssOverride::Alpha {
            target: AssAlphaTarget::All,
            value: Some(-1),
        }],
    ));
    assert_eq!(state.colors[0].transparency, 0);
}
