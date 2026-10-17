//! Помощники тестов модуля `ass`: сборка сценария и реплик без разбора файла.

use std::sync::Arc;

use subtitle_core::{
    AssAlignment, AssBorderStyle, AssColor, AssCue, AssEventFormat, AssEventMargins,
    AssHardOverrides, AssKerning, AssLayoutRes, AssOverride, AssPlayRes, AssScaledBorderAndShadow,
    AssScript, AssScriptInfo, AssScriptKind, AssStyle, AssTextItem, AssWrapStyle, AssYCbCrMatrix,
};

/// Параметры `[Script Info]` для тестов.
pub(super) fn script_info(play: (i32, i32), scaled: AssScaledBorderAndShadow) -> AssScriptInfo {
    AssScriptInfo {
        play_res: AssPlayRes::resolve(play.0, play.1),
        layout_res: None::<AssLayoutRes>,
        scaled_border_and_shadow: scaled,
        wrap_style: AssWrapStyle::Smart,
        kerning: AssKerning::Disabled,
        ycbcr_matrix: AssYCbCrMatrix::Default,
        language: None,
    }
}

/// Стиль «Default»: Noto Sans 40, белый, контур 2, тень 0, снизу по центру, поля 10.
pub(super) fn default_style() -> AssStyle {
    AssStyle {
        name: "Default".to_owned(),
        font_name: "Noto Sans".to_owned(),
        font_size: 40.0,
        primary_color: AssColor::new(255, 255, 255, 0),
        secondary_color: AssColor::new(255, 0, 0, 0),
        outline_color: AssColor::new(0, 0, 0, 0),
        back_color: AssColor::new(0, 0, 0, 0x80),
        bold: subtitle_core::AssBold::Normal,
        italic: false,
        underline: false,
        strike_out: false,
        scale_x: 1.0,
        scale_y: 1.0,
        spacing: 0.0,
        angle: 0.0,
        border_style: AssBorderStyle::OutlineAndShadow,
        outline: 2.0,
        shadow: 0.0,
        alignment: AssAlignment::BOTTOM_CENTER,
        margin_left: 10,
        margin_right: 10,
        margin_vertical: 10,
        encoding: 1,
    }
}

/// Сценарий PlayRes 1280x720 с заданными стилями.
pub(super) fn script_with(styles: Vec<AssStyle>) -> Arc<AssScript> {
    Arc::new(AssScript::new(
        AssScriptKind::Ass,
        script_info((1280, 720), AssScaledBorderAndShadow::Enabled),
        AssEventFormat::new(Vec::new()),
        styles,
    ))
}

/// Реплика стиля `Default` с заданным текстом.
pub(super) fn cue_with(script: &Arc<AssScript>, text: Vec<AssTextItem>) -> AssCue {
    AssCue {
        script: Arc::clone(script),
        style: script.lookup_style("Default"),
        read_order: 0,
        margins: AssEventMargins::default(),
        name: String::new(),
        effect: String::new(),
        hard_overrides: AssHardOverrides::Absent,
        text,
    }
}

/// Блок тегов.
pub(super) fn tags(overrides: Vec<AssOverride>) -> AssTextItem {
    AssTextItem::Overrides(overrides)
}

/// Кусок текста.
pub(super) fn text(content: &str) -> AssTextItem {
    AssTextItem::Text(content.to_owned())
}

/// Тег `\t` с заданными временами и вложенными тегами.
pub(super) fn transform(
    timing: subtitle_core::AssTransformTiming,
    nested: Vec<AssOverride>,
) -> AssOverride {
    AssOverride::Transform(subtitle_core::AssTransform {
        timing,
        tags: nested,
    })
}

/// `\t(t1,t2,...)` без ускорения.
pub(super) fn transform_range(t1_ms: i32, t2_ms: i32, nested: Vec<AssOverride>) -> AssOverride {
    transform(
        subtitle_core::AssTransformTiming::Range { t1_ms, t2_ms },
        nested,
    )
}
