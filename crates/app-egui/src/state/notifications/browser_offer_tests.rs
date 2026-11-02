//! Предложение подключить браузеры: настоящий egui-кадр, настоящие клики по кнопкам.

use std::time::{Duration, Instant};

use super::render_tests::{click, settle};
use super::*;
use crate::ui::notifications::{
    BROWSER_OFFER_NOT_NOW_LABEL, BROWSER_OFFER_SET_UP_LABEL, NotificationAction,
};

#[test]
fn offer_toast_is_painted_with_both_buttons_and_never_expires() {
    let now = Instant::now();
    let mut center = NotificationCenter::default();
    let offer_id = center.notify_browser_connection_offer(now);
    let egui_ctx = egui::Context::default();

    // Через сутки toast всё ещё на экране: срока у предложения нет.
    let much_later = now + Duration::from_secs(24 * 60 * 60);
    let frame = center.frame(OpenProgress::Idle, UiMotion::Reduced, much_later);
    assert_eq!(frame.toasts[0].id, offer_id);
    assert_eq!(frame.toasts[0].kind, ToastKind::BrowserOffer);
    let rendered = settle(&egui_ctx, &frame);
    assert!(rendered.shows(BROWSER_OFFER_MESSAGE));
    assert!(rendered.shows(BROWSER_OFFER_SET_UP_LABEL));
    assert!(rendered.shows(BROWSER_OFFER_NOT_NOW_LABEL));
    assert!(rendered.shows("×"));
    assert!(center.next_wake_deadline().is_none());
}

#[test]
fn set_up_button_returns_the_set_up_action_for_the_offer() {
    let now = Instant::now();
    let mut center = NotificationCenter::default();
    let offer_id = center.notify_browser_connection_offer(now);
    let egui_ctx = egui::Context::default();
    let frame = center.frame(OpenProgress::Idle, UiMotion::Reduced, now);
    let rendered = settle(&egui_ctx, &frame);

    let actions = click(
        &egui_ctx,
        &frame,
        rendered.center_of(BROWSER_OFFER_SET_UP_LABEL),
    );

    assert_eq!(actions, [NotificationAction::BrowserOfferSetUp(offer_id)]);
}

#[test]
fn not_now_button_and_cross_return_distinct_actions() {
    let now = Instant::now();
    let mut center = NotificationCenter::default();
    let offer_id = center.notify_browser_connection_offer(now);
    let egui_ctx = egui::Context::default();
    let frame = center.frame(OpenProgress::Idle, UiMotion::Reduced, now);
    let rendered = settle(&egui_ctx, &frame);

    let not_now = click(
        &egui_ctx,
        &frame,
        rendered.center_of(BROWSER_OFFER_NOT_NOW_LABEL),
    );
    let cross = click(&egui_ctx, &frame, rendered.center_of("×"));

    assert_eq!(not_now, [NotificationAction::BrowserOfferNotNow(offer_id)]);
    assert_eq!(cross, [NotificationAction::Dismiss(offer_id)]);
}
