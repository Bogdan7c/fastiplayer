//! Шов «переводчик ↔ HLS-поколение»: тот же приём, что у файла (`translator::file_port`).
//!
//! Переводчик видит приёмную сторону очереди ресурсов через трейт [`HlsWriterPort`] и
//! на каждое HLS-поколение берёт новую очередь у [`HlsFactory`]. Настоящая фабрика —
//! [`BridgeHlsFactory`]: очередь `pushed-media-source` с политикой договора протокола
//! и открыватель native HLS поверх транспорта моста; тесты подставляют запись вызовов.

use std::num::{NonZeroU64, NonZeroUsize};
use std::sync::Arc;
use std::time::Duration;

use browser_bridge_protocol::limits::{MAX_CONCURRENT_RESOURCE_FETCHES, MAX_RESOURCE_FETCH_BYTES};
use fastiplayer_config::{AppConfig, NetworkConfig};
use pushed_media_source::{
    ExternalResourceHandle, RangeRequestId, RangeRequestIds, RangeRequestNotifier,
    ResourceFetchOutcome, ResourceFetchPolicy, ResourceFetchPolicyError, ResourceFetchRequests,
    ResourceFetchViolation, ResourceFetchWriter, ResourceFrameVerdict, ResourceRequestClaim,
    resource_fetch_queue,
};

use super::opener::{HlsGenerationOpener, HlsOpenSettings};
use super::transport::BridgeResourceTransport;

/// Приёмная сторона очереди ресурсов так, как её видит переводчик. Настоящая —
/// [`ResourceFetchWriter`].
pub(crate) trait HlsWriterPort: Send {
    /// Выдала ли номер эта очередь (рамка вида 2 не говорит, кому она).
    fn claim_of(&self, id: RangeRequestId) -> ResourceRequestClaim;
    /// Кусок тела запроса `id` с `offset` тела.
    ///
    /// # Errors
    /// Нарушение договора страницей (очередь мертва, сеанс закрывается 4003).
    fn accept_bytes(
        &mut self,
        id: RangeRequestId,
        offset: u64,
        payload: &[u8],
    ) -> Result<ResourceFrameVerdict, ResourceFetchViolation>;
    /// Итог запроса `id` (`resource_fetch_finished`).
    ///
    /// # Errors
    /// Нарушение договора страницей.
    fn finish(
        &mut self,
        id: RangeRequestId,
        outcome: ResourceFetchOutcome,
    ) -> Result<ResourceFrameVerdict, ResourceFetchViolation>;
    /// «Больше ничего не будет» (сеанс кончился, поколение заменено).
    fn seal(&mut self);
}

impl HlsWriterPort for ResourceFetchWriter {
    fn claim_of(&self, id: RangeRequestId) -> ResourceRequestClaim {
        Self::claim_of(self, id)
    }

    fn accept_bytes(
        &mut self,
        id: RangeRequestId,
        offset: u64,
        payload: &[u8],
    ) -> Result<ResourceFrameVerdict, ResourceFetchViolation> {
        Self::accept_bytes(self, id, offset, payload)
    }

    fn finish(
        &mut self,
        id: RangeRequestId,
        outcome: ResourceFetchOutcome,
    ) -> Result<ResourceFrameVerdict, ResourceFetchViolation> {
        Self::finish(self, id, outcome)
    }

    fn seal(&mut self) {
        Self::seal(self);
    }
}

/// Только что созданное HLS-поколение: писатель остаётся у переводчика, остальное
/// уходит главному циклу.
pub(crate) struct StartedHls {
    /// Приёмная сторона очереди.
    pub(crate) writer: Box<dyn HlsWriterPort>,
    /// Открыватель native HLS поколения.
    pub(crate) opener: HlsGenerationOpener,
    /// Запросы ресурсов, которые главный цикл отправляет странице (`fetch_resource`).
    pub(crate) requests: ResourceFetchRequests,
}

/// Откуда переводчик берёт новое HLS-поколение.
pub(crate) trait HlsFactory: Send {
    /// Создаёт поколение с корнем `root`. `ids` — выдаватель номеров запросов, общий на
    /// сеанс (тот же, что у файла): рамку чужого номера очередь узнаёт как «не наша».
    fn start_hls(&mut self, ids: RangeRequestIds, root: ExternalResourceHandle) -> StartedHls;
}

/// Политика очереди ресурсов моста: договор протокола 1.1 и срок молчания `[network]`.
///
/// - не больше [`MAX_CONCURRENT_RESOURCE_FETCHES`] запросов у страницы одновременно;
/// - страница может молчать по запросу не дольше `network.read_timeout_ms` — тот же
///   срок, что у чтения обычной ссылки (новых настроек нет);
/// - ресурс не больше [`MAX_RESOURCE_FETCH_BYTES`].
///
/// # Errors
/// Нулевой `read_timeout_ms` (очередь не может ждать ни мгновения).
pub(crate) fn bridge_resource_policy(
    network: &NetworkConfig,
) -> Result<ResourceFetchPolicy, ResourceFetchPolicyError> {
    // Обе константы протокола ненулевые; запасное значение недостижимо.
    let max_in_flight =
        NonZeroUsize::new(MAX_CONCURRENT_RESOURCE_FETCHES).unwrap_or(NonZeroUsize::MIN);
    let ceiling = NonZeroU64::new(MAX_RESOURCE_FETCH_BYTES).unwrap_or(NonZeroU64::MIN);
    ResourceFetchPolicy::new(
        max_in_flight,
        Duration::from_millis(network.read_timeout_ms),
        ceiling,
    )
}

/// Настоящая фабрика: очередь ресурсов + открыватель native HLS.
pub(crate) struct BridgeHlsFactory {
    /// Политика очереди каждого поколения.
    policy: ResourceFetchPolicy,
    /// Будильник главного цикла: новый запрос ресурса уходит странице сразу.
    notifier: Arc<dyn RangeRequestNotifier>,
    /// Настройки открытия (общие для всех поколений процесса).
    settings: Arc<HlsOpenSettings>,
}

impl BridgeHlsFactory {
    /// Фабрика по config процесса.
    ///
    /// # Errors
    /// Политика очереди из `[network]` недопустима ([`bridge_resource_policy`]).
    pub(crate) fn new(
        config: &AppConfig,
        notifier: Arc<dyn RangeRequestNotifier>,
    ) -> Result<Self, ResourceFetchPolicyError> {
        Ok(Self {
            policy: bridge_resource_policy(&config.network)?,
            notifier,
            settings: Arc::new(HlsOpenSettings::from_app_config(config)),
        })
    }
}

impl HlsFactory for BridgeHlsFactory {
    fn start_hls(&mut self, ids: RangeRequestIds, root: ExternalResourceHandle) -> StartedHls {
        let (writer, client, requests) =
            resource_fetch_queue(self.policy, ids, Arc::clone(&self.notifier));
        let opener = HlsGenerationOpener::new(
            BridgeResourceTransport::new(client),
            requests.clone(),
            root,
            Arc::clone(&self.settings),
        );
        StartedHls {
            writer: Box::new(writer),
            opener,
            requests,
        }
    }
}

/// Фабрика для тестов, которым HLS не нужен: настоящая очередь с будильником, который
/// никого не будит.
#[cfg(test)]
pub(crate) fn unused_hls_factory() -> Box<dyn HlsFactory> {
    struct SilentNotifier;
    impl RangeRequestNotifier for SilentNotifier {
        fn range_requested(&self) {}
    }
    Box::new(
        BridgeHlsFactory::new(&AppConfig::default(), Arc::new(SilentNotifier))
            .expect("default network settings give a valid resource policy"),
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Политика очереди — договор протокола и `[network]` пользователя, без новых настроек.
    #[test]
    fn the_resource_policy_follows_the_protocol_contract_and_the_read_timeout() {
        let mut network = AppConfig::default().network;
        network.read_timeout_ms = 7_500;
        let policy = bridge_resource_policy(&network).expect("valid policy");
        assert_eq!(
            policy.max_in_flight().get(),
            MAX_CONCURRENT_RESOURCE_FETCHES
        );
        assert_eq!(policy.idle_timeout(), Duration::from_millis(7_500));
        assert_eq!(policy.max_resource_bytes().get(), MAX_RESOURCE_FETCH_BYTES);

        network.read_timeout_ms = 0;
        assert_eq!(
            bridge_resource_policy(&network),
            Err(ResourceFetchPolicyError::ZeroIdleTimeout)
        );
    }
}
