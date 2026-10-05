//! Отрисовка уведомлений: сообщение в центре и стопка toast-ов в правом нижнем углу.
//!
//! Модуль ничего не решает о жизненном цикле — он рисует готовую проекцию
//! [`NotificationsFrame`] от владельца `state::notifications` и возвращает typed-действия
//! (нажатый ×) через [`NotificationUiOutput`]. Время и состояние здесь не хранятся.
//!
//! Рисуется только стандартными виджетами egui (`Frame`, `Label`, кнопка); собственных
//! paint-примитивов нет, поэтому `ui-artwork-egui` не нужен.

use std::time::Duration;

use animation_core::Easing;

use crate::state::{CenterNotice, NotificationId, NotificationsFrame, ToastKind, ToastView};
use crate::ui::animation::UiMotion;

/// Отступ сообщения в центре от верха центральной области (как было до сессии 04).
const CENTER_NOTICE_TOP_OFFSET: f32 = 40.0;

/// Максимальная ширина плашки ошибки в центре: длинный текст переносится, а не
/// растягивается на всё окно.
const CENTER_NOTICE_MAX_WIDTH: f32 = 560.0;

/// Максимальная ширина toast-а в углу.
const TOAST_MAX_WIDTH: f32 = 360.0;

/// Отступ стопки toast-ов от правого нижнего угла центральной области.
const TOAST_STACK_MARGIN: f32 = 16.0;

/// Расстояние между соседними toast-ами.
const TOAST_SPACING: f32 = 8.0;

/// Длительность плавного проявления нового toast-а.
///
/// Короткая, чтобы не задерживать чтение; при reduced motion проявления нет совсем.
const TOAST_FADE_IN: Duration = Duration::from_millis(150);

/// Цвет текста прогресса в центре (как было до сессии 04).
const PROGRESS_TEXT_COLOR: egui::Color32 = egui::Color32::LIGHT_BLUE;

/// Символ и подсказка кнопки закрытия.
const DISMISS_BUTTON_LABEL: &str = "×";
const DISMISS_BUTTON_HINT: &str = "Закрыть";

/// Stable egui id области toast-ов: одна область на всё окно.
const TOAST_STACK_AREA_ID: &str = "fastiplayer_notification_toasts";

/// Действие пользователя с уведомлением.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum NotificationAction {
    /// Нажат × у уведомления с этим id.
    Dismiss(NotificationId),
}

/// Действия, собранные за кадр; применяются владельцем после egui pass-а.
#[derive(Debug, Default)]
pub(crate) struct NotificationUiOutput {
    actions: Vec<NotificationAction>,
}

impl NotificationUiOutput {
    /// Забирает накопленные действия.
    pub(crate) fn take_actions(&mut self) -> Vec<NotificationAction> {
        std::mem::take(&mut self.actions)
    }

    /// Запоминает действие пользователя.
    fn push(&mut self, action: NotificationAction) {
        self.actions.push(action);
    }
}

/// Рисует сообщение центра: прогресс голубым текстом, фатальную ошибку — плашкой с ×.
pub(crate) fn render_center_notice(
    ui: &mut egui::Ui,
    notice: &CenterNotice,
    output: &mut NotificationUiOutput,
) {
    ui.vertical_centered(|ui| {
        ui.add_space(CENTER_NOTICE_TOP_OFFSET);
        match notice {
            CenterNotice::Progress(message) => {
                ui.colored_label(PROGRESS_TEXT_COLOR, message.as_ref());
            }
            CenterNotice::MediaFailure(failure) => {
                let error_color = ui.visuals().error_fg_color;
                egui::Frame::popup(ui.style()).show(ui, |ui| {
                    ui.set_max_width(CENTER_NOTICE_MAX_WIDTH);
                    ui.horizontal(|ui| {
                        ui.add(
                            egui::Label::new(
                                egui::RichText::new(failure.message.as_ref()).color(error_color),
                            )
                            .wrap(),
                        );
                        if dismiss_button(ui) {
                            output.push(NotificationAction::Dismiss(failure.id));
                        }
                    });
                });
            }
        }
    });
}

/// Рисует стопку toast-ов поверх видео в правом нижнем углу `area_rect`.
///
/// `area_rect` — центральная область без sidebar-а и панели управления, поэтому плашки
/// не перекрывают кнопки транспорта. Самый новый toast — ближе всего к углу.
pub(crate) fn render_toast_stack(
    ui: &egui::Ui,
    area_rect: egui::Rect,
    frame: &NotificationsFrame,
    output: &mut NotificationUiOutput,
) {
    if frame.toasts.is_empty() {
        return;
    }
    let anchor = area_rect.right_bottom() - egui::vec2(TOAST_STACK_MARGIN, TOAST_STACK_MARGIN);
    egui::Area::new(egui::Id::new(TOAST_STACK_AREA_ID))
        .order(egui::Order::Foreground)
        .pivot(egui::Align2::RIGHT_BOTTOM)
        .fixed_pos(anchor)
        .constrain_to(area_rect)
        // Встроенное проявление egui не знает про reduced motion и дублировало бы наше:
        // единственный владелец анимации — `toast_opacity`.
        .fade_in(false)
        .show(ui.ctx(), |ui| {
            ui.set_max_width(TOAST_MAX_WIDTH);
            ui.spacing_mut().item_spacing.y = TOAST_SPACING;
            // Проекция хранит новые первыми, а рисуем сверху вниз: новый — последним, у угла.
            for toast in frame.toasts.iter().rev() {
                ui.push_id(toast.id, |ui| render_toast(ui, toast, frame.motion, output));
            }
        });
}

/// Рисует один toast с плавным проявлением.
fn render_toast(
    ui: &mut egui::Ui,
    toast: &ToastView,
    motion: UiMotion,
    output: &mut NotificationUiOutput,
) {
    let opacity = toast_opacity(toast.age, motion);
    if opacity < 1.0 {
        // Пока плашка проявляется, нужен следующий кадр даже на паузе.
        ui.ctx().request_repaint();
    }
    let text_color = match toast.kind {
        ToastKind::Transient => ui.visuals().warn_fg_color,
        ToastKind::Info => ui.visuals().text_color(),
        // Важное предупреждение выделено цветом ошибки: оно о потере настроек.
        ToastKind::Warning => ui.visuals().error_fg_color,
    };
    ui.scope(|ui| {
        ui.multiply_opacity(opacity);
        egui::Frame::popup(ui.style()).show(ui, |ui| {
            ui.horizontal(|ui| {
                ui.add(
                    egui::Label::new(egui::RichText::new(toast.message.as_ref()).color(text_color))
                        .wrap(),
                );
                if dismiss_button(ui) {
                    output.push(NotificationAction::Dismiss(toast.id));
                }
            });
        });
    });
}

/// Кнопка ×; возвращает `true`, если по ней кликнули в этом кадре.
fn dismiss_button(ui: &mut egui::Ui) -> bool {
    ui.small_button(DISMISS_BUTTON_LABEL)
        .on_hover_text(DISMISS_BUTTON_HINT)
        .clicked()
}

/// Непрозрачность toast-а по его возрасту: проявление за [`TOAST_FADE_IN`].
///
/// Считается от возраста, который дал владелец, а не через внутренний таймер egui:
/// так результат детерминирован и проверяется тестом с фальшивым временем.
fn toast_opacity(age: Duration, motion: UiMotion) -> f32 {
    if motion == UiMotion::Reduced || age >= TOAST_FADE_IN {
        return 1.0;
    }
    let linear_progress = age.as_secs_f32() / TOAST_FADE_IN.as_secs_f32();
    Easing::EaseOutCubic.apply(linear_progress)
}

#[cfg(test)]
mod tests;
