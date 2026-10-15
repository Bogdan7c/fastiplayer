//! Функциональные тесты отрисовки уведомлений: настоящий egui-кадр, настоящие клики по ×,
//! действия возвращаются владельцу и меняют следующий кадр.

use std::time::{Duration, Instant};

use super::*;
use crate::external_open::request::DroppedWebUrl;
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
    /// Подписи кнопок в дереве accesskit (пусто, если accesskit в контексте не включён).
    accesskit_button_labels: Vec<String>,
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
            .unwrap_or_else(|| panic!("text {text:?} not drawn: {:?}", self.painted))
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
    let accesskit_button_labels = output
        .platform_output
        .accesskit_update
        .as_ref()
        .map(|update| {
            update
                .nodes
                .iter()
                .filter(|(_, node)| node.role() == egui::accesskit::Role::Button)
                .filter_map(|(_, node)| node.label().map(str::to_owned))
                .collect()
        })
        .unwrap_or_default();
    RenderedFrame {
        painted,
        actions: notification_output.take_actions(),
        repaint_delay,
        accesskit_button_labels,
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
        "broken.mkv failed to open: file corrupted",
        MediaFailureOrigin::MediaOpen,
    );
    let egui_ctx = egui::Context::default();

    let frame = center.frame(OpenProgress::Idle, UiMotion::Reduced, now);
    let rendered = settle(&egui_ctx, &frame);
    assert!(rendered.shows("broken.mkv failed to open: file corrupted"));
    // Без клика никаких действий: уведомление не закрывается само.
    assert!(rendered.actions.is_empty());

    let actions = click(&egui_ctx, &frame, rendered.center_of("×"));
    assert_eq!(actions, [NotificationAction::Dismiss(failure_id)]);
    assert_eq!(
        center.dismiss(failure_id),
        NotificationDismissOutcome::Dismissed
    );

    let after_dismiss = settle(
        &egui_ctx,
        &center.frame(OpenProgress::Idle, UiMotion::Reduced, now),
    );
    assert!(!after_dismiss.shows("broken.mkv failed to open: file corrupted"));
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
        "The toast should be in the lower right corner: {message_position:?}"
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
    center.notify_transient("Seeking unavailable", started_at);
    let egui_ctx = egui::Context::default();

    let visible = settle(
        &egui_ctx,
        &center.frame(OpenProgress::Idle, UiMotion::Reduced, started_at),
    );
    assert!(visible.shows("Seeking unavailable"));

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
            OpenProgress::InProgress("Opening “next.mkv”…"),
            UiMotion::Reduced,
            now,
        ),
    );
    let texts: Vec<&str> = rendered
        .painted
        .iter()
        .map(|painted| painted.text.as_str())
        .collect();
    assert_eq!(texts, ["Opening “next.mkv”…"]);
}

#[test]
fn fading_toast_requests_repaint_only_with_standard_motion() {
    let now = Instant::now();
    let mut center = NotificationCenter::default();
    center.notify_transient("Seeking unavailable", now);

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
    assert!(reduced.shows("Seeking unavailable"));
    assert_eq!(reduced.repaint_delay, Duration::MAX);
}

/// Сквозной сценарий сессии 05: битый `config.toml` на диске → восстановление config-ом →
/// текст предупреждения → настоящий egui-кадр. Плашка видна и через час, × её закрывает.
#[test]
fn broken_config_on_disk_reaches_painted_warning_that_stays_until_closed() {
    let config_directory = tempfile::tempdir().expect("dir temp created");
    let config_path = config_directory.path().join("config.toml");
    std::fs::write(&config_path, "schema_version = 13\n[[[\n").expect("broken config recorded");
    let loaded = fastiplayer_config::load_or_recover_at(&config_path, std::time::SystemTime::now())
        .expect("startup does not fail");
    let warning =
        crate::config_startup_notice::config_startup_warning(&loaded).expect("Recovery Warning");
    let fastiplayer_config::ConfigLoadOrigin::RecoveredFromBroken(recovery) = &loaded.origin else {
        panic!("The broken file must be recovered: {:?}", loaded.origin);
    };
    assert!(
        config_directory
            .path()
            .join(&recovery.backup_file_name)
            .is_file()
    );
    assert!(warning.contains(&recovery.backup_file_name));

    let now = Instant::now();
    let mut center = NotificationCenter::with_startup_messages(None, Some(warning.clone()), now);
    let egui_ctx = egui::Context::default();

    let hour_later = now + Duration::from_secs(3_600);
    let frame = center.frame(OpenProgress::Idle, UiMotion::Reduced, hour_later);
    let rendered = settle(&egui_ctx, &frame);
    assert!(
        rendered.shows(&warning),
        "Warning drawn: {:?}",
        rendered.painted
    );

    let actions = click(&egui_ctx, &frame, rendered.center_of("×"));
    let [NotificationAction::Dismiss(warning_id)] = actions.as_slice() else {
        panic!("Expected one closure: {actions:?}");
    };
    assert_eq!(
        center.dismiss(*warning_id),
        NotificationDismissOutcome::Dismissed
    );
    let after_dismiss = settle(
        &egui_ctx,
        &center.frame(OpenProgress::Idle, UiMotion::Reduced, hour_later),
    );
    assert!(!after_dismiss.shows(&warning));
}

/// Ошибка открытия с «Повторить» (B9): настоящий кадр рисует кнопку рядом с ×, клик по ней
/// даёт typed-действие `Retry`, а клик по × по-прежнему только закрывает.
#[test]
fn center_failure_with_retry_paints_button_and_click_emits_retry_action() {
    let now = Instant::now();
    let mut center = NotificationCenter::default();
    let failure_id = center.show_media_failure_planned(
        "Could not open the link (media.test): cannot connect to the server; check your internet connection",
        MediaFailureOrigin::MediaOpen,
        RetryPlan::Offered(MediaRetryTarget::WebUrl(DroppedWebUrl::new(
            "http://media.test/video.mp4".to_owned(),
        ))),
    );
    let egui_ctx = egui::Context::default();
    let frame = center.frame(OpenProgress::Idle, UiMotion::Reduced, now);
    let rendered = settle(&egui_ctx, &frame);

    assert!(rendered.shows(crate::ui::notifications::RETRY_BUTTON_LABEL));
    assert!(rendered.shows("×"));
    assert!(
        rendered.center_of("Repeat.").x < rendered.center_of("×").x,
        "\"Repeat\" to the left x"
    );
    assert!(rendered.actions.is_empty(), "no action without a click");

    let retry_actions = click(&egui_ctx, &frame, rendered.center_of("Repeat."));
    assert_eq!(retry_actions, [NotificationAction::Retry(failure_id)]);
    let close_actions = click(&egui_ctx, &frame, rendered.center_of("×"));
    assert_eq!(close_actions, [NotificationAction::Dismiss(failure_id)]);
}

/// Без решения владельца о повторе кнопки нет вовсе (формат не поддерживается и т.п.).
#[test]
fn center_failure_without_retry_paints_no_retry_button() {
    let now = Instant::now();
    let mut center = NotificationCenter::default();
    center.show_media_failure(
        "Could not open “clip.mkv”: file format is unrecognized or unsupported",
        MediaFailureOrigin::MediaOpen,
    );
    let egui_ctx = egui::Context::default();

    let rendered = settle(
        &egui_ctx,
        &center.frame(OpenProgress::Idle, UiMotion::Reduced, now),
    );

    assert!(!rendered.shows("Repeat."));
    assert!(rendered.shows("×"));
}

/// Доступность: «Повторить» — обычная кнопка egui с подписью, поэтому видна в дереве accesskit.
#[test]
fn retry_button_is_a_labeled_button_in_the_accesskit_tree() {
    let now = Instant::now();
    let mut center = NotificationCenter::default();
    center.show_media_failure_planned(
        "No connection to the server.",
        MediaFailureOrigin::PlayerFailed,
        RetryPlan::Offered(MediaRetryTarget::ResumeBlockedQueue),
    );
    let egui_ctx = egui::Context::default();
    egui_ctx.enable_accesskit();

    let rendered = settle(
        &egui_ctx,
        &center.frame(OpenProgress::Idle, UiMotion::Reduced, now),
    );

    assert!(
        rendered
            .accesskit_button_labels
            .iter()
            .any(|label| label == "Repeat."),
        "buttons accesskit: {:?}",
        rendered.accesskit_button_labels
    );
}

/// Сквозной путь B9 через настоящий кадр: ошибка сети при открытии URL → в центре
/// «Повторить» → клик → тот же URL уходит в путь открытия ссылки.
#[test]
fn clicking_retry_in_a_painted_frame_reopens_the_same_url() {
    use crate::state::media_retry::test_support::{RecordingRetryPort, RetryCall};

    let now = Instant::now();
    let url = DroppedWebUrl::new("http://media.test/video.mp4".to_owned());
    let reason = crate::media_open::WebOpenFailureReason::NoConnection;
    let mut center = NotificationCenter::default();
    center.show_media_failure_planned(
        crate::web_open_message::web_open_failure_message(Some("media.test"), reason),
        MediaFailureOrigin::MediaOpen,
        RetryPlan::for_reason(reason.into(), MediaRetryTarget::WebUrl(url.clone())),
    );
    let egui_ctx = egui::Context::default();
    let frame = center.frame(OpenProgress::Idle, UiMotion::Reduced, now);
    let rendered = settle(&egui_ctx, &frame);

    let actions = click(&egui_ctx, &frame, rendered.center_of("Repeat."));
    let [NotificationAction::Retry(failure_id)] = actions.as_slice() else {
        panic!("One repeat request was expected: {actions:?}");
    };
    // То же, что делает `AppState::apply_notification_actions` и исполнитель кадра.
    assert_eq!(
        center.request_retry(*failure_id),
        RetryRequestOutcome::Requested
    );
    let target = center.take_retry_request().expect("target accepted");
    let mut port = RecordingRetryPort::new(center);
    crate::state::media_retry::execute_media_retry(&mut port, target);

    assert_eq!(
        port.calls,
        [RetryCall::ClearFailure, RetryCall::OpenWebUrl(url)]
    );
    let after = settle(
        &egui_ctx,
        &port
            .center
            .frame(OpenProgress::Idle, UiMotion::Reduced, now),
    );
    assert!(!after.shows("Repeat."), "Error removed, no button");
}

/// Результат кадра «панель телеметрии справа + центральные уведомления» (G3).
struct TelemetryLayoutFrame {
    /// Область центральных уведомлений: всё, что не занято панелью телеметрии.
    central_rect: egui::Rect,
    /// Нарисованный текст с прямоугольниками и clip-областью, в которой он виден.
    painted: Vec<(PaintedText, egui::Rect)>,
    actions: Vec<NotificationAction>,
}

/// Рисует кадр так же, как кадр приложения: панель телеметрии, затем центральная область.
fn render_frame_with_telemetry(
    egui_ctx: &egui::Context,
    frame: &NotificationsFrame,
    screen_size: egui::Vec2,
    events: Vec<egui::Event>,
) -> TelemetryLayoutFrame {
    let mut notification_output = NotificationUiOutput::default();
    let mut central_rect = egui::Rect::NOTHING;
    let output = crate::ui::test_frame::run_ui_frame(
        egui_ctx,
        egui::RawInput {
            screen_rect: Some(egui::Rect::from_min_size(egui::Pos2::ZERO, screen_size)),
            events,
            ..Default::default()
        },
        |ui| {
            crate::state::AppState::render_telemetry_panel(ui, &[]);
            central_rect = ui.available_rect_before_wrap();
            egui::CentralPanel::default()
                .frame(egui::Frame::NONE)
                .show(ui, |ui| {
                    if let Some(notice) = frame.center.as_ref() {
                        render_center_notice(ui, notice, &mut notification_output);
                    }
                });
        },
    );
    let mut painted = Vec::new();
    for clipped in &output.shapes {
        let mut texts = Vec::new();
        collect_painted_text(&clipped.shape, &mut texts);
        painted.extend(texts.into_iter().map(|text| (text, clipped.clip_rect)));
    }
    TelemetryLayoutFrame {
        central_rect,
        painted,
        actions: notification_output.take_actions(),
    }
}

/// G3: при открытой панели телеметрии и минимальной ширине окна 400 кнопка «Повторить» и ×
/// видны целиком в области центральных уведомлений и не заходят под панель телеметрии.
#[test]
fn center_failure_buttons_stay_fully_visible_beside_the_telemetry_panel() {
    let message = "Could not open the link (127.0.0.1): cannot connect to the server; check your internet connection";
    let mut center = NotificationCenter::default();
    let failure_id = center.show_media_failure_planned(
        message,
        MediaFailureOrigin::MediaOpen,
        RetryPlan::Offered(MediaRetryTarget::ResumeBlockedQueue),
    );
    let frame = center.frame(OpenProgress::Idle, UiMotion::Reduced, Instant::now());

    for width in [400.0, 640.0, 1280.0] {
        let screen_size = egui::vec2(width, 600.0);
        let egui_ctx = egui::Context::default();
        let mut rendered = render_frame_with_telemetry(&egui_ctx, &frame, screen_size, Vec::new());
        for _ in 0..2 {
            rendered = render_frame_with_telemetry(&egui_ctx, &frame, screen_size, Vec::new());
        }

        assert!(
            rendered.central_rect.width() > 0.0,
            "Width {width}: Central region has no space: {:?}",
            rendered.central_rect
        );
        for label in ["Repeat.", "×"] {
            let (text, clip_rect) = rendered
                .painted
                .iter()
                .find(|(text, _)| text.text == label)
                .unwrap_or_else(|| {
                    panic!(
                        "width {width}: {label:?} not drawn; center {:?}; text {:?}",
                        rendered.central_rect, rendered.painted
                    )
                });
            assert!(
                rendered.central_rect.contains_rect(text.rect),
                "width {width}: {label:?} {:?} outside the central area {:?}",
                text.rect,
                rendered.central_rect
            );
            assert!(
                clip_rect.contains_rect(text.rect),
                "width {width}: {label:?} {:?} cropped clip-field {clip_rect:?}",
                text.rect
            );
        }

        // Кнопки не только видны, но и кликаются там, где нарисованы.
        let retry_center = rendered
            .painted
            .iter()
            .find(|(text, _)| text.text == "Repeat.")
            .map(|(text, _)| text.rect.center())
            .expect("\"Repeat\" button drawn");
        let pointer_button = |pressed| egui::Event::PointerButton {
            pos: retry_center,
            button: egui::PointerButton::Primary,
            pressed,
            modifiers: egui::Modifiers::default(),
        };
        let mut actions = Vec::new();
        for events in [
            vec![egui::Event::PointerMoved(retry_center)],
            vec![pointer_button(true)],
            vec![pointer_button(false)],
        ] {
            actions.extend(
                render_frame_with_telemetry(&egui_ctx, &frame, screen_size, events).actions,
            );
        }
        assert_eq!(
            actions,
            [NotificationAction::Retry(failure_id)],
            "Width {width}: Click on “Repeat” did not reach the owner"
        );
    }
}
