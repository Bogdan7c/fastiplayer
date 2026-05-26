use std::sync::Arc;
use std::time::Duration;

#[cfg(test)]
pub(crate) use video_core::DecodeBackpressureReason;
pub(crate) use video_core::{
    DecodePacket as PlayerDecodePacket, DecodeSendError, DecodeThreadError, DecoderResourceSnapshot,
};

/// Backwards-compatible public name для backend-neutral decoder-thread config.
pub type PlayerVideoDecoderThreadConfig = video_core::VideoDecoderThreadConfig;

/// Player-core specialization neutral decoder handle-а с renderer-neutral resource provider-ом.
pub(crate) type PlayerVideoDecoderThreadHandle =
    dyn video_core::VideoDecoderThreadHandle<ResourceProvider = PresentFrameResourceProviderHandle>;

/// Результат renderer-neutral lookup-а decoded resource-а без GPU handles.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PresentFrameResourceProviderLookup {
    /// Backend resource table доступен, opaque handle можно materialize в renderer layer-е.
    Ready {
        /// Сколько render thread ждал lock backend resource pool-а внутри provider-а.
        texture_pool_lock_wait: Duration,
    },

    /// Backend resource pool занят, render hot path должен выбрать fallback без ожидания.
    Busy {
        /// Сколько заняла non-blocking попытка получить lock.
        texture_pool_lock_wait: Duration,
    },

    /// Backend доступен, но resource для handle отсутствует.
    Missing {
        /// Сколько render thread ждал lock backend resource pool-а внутри provider-а.
        texture_pool_lock_wait: Duration,
    },

    /// Backend обнаружил poisoned/fatal state при lookup-е.
    Error {
        /// Сколько render thread ждал lock backend resource pool-а внутри provider-а.
        texture_pool_lock_wait: Duration,
    },
}

impl PresentFrameResourceProviderLookup {
    /// Возвращает lock wait sample без раскрытия конкретного outcome.
    #[must_use]
    pub const fn texture_pool_lock_wait(&self) -> Duration {
        match self {
            Self::Ready {
                texture_pool_lock_wait,
                ..
            }
            | Self::Busy {
                texture_pool_lock_wait,
            }
            | Self::Missing {
                texture_pool_lock_wait,
            }
            | Self::Error {
                texture_pool_lock_wait,
            } => *texture_pool_lock_wait,
        }
    }
}

/// Renderer-neutral provider для status lookup-а и renderer-owned release.
pub trait PresentFrameResourceProvider: Send + Sync {
    /// Получает status и lock diagnostics для frame handle без возврата GPU handles.
    fn resource_lookup(
        &self,
        handle: video_core::FrameTextureHandle,
    ) -> PresentFrameResourceProviderLookup;

    /// Пытается получить status без ожидания backend resource pool mutex-а.
    fn try_resource_lookup(
        &self,
        handle: video_core::FrameTextureHandle,
    ) -> PresentFrameResourceProviderLookup {
        self.resource_lookup(handle)
    }

    /// Освобождает renderer-owned frame после submitted GPU work или fallback release.
    fn release_frame(&self, handle: video_core::FrameTextureHandle);
}

/// Clone-able handle, который скрывает конкретный backend provider за trait boundary.
#[derive(Clone)]
pub struct PresentFrameResourceProviderHandle {
    /// Shared provider живёт столько же, сколько render leases, которые его держат.
    provider: Arc<dyn PresentFrameResourceProvider>,
}

impl PresentFrameResourceProviderHandle {
    /// Оборачивает concrete backend provider в renderer-neutral resource boundary handle.
    #[must_use]
    pub fn new(provider: impl PresentFrameResourceProvider + 'static) -> Self {
        Self {
            provider: Arc::new(provider),
        }
    }

    /// Получает resource status и lock diagnostics через backend provider.
    #[must_use]
    pub fn resource_lookup(
        &self,
        handle: video_core::FrameTextureHandle,
    ) -> PresentFrameResourceProviderLookup {
        self.provider.resource_lookup(handle)
    }

    /// Пытается получить resource status без ожидания backend resource pool mutex-а.
    #[must_use]
    pub fn try_resource_lookup(
        &self,
        handle: video_core::FrameTextureHandle,
    ) -> PresentFrameResourceProviderLookup {
        self.provider.try_resource_lookup(handle)
    }

    /// Освобождает frame через backend provider, который создал texture handle.
    pub fn release_frame(&self, handle: video_core::FrameTextureHandle) {
        self.provider.release_frame(handle);
    }
}
