//! Панель настроек режима правки: цвета, прозрачность плашки, оформление и кнопки выхода.
//!
//! Панель - `egui::Area` порядка `Foreground` вверху свободной области видео по центру.
//! Не внизу: там стоят панель управления и субтитры по умолчанию, и панель закрыла бы
//! именно то, что пользователь настраивает.

use egui::{Align2, Context, Id, Order, Response, Ui, pos2};
use fastiplayer_config::{MAX_BACKGROUND_OPACITY_PERCENT, SubtitleDecorationPreference};

use super::{SubtitleAppearanceEditAction, SubtitleAppearanceEditInput};
use crate::state::{AppearanceEdit, AppearanceEditFinish, SubtitleRgb, UserSubtitleStyle};

/// Стабильный id области панели.
const PANEL_AREA_ID: &str = "subtitle_appearance_edit_panel";

/// Подсказка жестов под элементами панели.
const GESTURE_HINT: &str =
    "Drag the text to move it. Drag a corner or use the mouse wheel to resize.";

/// Рисует панель и возвращает намерения кадра.
pub(super) fn show(
    egui_ctx: &Context,
    input: SubtitleAppearanceEditInput<'_>,
) -> Vec<SubtitleAppearanceEditAction> {
    let SubtitleAppearanceEditInput {
        view,
        video_rect,
        style,
    } = input;
    if !video_rect.is_positive() {
        return Vec::new();
    }
    let frame = egui::Frame::NONE
        .fill(style.panel_fill)
        .stroke(style.panel_stroke)
        .corner_radius(style.panel_corner_radius)
        .inner_margin(style.panel_inner_margin);
    egui::Area::new(Id::new(PANEL_AREA_ID))
        .order(Order::Foreground)
        .pivot(Align2::CENTER_TOP)
        .fixed_pos(pos2(
            video_rect.center().x,
            video_rect.top() + style.panel_top_margin,
        ))
        .show(egui_ctx, |ui| {
            frame
                .show(ui, |ui| {
                    // Панель не шире видео: на узком окне элементы переносятся на новую строку.
                    ui.set_max_width(
                        (video_rect.width() - 2.0 * style.panel_inner_margin).max(0.0),
                    );
                    panel_contents(ui, &view.draft)
                })
                .inner
        })
        .inner
}

/// Содержимое панели для текущего черновика.
fn panel_contents(ui: &mut Ui, draft: &UserSubtitleStyle) -> Vec<SubtitleAppearanceEditAction> {
    let mut actions = Vec::new();
    ui.horizontal_wrapped(|ui| {
        ui.label("Text");
        if let Some(color) = color_button(ui, draft.text_color) {
            actions.push(edit(AppearanceEdit::SetTextColor(color)));
        }
        ui.label("Background");
        if let Some(color) = color_button(ui, draft.background_color) {
            actions.push(edit(AppearanceEdit::SetBackgroundColor(color)));
        }
        if let Some(percent) = opacity_slider(ui, draft.background_opacity_percent) {
            actions.push(edit(AppearanceEdit::SetBackgroundOpacity { percent }));
        }
        if let Some(decoration) = decoration_selector(ui, draft.decoration) {
            actions.push(edit(AppearanceEdit::SetDecoration(decoration)));
        }
    });
    ui.horizontal_wrapped(|ui| {
        if action_button(ui, "Reset") {
            actions.push(SubtitleAppearanceEditAction::Reset);
        }
        if action_button(ui, "Cancel") {
            actions.push(SubtitleAppearanceEditAction::Finish(
                AppearanceEditFinish::Cancel,
            ));
        }
        if action_button(ui, "Done") {
            actions.push(SubtitleAppearanceEditAction::Finish(
                AppearanceEditFinish::Save,
            ));
        }
    });
    ui.weak(GESTURE_HINT);
    actions
}

/// Обёртка: правка черновика как намерение.
fn edit(change: AppearanceEdit) -> SubtitleAppearanceEditAction {
    SubtitleAppearanceEditAction::Edit(change)
}

/// Кнопка цвета; возвращает новый цвет, если пользователь его изменил.
///
/// Окно выбора цвета - попап egui: пока оно открыто, Esc закрывает его (приоритет
/// `EscapeTarget::EguiPopup`), и только следующий Esc отменяет правку.
fn color_button(ui: &mut Ui, current: SubtitleRgb) -> Option<SubtitleRgb> {
    let mut channels = [current.red, current.green, current.blue];
    let response = ui.color_edit_button_srgb(&mut channels);
    surrender_focus_after_pointer(&response);
    response.changed().then_some(SubtitleRgb {
        red: channels[0],
        green: channels[1],
        blue: channels[2],
    })
}

/// Ползунок непрозрачности плашки `0..=100 %`; возвращает значение, только если его меняет пользователь.
fn opacity_slider(ui: &mut Ui, current_percent: u8) -> Option<u8> {
    let mut percent = current_percent;
    let response = ui.add(
        egui::Slider::new(&mut percent, 0..=MAX_BACKGROUND_OPACITY_PERCENT)
            .text("Opacity")
            .suffix(" %")
            // `Edits`: отрисовка не должна менять значение (инвариант слайдеров проекта).
            .clamping(egui::SliderClamping::Edits),
    );
    if response.drag_stopped() {
        // После перетаскивания мышью фокус не остаётся на ползунке, иначе Esc ушёл бы на
        // снятие фокуса, а не на отмену правки.
        response.surrender_focus();
    }
    (response.changed() && percent != current_percent).then_some(percent)
}

/// Выбор «Box / Outline»; возвращает новое значение при клике.
fn decoration_selector(
    ui: &mut Ui,
    current: SubtitleDecorationPreference,
) -> Option<SubtitleDecorationPreference> {
    let mut chosen = None;
    for (decoration, label) in [
        (SubtitleDecorationPreference::Box, "Box"),
        (SubtitleDecorationPreference::Outline, "Outline"),
    ] {
        let response = ui.selectable_label(current == decoration, label);
        surrender_focus_after_pointer(&response);
        if response.clicked() && current != decoration {
            chosen = Some(decoration);
        }
    }
    chosen
}

/// Кнопка панели; `true` при клике. Фокус после мыши отдаётся (см. документацию модуля).
fn action_button(ui: &mut Ui, label: &str) -> bool {
    let response = ui.button(label);
    surrender_focus_after_pointer(&response);
    response.clicked()
}

/// Отдаёт клавиатурный фокус после клика мышью; Tab-фокус остаётся.
///
/// Так же устроены кнопки play/next/shuffle: `clicked() && interact_pointer_pos().is_some()`.
fn surrender_focus_after_pointer(response: &Response) {
    if response.clicked() && response.interact_pointer_pos().is_some() {
        response.surrender_focus();
    }
}
