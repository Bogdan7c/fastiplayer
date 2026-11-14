//! Наблюдатель ресурсов одного открытого сегмента живого эфира.
//!
//! Источник сегментов отдаёт ошибки наружу строкой без типа, поэтому демуксер узнаёт
//! «что именно случилось» только через этого наблюдателя:
//! - протухание ссылки (401/403/404/410) — сигнал циклу обновления заменить ссылку;
//! - временный сетевой сбой (16d3, решение 334) — повод подождать сеть, а не падать.
//!
//! Вынесен из `demux.rs` в 16d3 вместе с новой отметкой о сетевом сбое.

use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};

use source_core::HttpRetryAfter;
use web_media_transport_api::SourceGeneration;

use super::refresh::{HlsLiveEndpointExpirySignal, HlsLiveRefreshControl};
use crate::HlsEndpointRefreshReason;
use crate::source::{HlsRefreshableResourceKind, HlsResourceExpiryObserver};

pub(super) struct HlsLiveSegmentExpiryObserver {
    generation: SourceGeneration,
    control: Arc<HlsLiveRefreshControl>,
    observed: AtomicBool,
    /// Последний временный сетевой сбой и подсказка сервера, сколько подождать.
    network_failure: Mutex<Option<HttpRetryAfter>>,
}

impl HlsLiveSegmentExpiryObserver {
    pub(super) fn new(
        generation: SourceGeneration,
        control: Arc<HlsLiveRefreshControl>,
    ) -> Arc<Self> {
        Arc::new(Self {
            generation,
            control,
            observed: AtomicBool::new(false),
            network_failure: Mutex::new(None),
        })
    }

    pub(super) fn take_observed(&self) -> bool {
        self.observed.swap(false, Ordering::SeqCst)
    }

    /// Снимает отметку о сетевом сбое; `Some` — последняя ошибка ресурса была сетевой.
    pub(super) fn take_network_failure(&self) -> Option<HttpRetryAfter> {
        self.network_failure
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .take()
    }

    pub(super) const fn generation(&self) -> SourceGeneration {
        self.generation
    }
}

impl HlsResourceExpiryObserver for HlsLiveSegmentExpiryObserver {
    fn observe_refreshable_expiry(
        &self,
        reason: HlsEndpointRefreshReason,
        resource_kind: HlsRefreshableResourceKind,
    ) {
        self.observed.store(true, Ordering::SeqCst);
        self.control.signal_expiry(HlsLiveEndpointExpirySignal {
            generation: self.generation,
            reason,
            resource_kind,
        });
    }

    fn observe_transient_network_failure(&self, retry_after: HttpRetryAfter) {
        *self
            .network_failure
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner) = Some(retry_after);
    }
}
