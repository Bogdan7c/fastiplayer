//! Приём запросов от следующих запусков плеера (сессия 13).
//!
//! Второй запуск («Открыть с помощью» при открытом плеере) пересылает свои файлы
//! сюда через `desktop_integration` и ждёт подтверждения. Этот модуль — владелец
//! приёмной стороны в приложении:
//! - [`InstanceForwardingInbox`] запускает службу приёма, держит ограниченный ящик
//!   доставок и очередь ещё не исполненных запросов;
//! - [`external_open_request_for`] превращает пересланные URI в тот же запрос
//!   внешнего открытия, что и бросок файлов на видео (сессия 12): все правила
//!   (один файл как кнопка Open, несколько — новая очередь с подтверждением,
//!   плейлисты, ссылки, «Файл ещё открывается») берутся оттуда без копий.
//!
//! Подтверждение отправителю даётся при разборе ящика на UI-потоке: это и есть
//! доказательство, что первый экземпляр жив. Исполнение может подождать, пока
//! появятся окно и состояние приложения (запрос пришёл во время запуска).
//!
//! Поднятие окна — `window_activation` (единственный winit-aware файл модуля).

pub(crate) mod window_activation;

use std::collections::VecDeque;
use std::sync::Arc;
use std::sync::mpsc::{Receiver, SyncSender, TrySendError, sync_channel};
use std::time::Duration;

use desktop_integration::{
    ForwardedInstanceAction, ForwardedInstanceRequest, ForwardedRequestDelivery,
    ForwardedRequestSink, ForwardedRequestSinkError, InstanceForwardingService,
    InstanceForwardingServiceConfig, InstanceForwardingServiceError,
};
use tracing::{info, warn};

use crate::app_wake::AppWakePort;
use crate::external_open::DropTarget;
use crate::external_open::request::ExternalOpenRequest;
use crate::external_open::uri::item_from_uri;

/// Сколько служба ждёт, пока UI-поток заберёт запрос. Меньше, чем таймаут вызова
/// у отправителя (`app_instance::forwarding`), чтобы тот получил ответ
/// «не отвечает», а не обрыв по своему таймауту.
const ACKNOWLEDGEMENT_TIMEOUT: Duration = Duration::from_secs(5);

/// Ёмкость ящика доставок. Каждая доставка ждёт подтверждения, поэтому в ящике
/// одновременно лишь запросы, отправленные за время одного кадра UI. Переполнение
/// значит, что UI-поток не разбирает ящик, и отправитель получит «не отвечает».
const DELIVERY_MAILBOX_CAPACITY: usize = 8;

/// Сколько принятых, но ещё не исполненных запросов хранится, пока нет окна.
/// При переполнении отбрасывается самый старый: пользователь ждёт последний.
const PENDING_REQUEST_LIMIT: usize = 8;

/// Неблокирующий приёмник для потока транспорта: кладёт в ящик и будит UI-поток.
struct InboxSink {
    deliveries: SyncSender<ForwardedRequestDelivery>,
    wake_port: AppWakePort,
}

impl ForwardedRequestSink for InboxSink {
    fn deliver(&self, delivery: ForwardedRequestDelivery) -> Result<(), ForwardedRequestSinkError> {
        // Payload публикуется до пробуждения: это контракт `AppWakePort`.
        self.deliveries
            .try_send(delivery)
            .map_err(|send_error| match send_error {
                TrySendError::Full(_) => ForwardedRequestSinkError::Full,
                TrySendError::Disconnected(_) => ForwardedRequestSinkError::Closed,
            })?;
        self.wake_port.request_wake();
        Ok(())
    }
}

/// Владелец приёмной стороны: служба, ящик доставок и очередь исполнения.
pub(crate) struct InstanceForwardingInbox {
    /// Служба приёма; `None`, если не запустилась или уже остановлена.
    service: Option<InstanceForwardingService>,
    /// Ящик доставок; `None` после остановки (новые доставки получат `Closed`).
    deliveries: Option<Receiver<ForwardedRequestDelivery>>,
    /// Подтверждённые запросы в порядке прихода, ждущие окна и состояния.
    pending_requests: VecDeque<ForwardedInstanceRequest>,
    wake_port: AppWakePort,
}

impl InstanceForwardingInbox {
    /// Запускает службу приёма. Вызывать только под process lease.
    ///
    /// Если служба не запустилась (нет шины, имя занято), плеер работает дальше:
    /// второй запуск тогда сообщит пользователю, что передать файл не удалось.
    pub(crate) fn start(wake_port: AppWakePort) -> Self {
        Self::start_with(wake_port, |config, sink| {
            desktop_integration::start_instance_forwarding_service(config, sink)
        })
    }

    /// Запуск с подменяемым транспортом (тесты).
    pub(super) fn start_with(
        wake_port: AppWakePort,
        start_service: impl FnOnce(
            InstanceForwardingServiceConfig,
            Arc<dyn ForwardedRequestSink>,
        )
            -> Result<InstanceForwardingService, InstanceForwardingServiceError>,
    ) -> Self {
        let (sender, receiver) = sync_channel(DELIVERY_MAILBOX_CAPACITY);
        let sink = Arc::new(InboxSink {
            deliveries: sender,
            wake_port: wake_port.clone(),
        });
        let config = InstanceForwardingServiceConfig {
            acknowledgement_timeout: ACKNOWLEDGEMENT_TIMEOUT,
        };
        let service = match start_service(config, sink) {
            Ok(service) => Some(service),
            Err(error) => {
                warn!(%error, "Служба приёма файлов от второго запуска не запущена");
                None
            }
        };
        Self {
            service,
            deliveries: Some(receiver),
            pending_requests: VecDeque::new(),
            wake_port,
        }
    }

    /// Разбирает ящик на UI-потоке: подтверждает каждую доставку отправителю и
    /// ставит запрос в очередь исполнения. Возвращает число принятых запросов.
    pub(crate) fn drain_deliveries(&mut self) -> usize {
        // Сначала снимается флаг пробуждения, потом читается ящик: доставка,
        // пришедшая во время разбора, снова поднимет флаг и не потеряется.
        self.wake_port.clear_pending_for_drain();
        let Some(deliveries) = self.deliveries.as_ref() else {
            return 0;
        };
        let mut accepted = 0;
        while let Ok(delivery) = deliveries.try_recv() {
            let (request, acknowledgement) = delivery.into_parts();
            acknowledgement.acknowledge();
            if self.pending_requests.len() == PENDING_REQUEST_LIMIT {
                self.pending_requests.pop_front();
                warn!(
                    "Слишком много пересланных запросов до готовности окна: самый старый отброшен"
                );
            }
            self.pending_requests.push_back(request);
            accepted += 1;
        }
        if accepted > 0 {
            info!(accepted, "Приняты запросы от второго запуска");
        }
        accepted
    }

    /// Следующий подтверждённый запрос для исполнения (в порядке прихода).
    pub(crate) fn take_next_pending_request(&mut self) -> Option<ForwardedInstanceRequest> {
        self.pending_requests.pop_front()
    }

    /// Останавливает приём: имя на шине освобождается, ящик закрывается, ещё не
    /// разобранные доставки уничтожаются (их отправители получат «завершается»).
    /// Повторный вызов ничего не делает.
    pub(crate) fn shutdown(&mut self) {
        if let Some(service) = self.service.take() {
            service.shutdown();
        }
        self.deliveries = None;
        self.pending_requests.clear();
    }
}

/// Пересланное действие → запрос внешнего открытия на область видео.
///
/// `None` — открывать нечего (второй запуск без файлов): нужно только поднять окно.
/// Цель всегда «видео»: «Открыть с помощью» означает «открой это», как бросок
/// на видео, а не «добавь в конец очереди».
pub(crate) fn external_open_request_for(
    action: ForwardedInstanceAction,
) -> Option<ExternalOpenRequest> {
    match action {
        ForwardedInstanceAction::Activate => None,
        ForwardedInstanceAction::Open(uris) => Some(ExternalOpenRequest {
            target: DropTarget::Video,
            items: uris.iter().map(|uri| item_from_uri(uri.as_str())).collect(),
        }),
    }
}

#[cfg(test)]
mod tests;
