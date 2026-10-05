//! Функциональные тесты отрисовки уведомлений: настоящий egui-кадр, настоящие клики по ×,
//! действия возвращаются владельцу и меняют следующий кадр.

use std::time::{Duration, Instant};

use super::*;
use crate::ui::notifications::{
    NotificationAction, NotificationUiOutput, render_center_notice, render_toast_stack,
};

/// Размер тестового окна.
const SCREEN_SIZE: egui::Vec2 = egui::vec2(1280.0, 720.0);

/// Нарисованный текст и его прямоугольник на экране.
#[derive(Debug, Clone)]
struct PaintedText {
    text: String,
    rect: egui::Rect,
}

/// Собирает текстовые shape-ы рекурсивно.
fn collect_painted_text(shape: &egui::Shape, painted: &mut Vec<PaintedText>) {
    match shape {
        egui::Shape::Text(text_shape) => painted.push(PaintedText {
            text: text_shape.galley.text().to_owned(),
            rect: text_shape.visual_bounding_rect(),
        }),
        egui::Shape::Vec(shapes) => {
            for nested in shapes {
                collect_painted_text(nested, painted);
            }
        }
        _ => {}
    }
}

/// Результат одного кадра.
struct RenderedFrame {
    painted: Vec<PaintedText>,
    actions: Vec<NotificationAction>,
    repaint_delay: Duration,
}

impl RenderedFrame {
    /// Нарисован ли такой текст.
    fn shows(&self, text: &str) -> bool {
        self.painted.iter().any(|painted| painted.text == text)
    }

    /// Центр нарисованного текста (для клика).
    fn center_of(&self, text: &str) -> egui::Pos2 {
        self.painted
            .iter()
            .find(|painted| painted.text == text)
            .map(|painted| painted.rect.center())
            .unwrap_or_else(|| panic!("текст {text:?} не нарисован: {:?}", self.painted))
    }
}

/// Рисует кадр так же, как центральный overlay приложения: центр + стопка toast-ов.
fn render_frame(
    egui_ctx: &egui::Context,
    frame: &NotificationsFrame,
    events: Vec<egui::Event>,
) -> RenderedFrame {
    let mut notification_output = NotificationUiOutput::default();
    let output = crate::ui::test_frame::run_ui_frame(
        egui_ctx,
        egui::RawInput {
            screen_rect: Some(egui::Rect::from_min_size(egui::Pos2::ZERO, SCREEN_SIZE)),
            events: events.clone(),
            ..Default::default()
        },
        |ui| {
            egui::CentralPanel::default()
                .frame(egui::Frame::NONE)
                .show(ui, |ui| {
                    if let Some(notice) = frame.center.as_ref() {
                        render_center_notice(ui, notice, &mut notification_output);
                    }
                    let area_rect = ui.max_rect();
                    render_toast_stack(ui, area_rect, frame, &mut notification_output);
                });
        },
    );
    let mut painted = Vec::new();
    for clipped in &output.shapes {
        collect_painted_text(&clipped.shape, &mut painted);
    }
    let repaint_delay = output
        .viewport_output
        .get(&egui::ViewportId::ROOT)
        .map_or(Duration::MAX, |viewport| viewport.repaint_delay);
    RenderedFrame {
        painted,
        actions: notification_output.take_actions(),
        repaint_delay,
    }
}

/// Прогоняет несколько кадров без ввода: новая egui `Area` первый кадр только измеряется.
fn settle(egui_ctx: &egui::Context, frame: &NotificationsFrame) -> RenderedFrame {
    let mut rendered = render_frame(egui_ctx, frame, Vec::new());
    for _ in 0..2 {
        rendered = render_frame(egui_ctx, frame, Vec::new());
    }
    rendered
}

/// Настоящий клик мышью: наведение, нажатие и отпускание в отдельных кадрах.
fn click(
    egui_ctx: &egui::Context,
    frame: &NotificationsFrame,
    position: egui::Pos2,
) -> Vec<NotificationAction> {
    let pointer_button = |pressed| egui::Event::PointerButton {
        pos: position,
        button: egui::PointerButton::Primary,
        pressed,
        modifiers: egui::Modifiers::default(),
    };
    let mut actions = Vec::new();
    for events in [
        vec![egui::Event::PointerMoved(position)],
        vec![pointer_button(true)],
        vec![pointer_button(false)],
    ] {
        actions.extend(render_frame(egui_ctx, frame, events).actions);
    }
    actions
}

#[test]
fn center_failure_close_button_removes_failure_from_next_frame() {
    let now = Instant::now();
    let mut center = NotificationCenter::default();
    let failure_id = center.show_media_failure(
        "Не удалось открыть «broken.mkv»: файл повреждён",
        MediaFailureOrigin::MediaOpen,
    );
    let egui_ctx = egui::Context::default();

    let frame = center.frame(OpenProgress::Idle, UiMotion::Reduced, now);
    let rendered = settle(&egui_ctx, &frame);
    assert!(rendered.shows("Не удалось открыть «broken.mkv»: файл повреждён"));
    // Без клика никаких действий: уведомление не закрывается само.
    assert!(rendered.actions.is_empty());

    let actions = click(&egui_ctx, &frame, rendered.center_of("×"));
    assert_eq!(actions, [NotificationAction::Dismiss(failure_id)]);
    for NotificationAction::Dismiss(id) in actions {
        assert_eq!(center.dismiss(id), NotificationDismissOutcome::Dismissed);
    }

    let after_dismiss = settle(
        &egui_ctx,
        &center.frame(OpenProgress::Idle, UiMotion::Reduced, now),
    );
    assert!(!after_dismiss.shows("Не удалось открыть «broken.mkv»: файл повреждён"));
    assert!(!after_dismiss.shows("×"));
}

#[test]
fn toast_is_painted_in_bottom_right_corner_and_closes_by_click() {
    let now = Instant::now();
    let mut center = NotificationCenter::default();
    let toast_id = center.notify_transient(OPEN_STILL_IN_PROGRESS_MESSAGE, now);
    let egui_ctx = egui::Context::default();

    let frame = center.frame(OpenProgress::Idle, UiMotion::Reduced, now);
    let rendered = settle(&egui_ctx, &frame);
    let message_position = rendered.center_of(OPEN_STILL_IN_PROGRESS_MESSAGE);
    assert!(
        message_position.x > SCREEN_SIZE.x / 2.0 && message_position.y > SCREEN_SIZE.y / 2.0,
        "toast должен быть в правом нижнем углу: {message_position:?}"
    );

    let actions = click(&egui_ctx, &frame, rendered.center_of("×"));
    assert_eq!(actions, [NotificationAction::Dismiss(toast_id)]);
    assert_eq!(
        center.dismiss(toast_id),
        NotificationDismissOutcome::Dismissed
    );
    let after_dismiss = settle(
        &egui_ctx,
        &center.frame(OpenProgress::Idle, UiMotion::Reduced, now),
    );
    assert!(!after_dismiss.shows(OPEN_STILL_IN_PROGRESS_MESSAGE));
}

#[test]
fn transient_toast_is_no_longer_painted_after_lifetime() {
    let started_at = Instant::now();
    let mut center = NotificationCenter::default();
    center.notify_transient("Перемотка недоступна", started_at);
    let egui_ctx = egui::Context::default();

    let visible = settle(
        &egui_ctx,
        &center.frame(OpenProgress::Idle, UiMotion::Reduced, started_at),
    );
    assert!(visible.shows("Перемотка недоступна"));

    let expired = settle(
        &egui_ctx,
        &center.frame(
            OpenProgress::Idle,
            UiMotion::Reduced,
            started_at + TRANSIENT_NOTIFICATION_LIFETIME,
        ),
    );
    assert!(expired.painted.is_empty(), "{:?}", expired.painted);
}

#[test]
fn progress_in_center_has_no_close_button() {
    let now = Instant::now();
    let mut center = NotificationCenter::default();
    center.show_media_failure("Старая ошибка", MediaFailureOrigin::MediaOpen);
    let egui_ctx = egui::Context::default();

    let rendered = settle(
        &egui_ctx,
        &center.frame(
            OpenProgress::InProgress("Открываем «next.mkv»…"),
            UiMotion::Reduced,
            now,
        ),
    );
    let texts: Vec<&str> = rendered
        .painted
        .iter()
        .map(|painted| painted.text.as_str())
        .collect();
    assert_eq!(texts, ["Открываем «next.mkv»…"]);
}

#[test]
fn fading_toast_requests_repaint_only_with_standard_motion() {
    let now = Instant::now();
    let mut center = NotificationCenter::default();
    center.notify_transient("Перемотка недоступна", now);

    // Возраст 0: в обычном режиме плашка ещё проявляется и просит следующий кадр.
    let standard_ctx = crate::ui::test_frame::app_behavior_context();
    let standard = settle(
        &standard_ctx,
        &center.frame(OpenProgress::Idle, UiMotion::Standard, now),
    );
    assert_eq!(standard.repaint_delay, Duration::ZERO);

    // Reduced motion: плашка сразу непрозрачна, лишних кадров не нужно.
    let reduced_ctx = crate::ui::test_frame::app_behavior_context();
    let reduced = settle(
        &reduced_ctx,
        &center.frame(OpenProgress::Idle, UiMotion::Reduced, now),
    );
    assert!(reduced.shows("Перемотка недоступна"));
    assert_eq!(reduced.repaint_delay, Duration::MAX);
}
