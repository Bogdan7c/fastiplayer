//! Плашка рекламы в настоящем кадре egui: видны правильные подписи, клик даёт действие,
//! занятая кнопка не кликается.

use super::*;

/// Размер тестового окна.
const SCREEN_SIZE: egui::Vec2 = egui::vec2(1280.0, 720.0);

/// Нарисованный текст и его прямоугольник.
struct Painted {
    text: String,
    rect: egui::Rect,
}

fn collect(shape: &egui::Shape, painted: &mut Vec<Painted>) {
    match shape {
        egui::Shape::Text(text_shape) => painted.push(Painted {
            text: text_shape.galley.text().to_owned(),
            rect: text_shape.visual_bounding_rect(),
        }),
        egui::Shape::Vec(shapes) => shapes.iter().for_each(|nested| collect(nested, painted)),
        _ => {}
    }
}

/// Один кадр с плашкой `plate` и событиями `events`: что нарисовано и какие действия.
fn frame(
    egui_ctx: &egui::Context,
    plate: &AdPlate,
    events: Vec<egui::Event>,
) -> (Vec<Painted>, Vec<NotificationAction>) {
    let mut output = NotificationUiOutput::default();
    let full = crate::ui::test_frame::run_ui_frame(
        egui_ctx,
        egui::RawInput {
            screen_rect: Some(egui::Rect::from_min_size(egui::Pos2::ZERO, SCREEN_SIZE)),
            events,
            ..Default::default()
        },
        |ui| {
            egui::CentralPanel::default()
                .frame(egui::Frame::NONE)
                .show(ui, |ui| {
                    let area = ui.max_rect();
                    render_ad_plate(ui, area, plate, &mut output);
                });
        },
    );
    let mut painted = Vec::new();
    for clipped in &full.shapes {
        collect(&clipped.shape, &mut painted);
    }
    (painted, output.take_actions())
}

/// Новая `Area` первый кадр только измеряется: прогоняем три.
fn settle(egui_ctx: &egui::Context, plate: &AdPlate) -> Vec<Painted> {
    let mut painted = Vec::new();
    for _ in 0..3 {
        painted = frame(egui_ctx, plate, Vec::new()).0;
    }
    painted
}

fn click(
    egui_ctx: &egui::Context,
    plate: &AdPlate,
    position: egui::Pos2,
) -> Vec<NotificationAction> {
    let button = |pressed| egui::Event::PointerButton {
        pos: position,
        button: egui::PointerButton::Primary,
        pressed,
        modifiers: egui::Modifiers::default(),
    };
    let mut actions = Vec::new();
    for events in [
        vec![egui::Event::PointerMoved(position)],
        vec![button(true)],
        vec![button(false)],
    ] {
        actions.extend(frame(egui_ctx, plate, events).1);
    }
    actions
}

fn texts(painted: &[Painted]) -> Vec<&str> {
    painted.iter().map(|item| item.text.as_str()).collect()
}

fn center_of(painted: &[Painted], text: &str) -> egui::Pos2 {
    painted
        .iter()
        .find(|item| item.text == text)
        .unwrap_or_else(|| panic!("{text:?} not drawn: {:?}", texts(painted)))
        .rect
        .center()
}

fn plate(pod: AdPodLabel, offer: AdSkipOffer, progress: AdActionProgress) -> AdPlate {
    AdPlate {
        pod,
        offer,
        progress,
        locks_transport: true,
    }
}

#[test]
fn the_countdown_plate_reads_ad_n_of_m_and_skip_in_seconds_and_has_no_button() {
    let egui_ctx = egui::Context::default();
    let plate = plate(
        AdPodLabel::Known { index: 1, count: 2 },
        AdSkipOffer::Countdown { seconds: 5 },
        AdActionProgress::Idle,
    );
    let painted = settle(&egui_ctx, &plate);
    let shown = texts(&painted);
    assert!(shown.contains(&"Ad 1 of 2"), "{shown:?}");
    assert!(shown.contains(&"Skip in 5"), "{shown:?}");
    assert!(!shown.contains(&SKIP_BUTTON_LABEL));
    // Клик по тексту плашки без кнопки ничего не делает.
    let actions = click(&egui_ctx, &plate, center_of(&painted, "Skip in 5"));
    assert!(actions.is_empty(), "{actions:?}");
}

#[test]
fn without_a_counter_the_plate_says_just_ad() {
    let egui_ctx = egui::Context::default();
    let painted = settle(
        &egui_ctx,
        &plate(
            AdPodLabel::Unknown,
            AdSkipOffer::Nothing,
            AdActionProgress::Idle,
        ),
    );
    assert_eq!(texts(&painted), ["Ad"]);
}

#[test]
fn the_plate_sits_in_the_bottom_right_corner() {
    let egui_ctx = egui::Context::default();
    let painted = settle(
        &egui_ctx,
        &plate(
            AdPodLabel::Unknown,
            AdSkipOffer::Skip,
            AdActionProgress::Idle,
        ),
    );
    let position = center_of(&painted, SKIP_BUTTON_LABEL);
    assert!(
        position.x > SCREEN_SIZE.x * 0.75 && position.y > SCREEN_SIZE.y * 0.75,
        "{position:?}"
    );
}

#[test]
fn clicking_skip_gives_the_skip_action() {
    let egui_ctx = egui::Context::default();
    let plate = plate(
        AdPodLabel::Known { index: 2, count: 2 },
        AdSkipOffer::Skip,
        AdActionProgress::Idle,
    );
    let painted = settle(&egui_ctx, &plate);
    assert!(texts(&painted).contains(&SKIP_BUTTON_LABEL));
    let actions = click(&egui_ctx, &plate, center_of(&painted, SKIP_BUTTON_LABEL));
    assert_eq!(actions, [NotificationAction::AdPlate(AdPlateAction::Skip)]);
}

#[test]
fn on_page_plate_shows_a_hint_without_a_button_and_a_click_gives_no_action() {
    let egui_ctx = egui::Context::default();
    let plate = plate(
        AdPodLabel::Unknown,
        AdSkipOffer::OnPage,
        AdActionProgress::Idle,
    );
    let painted = settle(&egui_ctx, &plate);
    let shown = texts(&painted);
    assert_eq!(shown, ["Ad", "·", SKIP_ON_PAGE_HINT], "{shown:?}");
    assert_eq!(SKIP_ON_PAGE_HINT, "Skip on the page");
    assert!(!shown.contains(&SKIP_BUTTON_LABEL), "{shown:?}");
    // Подсказка — текст: клик по ней ничего не отдаёт.
    let actions = click(&egui_ctx, &plate, center_of(&painted, SKIP_ON_PAGE_HINT));
    assert!(actions.is_empty(), "{actions:?}");
}

#[test]
fn a_busy_button_gives_no_action() {
    let egui_ctx = egui::Context::default();
    let plate = plate(
        AdPodLabel::Unknown,
        AdSkipOffer::Skip,
        AdActionProgress::InFlight,
    );
    let painted = settle(&egui_ctx, &plate);
    assert!(
        texts(&painted).contains(&SKIP_BUTTON_LABEL),
        "still drawn, just disabled"
    );
    let actions = click(&egui_ctx, &plate, center_of(&painted, SKIP_BUTTON_LABEL));
    assert!(actions.is_empty(), "{actions:?}");
}

#[test]
fn toasts_get_the_area_above_the_plate() {
    let area = egui::Rect::from_min_size(egui::Pos2::ZERO, SCREEN_SIZE);
    let above = area_above_plate(area);
    assert!(above.max.y < area.max.y);
    assert_eq!(above.min, area.min);
}
