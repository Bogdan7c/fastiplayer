//! Отрисовка и ввод кнопки CC и стрелки меню «▾».
//!
//! Доступность: CC — кнопка с состоянием «выбрана» (`WidgetInfo::selected`), стрелка —
//! кнопка «Subtitle options». Клик мышью не оставляет keyboard-focus outline
//! (`surrender_focus`), клавиатурный фокус виден так же, как у Repeat.

use egui::{
    Id, Key, Popup, PopupCloseBehavior, Rect, Response, Sense, Stroke, Ui, WidgetInfo, WidgetType,
};
use ui_artwork_egui::{ArtworkPainter, SubtitlesButtonPaintState, SubtitlesButtonStyle};

use super::layout::{SubtitleControlLayout, SubtitleControlMode};
use super::menu;
use super::{ControlAction, SubtitleControlAction};
use crate::state::{SubtitleMenuView, SubtitleVisibility};
use crate::ui::player_controls::queue_mode_controls::{
    ACTIVE_TRANSITION_SECONDS, HOVER_TRANSITION_SECONDS, PRESSED_TRANSITION_SECONDS, animated_bool,
    lerp_color, resolve_paint_state,
};
use crate::ui::skin::ControlsStyle;

/// Суффикс Id кнопки CC (сохраняет фокус и анимации между кадрами).
const CC_ID_SUFFIX: &str = "subtitle_cc_button";
/// Суффикс Id стрелки меню.
const ARROW_ID_SUFFIX: &str = "subtitle_options_button";

/// Подсказка кнопки CC в полном режиме.
const TOOLTIP_FULL: &str = "Subtitles (C)";
/// Подсказка кнопки CC в компактном режиме: меню открывается правым кликом.
const TOOLTIP_COMPACT: &str = "Subtitles (C) — right-click for options";
/// Подпись и подсказка стрелки.
const OPTIONS_LABEL: &str = "Subtitle options";
/// Подпись CC для скринридера.
const CC_LABEL: &str = "Subtitles";

/// Состояние появления CC на кадр.
#[derive(Debug, Clone, Copy)]
pub(super) struct RevealState {
    /// Прогресс появления `0.0..=1.0`: прозрачность кнопок.
    pub(super) progress: f32,
    /// Политика движения: при reduced motion подчёркивание переключается мгновенно.
    pub(super) reduced_motion: bool,
}

/// Рисует CC (и стрелку), добавляет действия, открывает меню.
pub(super) fn render(
    ui: &mut Ui,
    layout: &SubtitleControlLayout,
    view: &SubtitleMenuView,
    controls_style: ControlsStyle,
    reveal: RevealState,
    actions: &mut Vec<ControlAction>,
) {
    let cc_id = ui.make_persistent_id(CC_ID_SUFFIX);
    let arrow_id = ui.make_persistent_id(ARROW_ID_SUFFIX);
    let Some(button_rect) = layout.button_rect else {
        // Кнопки нет в этом кадре: меню не должно остаться висеть без якоря.
        close_menu_popups(ui, cc_id, arrow_id);
        return;
    };
    // Пока кнопка гаснет после пропажи дорожек, клики по ней уже не принимаются.
    let accepts_input = view.has_tracks;
    if !accepts_input {
        close_menu_popups(ui, cc_id, arrow_id);
    }

    let cc_response = interact(ui, cc_id, button_rect, accepts_input);
    cc_response.widget_info(|| {
        WidgetInfo::selected(
            WidgetType::Button,
            ui.is_enabled(),
            view.visibility.is_enabled(),
            CC_LABEL,
        )
    });
    if cc_response.clicked() && cc_response.interact_pointer_pos().is_some() {
        cc_response.surrender_focus();
    }
    let cc_response = cc_response.on_hover_text(match layout.mode {
        SubtitleControlMode::Compact => TOOLTIP_COMPACT,
        SubtitleControlMode::Full | SubtitleControlMode::Hidden => TOOLTIP_FULL,
    });
    if accepts_input && cc_response.clicked() {
        actions.push(ControlAction::Subtitles(
            SubtitleControlAction::ToggleVisibility,
        ));
    }
    paint_button(
        ui,
        &cc_response,
        view,
        controls_style,
        reveal,
        ButtonPart::Cc,
    );

    match (layout.mode, layout.arrow_rect) {
        (SubtitleControlMode::Full, Some(arrow_rect)) => {
            let arrow_response = interact(ui, arrow_id, arrow_rect, accepts_input);
            arrow_response.widget_info(|| {
                WidgetInfo::labeled(WidgetType::Button, ui.is_enabled(), OPTIONS_LABEL)
            });
            if arrow_response.clicked() && arrow_response.interact_pointer_pos().is_some() {
                arrow_response.surrender_focus();
            }
            let arrow_response = arrow_response.on_hover_text(OPTIONS_LABEL);
            paint_button(
                ui,
                &arrow_response,
                view,
                controls_style,
                reveal,
                ButtonPart::MenuArrow,
            );
            if accepts_input {
                // Левый клик по стрелке переключает меню (поведение `Popup::menu` по умолчанию).
                show_menu(Popup::menu(&arrow_response), view, actions);
            }
        }
        (SubtitleControlMode::Compact, _) if accepts_input => {
            // Правый клик открывает меню у курсора; левый клик переключает субтитры, а
            // `context_menu` при этом закрывает уже открытое меню.
            show_menu(Popup::context_menu(&cc_response), view, actions);
        }
        _ => {}
    }
}

/// Создаёт интерактивную область: кликабельную, пока у видео есть дорожки.
fn interact(ui: &Ui, id: Id, rect: Rect, accepts_input: bool) -> Response {
    let sense = if accepts_input {
        Sense::click()
    } else {
        Sense::hover()
    };
    ui.interact(rect, id, sense)
}

/// Закрывает меню обеих кнопок (якоря больше нет или дорожек не осталось).
fn close_menu_popups(ui: &Ui, cc_id: Id, arrow_id: Id) {
    for anchor_id in [cc_id, arrow_id] {
        // Id попапа выводится из Id отклика так же, как в `Popup::default_response_id`.
        Popup::close_id(ui.ctx(), anchor_id.with("popup"));
    }
}

/// Показывает меню у якоря: закрывается кликом вне меню, Esc или явным `ui.close()`.
fn show_menu(popup: Popup<'_>, view: &SubtitleMenuView, actions: &mut Vec<ControlAction>) {
    // Клик по недоступной дорожке не должен закрывать меню, поэтому закрываем явно из
    // `menu::show` только после настоящего выбора.
    popup
        .close_behavior(PopupCloseBehavior::CloseOnClickOutside)
        .show(|ui| menu::show(ui, view, actions));
}

/// Какая часть кнопки рисуется.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum ButtonPart {
    /// Значок «CC» с подчёркиванием.
    Cc,
    /// Стрелка «▾».
    MenuArrow,
}

/// Переводит hover/focus/press/enabled в нейтральное paint-состояние и рисует.
fn paint_button(
    ui: &Ui,
    response: &Response,
    view: &SubtitleMenuView,
    controls_style: ControlsStyle,
    reveal: RevealState,
    part: ButtonPart,
) {
    let persistent = controls_style.persistent_control;
    let hover_progress = animated_bool(
        ui,
        response.id.with("hover"),
        response.hovered() && view.has_tracks,
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
    let enabled_target = view.visibility == SubtitleVisibility::Shown;
    let enabled_progress = if reveal.reduced_motion {
        f32::from(u8::from(enabled_target))
    } else {
        animated_bool(
            ui,
            response.id.with("enabled"),
            enabled_target,
            ACTIVE_TRANSITION_SECONDS,
        )
    };
    // Общий резолвер Shuffle/Repeat; «активное» состояние показывает подчёркивание, поэтому
    // постоянной поверхности у CC нет (active_progress = 0).
    let resolved = resolve_paint_state(
        persistent,
        ui.is_enabled(),
        response.has_focus(),
        reveal.reduced_motion,
        hover_progress,
        0.0,
        pressed_progress,
    );
    let foreground = lerp_color(
        resolved.foreground,
        persistent.foreground_active,
        enabled_progress,
    );
    // Прозрачность появления накладывается на всё сразу.
    let opacity = reveal.progress.clamp(0.0, 1.0);
    let paint_state = SubtitlesButtonPaintState {
        foreground: foreground.gamma_multiply(opacity),
        surface_fill: resolved.surface_fill.gamma_multiply(opacity),
        focus_visible: resolved.focus_visible,
        content_scale: resolved.content_scale,
        enabled_progress: enabled_progress * opacity,
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
        ButtonPart::Cc => painter.subtitles_button(response.rect, paint_state, paint_style),
        ButtonPart::MenuArrow => {
            painter.subtitles_menu_arrow(response.rect, paint_state, paint_style);
        }
    }
}
