//! Отрисовка и ввод кнопки «♪» и стрелки меню «▾».
//!
//! У аудио нет переключателя «вкл/выкл»: левый клик по «♪» или по «▾» открывает меню. Поэтому
//! весь футпринт (кнопка и стрелка) — один интерактивный виджет и один якорь меню, а hover и
//! нажатие подсвечивают ту часть, над которой курсор. Клавиатурный фокус виден так же, как у
//! CC; клик мышью outline не оставляет (`surrender_focus`).

use egui::{
    Key, Popup, PopupCloseBehavior, Rect, Response, Sense, Stroke, Ui, WidgetInfo, WidgetType,
};
use ui_artwork_egui::{ArtworkPainter, SubtitlesButtonPaintState, SubtitlesButtonStyle};

use super::layout::{AudioTrackControlLayout, AudioTrackControlMode};
use super::{ControlAction, menu};
use crate::state::AudioTrackMenuView;
use crate::ui::player_controls::queue_mode_controls::{
    HOVER_TRANSITION_SECONDS, PRESSED_TRANSITION_SECONDS, animated_bool, resolve_paint_state,
};
use crate::ui::skin::ControlsStyle;

/// Суффикс Id виджета «♪» (сохраняет фокус и анимации между кадрами).
const BUTTON_ID_SUFFIX: &str = "audio_tracks_button";
/// Подсказка в полном режиме.
const TOOLTIP: &str = "Audio tracks (B = next)";
/// Подпись для скринридера.
const ACCESSIBLE_LABEL: &str = "Audio tracks";

/// Состояние появления «♪» на кадр.
#[derive(Debug, Clone, Copy)]
pub(super) struct RevealState {
    /// Прогресс появления `0.0..=1.0`: прозрачность кнопок.
    pub(super) progress: f32,
    /// Политика движения: при reduced motion hover/press переключаются мгновенно.
    pub(super) reduced_motion: bool,
}

/// Какая часть футпринта рисуется.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum ButtonPart {
    /// Значок «♪».
    Note,
    /// Стрелка «▾».
    MenuArrow,
}

/// Рисует «♪» (и стрелку), добавляет действия, открывает меню.
pub(super) fn render(
    ui: &mut Ui,
    layout: &AudioTrackControlLayout,
    view: &AudioTrackMenuView,
    controls_style: ControlsStyle,
    reveal: RevealState,
    actions: &mut Vec<ControlAction>,
) {
    let anchor_id = ui.make_persistent_id(BUTTON_ID_SUFFIX);
    let Some(button_rect) = layout.button_rect else {
        // Кнопки нет в этом кадре: меню не должно остаться висеть без якоря.
        Popup::close_id(ui.ctx(), anchor_id.with("popup"));
        return;
    };
    // Пока кнопка гаснет после пропажи выбора, клики по ней уже не принимаются.
    let accepts_input = view.has_choice();
    if !accepts_input {
        Popup::close_id(ui.ctx(), anchor_id.with("popup"));
    }
    let footprint = match (layout.mode, layout.arrow_rect) {
        (AudioTrackControlMode::Full, Some(arrow_rect)) => button_rect.union(arrow_rect),
        _ => button_rect,
    };

    let sense = if accepts_input {
        Sense::click()
    } else {
        Sense::hover()
    };
    let response = ui.interact(footprint, anchor_id, sense);
    response
        .widget_info(|| WidgetInfo::labeled(WidgetType::Button, ui.is_enabled(), ACCESSIBLE_LABEL));
    if response.clicked() && response.interact_pointer_pos().is_some() {
        response.surrender_focus();
    }
    let response = response.on_hover_text(TOOLTIP);

    paint_part(
        ui,
        &response,
        button_rect,
        controls_style,
        reveal,
        ButtonPart::Note,
    );
    if let Some(arrow_rect) = layout.arrow_rect {
        paint_part(
            ui,
            &response,
            arrow_rect,
            controls_style,
            reveal,
            ButtonPart::MenuArrow,
        );
    }
    if accepts_input {
        // Левый клик переключает меню (поведение `Popup::menu` по умолчанию).
        Popup::menu(&response)
            .close_behavior(PopupCloseBehavior::CloseOnClickOutside)
            .show(|ui| menu::show(ui, view, actions));
    }
}

/// Переводит hover/focus/press части кнопки в paint-состояние и рисует её.
fn paint_part(
    ui: &Ui,
    response: &Response,
    part_rect: Rect,
    controls_style: ControlsStyle,
    reveal: RevealState,
    part: ButtonPart,
) {
    let persistent = controls_style.persistent_control;
    let pointer_on_part = response
        .hover_pos()
        .is_some_and(|position| part_rect.contains(position));
    let hover_progress = animated_bool(
        ui,
        response.id.with(("hover", part_key(part))),
        pointer_on_part,
        HOVER_TRANSITION_SECONDS,
    );
    let keyboard_pressed = response.has_focus()
        && ui.input(|input| input.key_down(Key::Space) || input.key_down(Key::Enter));
    let pointer_pressed = response.is_pointer_button_down_on() && pointer_on_part;
    let pressed_progress = animated_bool(
        ui,
        response.id.with(("pressed", part_key(part))),
        pointer_pressed || keyboard_pressed,
        PRESSED_TRANSITION_SECONDS,
    );
    // Общий резолвер Shuffle/Repeat/CC; постоянной «активной» поверхности у «♪» нет.
    let resolved = resolve_paint_state(
        persistent,
        ui.is_enabled(),
        response.has_focus(),
        reveal.reduced_motion,
        hover_progress,
        0.0,
        pressed_progress,
    );
    let foreground = resolved.foreground;
    // Прозрачность появления накладывается на всё сразу.
    let opacity = reveal.progress.clamp(0.0, 1.0);
    let paint_state = SubtitlesButtonPaintState {
        foreground: foreground.gamma_multiply(opacity),
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
    let painter = ArtworkPainter::new(ui.painter());
    match part {
        ButtonPart::Note => painter.audio_tracks_button(part_rect, paint_state, paint_style),
        ButtonPart::MenuArrow => painter.subtitles_menu_arrow(part_rect, paint_state, paint_style),
    }
}

/// Стабильный ключ части для Id анимаций.
fn part_key(part: ButtonPart) -> &'static str {
    match part {
        ButtonPart::Note => "note",
        ButtonPart::MenuArrow => "arrow",
    }
}
