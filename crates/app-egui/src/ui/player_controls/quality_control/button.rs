//! Отрисовка и ввод кнопки качества «1080p».
//!
//! Один интерактивный виджет: левый клик открывает меню (как у «♪»). Подпись рисует
//! `ui-artwork-egui` текстом внутри скруглённой рамки, парной к рамкам CC и «♪»; hover, нажатие и клавиатурный фокус
//! проходят через тот же резолвер состояний, что у соседних кнопок. Клик мышью outline не
//! оставляет (`surrender_focus`).

use egui::{
    Key, Popup, PopupCloseBehavior, Rect, Response, Sense, Stroke, Ui, WidgetInfo, WidgetType,
};
use ui_artwork_egui::{ArtworkPainter, SubtitlesButtonPaintState, SubtitlesButtonStyle};

use super::layout::QualityControlLayout;
use super::{ControlAction, menu};
use crate::state::QualityMenuView;
use crate::ui::player_controls::queue_mode_controls::{
    HOVER_TRANSITION_SECONDS, PRESSED_TRANSITION_SECONDS, animated_bool, resolve_paint_state,
};
use crate::ui::skin::ControlsStyle;

/// Суффикс Id виджета кнопки (сохраняет фокус и анимации между кадрами).
const BUTTON_ID_SUFFIX: &str = "quality_button";
/// Подсказка кнопки.
const TOOLTIP: &str = "Video quality";

/// Состояние появления кнопки на кадр.
#[derive(Debug, Clone, Copy)]
pub(super) struct RevealState {
    /// Прогресс появления `0.0..=1.0`: прозрачность.
    pub(super) progress: f32,
    /// Политика движения: при reduced motion hover/press переключаются мгновенно.
    pub(super) reduced_motion: bool,
}

/// Рисует кнопку, добавляет действия, открывает меню.
pub(super) fn render(
    ui: &mut Ui,
    layout: &QualityControlLayout,
    view: &QualityMenuView,
    controls_style: ControlsStyle,
    reveal: RevealState,
    actions: &mut Vec<ControlAction>,
) {
    let anchor_id = ui.make_persistent_id(BUTTON_ID_SUFFIX);
    let popup_id = anchor_id.with("popup");
    let (Some(button_rect), Some(label)) = (layout.button_rect, view.tier_label()) else {
        // Кнопки нет в этом кадре: меню не должно остаться висеть без якоря.
        Popup::close_id(ui.ctx(), popup_id);
        return;
    };

    let response = ui.interact(button_rect, anchor_id, Sense::click());
    response.widget_info(|| {
        WidgetInfo::labeled(
            WidgetType::Button,
            ui.is_enabled(),
            format!("{TOOLTIP} {label}"),
        )
    });
    if response.clicked() && response.interact_pointer_pos().is_some() {
        response.surrender_focus();
    }
    let response = response.on_hover_text(TOOLTIP);

    paint(ui, &response, button_rect, &label, controls_style, reveal);
    // Левый клик переключает меню (поведение `Popup::menu` по умолчанию).
    Popup::menu(&response)
        .close_behavior(PopupCloseBehavior::CloseOnClickOutside)
        .show(|ui| menu::show(ui, view, actions));
}

/// Переводит hover/focus/press в paint-состояние и рисует рамку с подписью.
fn paint(
    ui: &Ui,
    response: &Response,
    rect: Rect,
    label: &str,
    controls_style: ControlsStyle,
    reveal: RevealState,
) {
    let persistent = controls_style.persistent_control;
    let hover_progress = animated_bool(
        ui,
        response.id.with("hover"),
        response.hovered(),
        HOVER_TRANSITION_SECONDS,
    );
    let keyboard_pressed = response.has_focus()
        && ui.input(|input| input.key_down(Key::Space) || input.key_down(Key::Enter));
    let pressed_progress = animated_bool(
        ui,
        response.id.with("pressed"),
        response.is_pointer_button_down_on() || keyboard_pressed,
        PRESSED_TRANSITION_SECONDS,
    );
    // Общий резолвер Shuffle/Repeat/CC/«♪»; постоянной «активной» поверхности у кнопки нет.
    let resolved = resolve_paint_state(
        persistent,
        ui.is_enabled(),
        response.has_focus(),
        reveal.reduced_motion,
        hover_progress,
        0.0,
        pressed_progress,
    );
    // Прозрачность появления накладывается на всё сразу.
    let opacity = reveal.progress.clamp(0.0, 1.0);
    let paint_state = SubtitlesButtonPaintState {
        foreground: resolved.foreground.gamma_multiply(opacity),
        surface_fill: resolved.surface_fill.gamma_multiply(opacity),
        focus_visible: resolved.focus_visible,
        content_scale: resolved.content_scale,
        enabled_progress: 0.0,
    };
    let paint_style = SubtitlesButtonStyle {
        icon_extent: controls_style.subtitles_button_icon_extent,
        glyph_stroke_width: controls_style.queue_mode_glyph_stroke_width,
        focus_outline: Stroke::new(
            controls_style.queue_mode_focus_outline_width,
            persistent.focus_outline,
        ),
        focus_inset: controls_style.queue_mode_focus_inset,
        underline_thickness: controls_style.subtitles_underline_thickness,
    };
    ArtworkPainter::new(ui.painter()).quality_label_button(rect, label, paint_state, paint_style);
}
