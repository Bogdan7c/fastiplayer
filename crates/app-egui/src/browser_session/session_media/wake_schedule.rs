//! Когда главному циклу проснуться без внешнего повода (сроки медиа-части).
//!
//! Вынесено из `session_media.rs` (предел размера модуля, сессия 11b1) без изменения
//! правил: владелец — по-прежнему [`SessionMedia`], здесь только сводка его сроков.

use std::time::Instant;

use super::SessionMedia;
use crate::browser_session::outbound::PENDING_RETRY_INTERVAL;

impl SessionMedia {
    /// Когда проснуться без внешнего повода.
    ///
    /// Сводка журнала плеера (раз в `BROWSER_PLAYBACK_SUMMARY_INTERVAL`) пишется во
    /// время `pump`, отдельного срока у неё нет: пока поколение загружено, цикл
    /// просыпается не реже тика часов (`CLOCK_INTERVAL` — 250 мс, много чаще
    /// сводки), а без поколения плееру нечего сводить — счётчики стоят, и будить
    /// цикл ради одинаковых строк незачем (без поколения и без повторов — `None`,
    /// цикл спит до пробуждения, холостого кручения нет). Открывающемуся поколению
    /// срок часов нужен отдельно: подсказка повтора открытия может быть реже тика.
    pub(super) fn next_wake_at(&self, now: Instant) -> Option<Instant> {
        let open_retry = self
            .pending
            .as_ref()
            .map(|pending| pending.retry_at.unwrap_or(now));
        // Часы идут странице; пока доигрываем, её нет, и цикл будит `playback_drain`.
        let clock = (self.page_hears_clock() && self.page_clock_runs())
            .then(|| self.outbound.next_clock_at().unwrap_or(now));
        let outbound_retry = self
            .outbound
            .has_pending()
            .then(|| now + PENDING_RETRY_INTERVAL);
        // Кнопка плашки оживает по сроку и без событий страницы.
        let ad_button = self.ad.in_flight_expires_at(now);
        // Запрет перемотки снимается по сроку и без слова страницы (предохранитель 206).
        let ad_lock = self.ad.transport_lock_expires_at(now);
        // Срок ответа на просьбу вернуть страницу (11b1).
        let page_reopen = self.page_reopen.deadline();
        [
            open_retry,
            clock,
            outbound_retry,
            ad_button,
            ad_lock,
            page_reopen,
        ]
        .into_iter()
        .flatten()
        .min()
    }
}
