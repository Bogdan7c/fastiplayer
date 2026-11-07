//! Сетевой маршрут native HLS: через какую «розетку» идёт вся сеть одного открытия.
//!
//! Один и тот же готовый native HLS (admission, каталог, VOD/live runtime, обновление
//! корня live) работает либо поверх обычного HTTP плеера, либо поверх внешнего
//! транспорта (мост в браузер: браузер качает каждый ресурс по номерку). Маршрут
//! выбирается один раз в `NativeHlsPreparationRequest` и протягивается во все места,
//! где строится `AdaptiveHttpContext`, поэтому «половина ресурсов мимо моста»
//! невозможна по построению.
//!
//! Маршрут же решает, есть ли у VOD восстановление адреса (endpoint recovery):
//! его ведёт приложение обычного плеера, а у внешнего маршрута такого ведущего нет.

use std::sync::Arc;

use source_core::SourceRuntimeConfig;
use web_media_adaptive::{
    AdaptiveHttpContext, AdaptiveResourceTransport, AdaptiveRetryPolicy, AdaptiveTransportError,
    AdaptiveTransportLimits,
};
use web_media_transport_api::TransportOpenRequest;

use crate::web_media_vod_recovery::VodEndpointRecoveryAttachment;

/// Через какую сеть native HLS качает плейлисты, сегменты, init и ключи AES.
#[derive(Clone, Debug)]
pub enum NativeHlsNetworkRoute {
    /// Обычный HTTP плеера (`HttpSourceSession` + cookie jar) — поведение обычного плеера.
    ///
    /// VOD получает восстановление адреса, которое ведёт приложение: при 401/403/404/410
    /// на ресурсе демуксер ждёт (`TemporarilyUnavailable`), пока приложение не обновит корень.
    PublicHttp,
    /// Внешний транспорт одного hop-а (мост в браузер), без обычной сети вовсе.
    ///
    /// Отличия от `PublicHttp`, о которых обязан знать вызывающий:
    /// - **восстановления адреса VOD нет**: вести его некому, поэтому ошибка ресурса
    ///   после повторов идёт честной ошибкой демуксера (плеер уходит в `Failed`), а не
    ///   вечным ожиданием. В итоге это видно как `VodEndpointRecoveryMode::NotAvailable`;
    /// - обновление корня live при истечении остаётся и идёт через этот же транспорт;
    /// - **откат на yt-dlp порт сам не выполняет**: `NativeHlsAttempt::RequiresExtractorFallback`
    ///   (например, 401/403 на корне или «не HLS») возвращается как есть, и вызывающий
    ///   (мост) обязан превратить его в честный отказ — у моста нет адреса для yt-dlp.
    External(ExternalHlsTransport),
}

/// Внешний транспорт, подключённый к native HLS (дешёвый `Clone`: общий `Arc`).
#[derive(Clone, Debug)]
pub struct ExternalHlsTransport {
    /// Одна «розетка» на всё открытие: admission, runtime и обновление корня live.
    transport: Arc<dyn AdaptiveResourceTransport>,
}

impl ExternalHlsTransport {
    /// Оборачивает транспорт, который исполняет ровно один hop без своей политики.
    #[must_use]
    pub fn new(transport: Arc<dyn AdaptiveResourceTransport>) -> Self {
        Self { transport }
    }
}

impl NativeHlsNetworkRoute {
    /// Собирает HTTP context над выбранной «розеткой»; политика над ней одинакова.
    ///
    /// Повторы, VOD-кэш, `RedirectPolicy`, наблюдатель истечения и лимиты задаёт
    /// вызывающий — маршрут меняет только то, кто физически исполняет hop.
    pub(super) fn open_adaptive_http_context(
        &self,
        transport_request: TransportOpenRequest,
        source_config: &SourceRuntimeConfig,
        limits: AdaptiveTransportLimits,
        retry: AdaptiveRetryPolicy,
    ) -> Result<AdaptiveHttpContext, AdaptiveTransportError> {
        match self {
            Self::PublicHttp => {
                AdaptiveHttpContext::new(transport_request, source_config, limits, retry)
            }
            Self::External(external) => AdaptiveHttpContext::with_transport(
                transport_request,
                Arc::clone(&external.transport),
                source_config,
                limits,
                retry,
            ),
        }
    }

    /// Решает, будет ли у VOD этого открытия восстановление адреса.
    ///
    /// Для `PublicHttp` создаётся свежий attachment (его наблюдатель подключается к
    /// HTTP context-у VOD), для внешнего маршрута восстановления нет явно.
    pub(super) fn vod_endpoint_recovery_mode(&self) -> VodEndpointRecoveryMode {
        match self {
            Self::PublicHttp => {
                VodEndpointRecoveryMode::AppDriven(VodEndpointRecoveryAttachment::new())
            }
            Self::External(_) => VodEndpointRecoveryMode::NotAvailable,
        }
    }
}

/// Есть ли у подготовленного VOD восстановление адреса и кто его ведёт.
///
/// Вместо `Option<VodEndpointRecoveryAttachment>`: «нет восстановления» — осознанное
/// решение маршрута, а не забытое поле, и его видно на месте использования.
#[derive(Debug)]
pub enum VodEndpointRecoveryMode {
    /// Восстановление ведёт приложение: наблюдатель истечения подключён к HTTP context-у,
    /// демуксер и seek port обёрнуты воротами attachment-а.
    AppDriven(VodEndpointRecoveryAttachment),
    /// Восстановления нет: наблюдатель не подключён, ошибка ресурса после повторов
    /// доходит до демуксера как обычная ошибка.
    NotAvailable,
}

impl VodEndpointRecoveryMode {
    /// Подключает наблюдатель истечения к запросу VOD, только если восстановление ведёт приложение.
    pub(super) fn attach_expiry_observer(
        &self,
        transport_request: TransportOpenRequest,
    ) -> TransportOpenRequest {
        match self {
            Self::AppDriven(attachment) => {
                transport_request.with_endpoint_expiry_observer(attachment.observer())
            }
            Self::NotAvailable => transport_request,
        }
    }

    /// Отдаёт attachment приложению (`PreparedWebMediaEnvelope`), если оно ведёт восстановление.
    #[must_use]
    pub fn into_app_attachment(self) -> Option<VodEndpointRecoveryAttachment> {
        match self {
            Self::AppDriven(attachment) => Some(attachment),
            Self::NotAvailable => None,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn public_http_route_keeps_app_driven_vod_recovery() {
        let mode = NativeHlsNetworkRoute::PublicHttp.vod_endpoint_recovery_mode();
        let attachment = mode
            .into_app_attachment()
            .expect("ordinary player keeps VOD endpoint recovery");
        // Свежий attachment ничего не ждёт до первого сигнала истечения.
        assert!(!attachment.is_recovery_pending());
    }

    #[test]
    fn external_route_has_no_vod_recovery_and_leaves_the_request_unobserved() {
        let transport = super::super::external_route_test_transport::InMemoryHlsTransport::new();
        let route = NativeHlsNetworkRoute::External(ExternalHlsTransport::new(transport));
        let mode = route.vod_endpoint_recovery_mode();
        assert!(matches!(mode, VodEndpointRecoveryMode::NotAvailable));

        let request = super::super::external_route_test_transport::vod_transport_request();
        let observed = mode.attach_expiry_observer(request);
        assert!(
            observed.endpoint_expiry_observer().is_none(),
            "nobody drives recovery on the external route, so no observer may be attached"
        );
        assert!(mode.into_app_attachment().is_none());
    }

    #[test]
    fn app_driven_mode_attaches_its_own_expiry_observer() {
        let mode = NativeHlsNetworkRoute::PublicHttp.vod_endpoint_recovery_mode();
        let request = super::super::external_route_test_transport::vod_transport_request();
        let observed = mode.attach_expiry_observer(request);
        assert!(observed.endpoint_expiry_observer().is_some());
    }
}
