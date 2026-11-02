//! Предложение «подключить браузеры» (решение 159): toast без срока с двумя кнопками.
//!
//! Центр уведомлений только показывает: кто и когда предлагает, решает владелец
//! `browser_integration`. Вид [`ToastKind::BrowserOffer`] не исчезает сам; кнопки
//! («Set up…», «Not now») и × возвращают действия UI, которые маршрутизирует `AppState`.

use std::time::Instant;

use super::{NotificationCenter, NotificationId, ToastKind};

/// Текст предложения.
pub(crate) const BROWSER_OFFER_MESSAGE: &str =
    "Fastiplayer can play videos from your browser. Connect your browsers?";

impl NotificationCenter {
    /// Показывает предложение подключить браузеры (висит до ответа или ×).
    pub(crate) fn notify_browser_connection_offer(&mut self, now: Instant) -> NotificationId {
        self.push_toast(ToastKind::BrowserOffer, BROWSER_OFFER_MESSAGE.into(), now)
    }
}
