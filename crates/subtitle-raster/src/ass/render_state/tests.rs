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
