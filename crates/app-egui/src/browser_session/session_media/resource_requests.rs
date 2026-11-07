//! Просьбы ресурсов HLS странице (`fetch_resource`, протокол 1.1) и версия протокола
//! подключения — часть медиа-части главного цикла (10a1).
//!
//! Чем владеет модуль:
//! - правилом «когда забирать просьбы у очереди ресурсов»: только когда страница
//!   подключена и говорит на 1.1. До `Attached` просьбы остаются в очереди (срок
//!   молчания страницы у них ещё не идёт: он начинается с `take_requests`);
//! - правилом «отказ очереди к странице — не тихая потеря»: переполнение, нечего
//!   записать или старая версия — `error!` и закрытие сеанса кодом 4003, как у
//!   `fetch_range` файла (иначе движок ждал бы ресурса, о котором страница не знает);
//! - итоговой строкой журнала (только числа).

use browser_bridge_protocol::{ProtocolVersion, SessionCloseReason};
use browser_bridge_session::SessionRef;
use pushed_media_source::ResourceRequest;
use tracing::{debug, error, info};

use super::SessionMedia;
use crate::browser_session::outbound::{ResourceQueueOutcome, ResourceRequestPermission};

impl SessionMedia {
    /// Сеанс подключён (в том числе снова после обрыва) с этой общей версией протокола.
    /// Чужой экземпляр не трогает версию текущего.
    pub(crate) fn on_session_attached(&mut self, session: &SessionRef, version: ProtocolVersion) {
        if self.is_current(session) {
            self.outbound.note_agreed_protocol(version);
            // Окно эфира — заново: прежнее соединение могло его потерять (`live_hls`).
            self.outbound.forget_live_window_on_attach();
        } else {
            debug!("Attach of a session instance that is no longer current ignored");
        }
    }

    /// Просьбы ресурсов HLS → странице, сразу по пробуждению: у ждущего поколения
    /// (открытие качает корень и варианты) и у загруженного (сегменты, live-корень,
    /// перемотка).
    pub(super) fn send_resource_requests(&mut self) {
        match self.outbound.resource_request_permission() {
            ResourceRequestPermission::Allowed => {}
            // Страница ещё не подключилась: просьбы ждут в очереди ресурсов.
            ResourceRequestPermission::NotAttachedYet => return,
            ResourceRequestPermission::ProtocolTooOld => {
                // Страховка за сторожем поколений: просьбы есть, а слать их некому.
                if self.has_resource_requester() {
                    self.refuse_resource_requests_of_an_old_page();
                }
                return;
            }
        }
        let pending_requests = self
            .pending
            .as_ref()
            .map(|pending| pending.feed.take_resource_requests())
            .unwrap_or_default();
        let loaded_requests = self
            .loaded
            .as_ref()
            .map(|loaded| loaded.feed.take_resource_requests())
            .unwrap_or_default();
        let mut all_requests: Vec<ResourceRequest> = pending_requests
            .into_iter()
            .chain(loaded_requests)
            .collect();
        // Номера сквозные на сеанс: порядок номеров — порядок выдачи.
        all_requests.sort_by_key(|request| request.id);
        for request in all_requests {
            self.queue_resource_request(request);
        }
    }

    /// Одна просьба — в очередь к странице; отказ очереди закрывает сеанс.
    fn queue_resource_request(&mut self, request: ResourceRequest) {
        let outcome = self.outbound.request_resource(request);
        if outcome == ResourceQueueOutcome::Queued {
            return;
        }
        error!(
            ?outcome,
            "A resource request could not be queued for the page; closing the session"
        );
        if let Some(session) = self.current_session.clone() {
            self.close(&session, SessionCloseReason::ProtocolViolation);
        }
    }

    /// Открывается ли HLS-поколение. Загруженного HLS без открытия не бывает, а
    /// открытие идёт после `Attached`, поэтому страховке хватает ждущего поколения.
    fn has_resource_requester(&self) -> bool {
        self.pending
            .as_ref()
            .is_some_and(|pending| pending.feed.is_hls())
    }

    /// HLS у страницы ниже 1.1 — дефект договора: ждущее поколение выбрасывается (его
    /// открыватель отменяется), сеанс закрывается — ровно один раз.
    fn refuse_resource_requests_of_an_old_page(&mut self) {
        error!(
            "An HLS generation runs for a page whose protocol has no fetch_resource; closing the session"
        );
        self.pending = None;
        if let Some(session) = self.current_session.clone() {
            self.close(&session, SessionCloseReason::ProtocolViolation);
        }
    }

    /// Итог просьб ресурсов за жизнь процесса (только числа).
    pub(super) fn log_resource_request_totals(&self) {
        let resources = self.outbound.resource_counters();
        info!(
            resource_requests_sent = resources.sent,
            resource_bytes_limit = resources.bytes_limit_requested,
            resource_requests_refused = resources.refused_queue_full,
            resource_requests_unencodable = resources.unencodable,
            resource_requests_old_protocol = resources.refused_old_protocol,
            "Browser HLS resource requests"
        );
    }
}
