use std::sync::Arc;
use std::time::Duration;

#[cfg(test)]
pub(crate) use video_core::DecodeBackpressureReason;
pub(crate) use video_core::{
    DecodePacket as PlayerDecodePacket, DecodeSendError, DecodeThreadError, DecoderResourceSnapshot,
};

/// Backwards-compatible public name для backend-neutral decoder-thread config.
pub type PlayerVideoDecoderThreadConfig = video_core::VideoDecoderThreadConfig;

/// Player-core specialization neutral decoder handle-а с WGPU render provider-ом.
pub(crate) type PlayerVideoDecoderThreadHandle =
    dyn video_core::VideoDecoderThreadHandle<TextureViewProvider = WgpuRenderTextureProviderHandle>;

/// WGPU-specific texture views, полученные render thread-ом по opaque frame handle.
pub struct WgpuRenderTextureViews {
    /// Texture view с luma/Y plane.
    pub y_view: wgpu::TextureView,

    /// Texture view с chroma/UV plane.
    pub uv_view: wgpu::TextureView,
}

/// Результат WGPU renderer-side lookup-а texture views без раскрытия backend pool-а.
pub enum WgpuRenderTextureViewLookup {
    /// Backend вернул валидные plane views для renderer-а.
    Ready {
        /// Texture views, которые можно передать WGPU render backend-у.
        views: WgpuRenderTextureViews,

        /// Сколько render thread ждал lock texture pool-а внутри backend provider-а.
        texture_pool_lock_wait: Duration,
    },

    /// Backend texture pool занят, render hot path должен выбрать fallback без ожидания.
    Busy {
        /// Сколько заняла non-blocking попытка получить lock.
        texture_pool_lock_wait: Duration,
    },

    /// Backend доступен, но texture views для handle отсутствуют.
    Missing {
        /// Сколько render thread ждал lock texture pool-а внутри backend provider-а.
        texture_pool_lock_wait: Duration,
    },

    /// Backend обнаружил poisoned/fatal state при lookup-е.
    Error {
        /// Сколько render thread ждал lock texture pool-а внутри backend provider-а.
        texture_pool_lock_wait: Duration,
    },
}

impl WgpuRenderTextureViewLookup {
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

/// WGPU-specific render-side provider для texture views и renderer-owned release.
pub trait WgpuRenderTextureProvider: Send + Sync {
    /// Получает WGPU views и lock diagnostics для frame handle на render thread.
    fn texture_view_lookup(
        &self,
        handle: video_core::FrameTextureHandle,
    ) -> WgpuRenderTextureViewLookup;

    /// Пытается получить WGPU views без ожидания backend texture pool mutex-а.
    fn try_texture_view_lookup(
        &self,
        handle: video_core::FrameTextureHandle,
    ) -> WgpuRenderTextureViewLookup {
        self.texture_view_lookup(handle)
    }

    /// Освобождает renderer-owned frame после submitted GPU work или fallback release.
    fn release_frame(&self, handle: video_core::FrameTextureHandle);
}

/// Clone-able handle, который скрывает конкретный backend provider за trait boundary.
#[derive(Clone)]
pub struct WgpuRenderTextureProviderHandle {
    /// Shared provider живёт столько же, сколько render leases, которые его держат.
    provider: Arc<dyn WgpuRenderTextureProvider>,
}

impl WgpuRenderTextureProviderHandle {
    /// Оборачивает concrete backend provider в WGPU render boundary handle.
    #[must_use]
    pub fn new(provider: impl WgpuRenderTextureProvider + 'static) -> Self {
        Self {
            provider: Arc::new(provider),
        }
    }

    /// Получает WGPU views и lock diagnostics через backend provider.
    #[must_use]
    pub fn texture_view_lookup(
        &self,
        handle: video_core::FrameTextureHandle,
    ) -> WgpuRenderTextureViewLookup {
        self.provider.texture_view_lookup(handle)
    }

    /// Пытается получить WGPU views без ожидания backend texture pool mutex-а.
    #[must_use]
    pub fn try_texture_view_lookup(
        &self,
        handle: video_core::FrameTextureHandle,
    ) -> WgpuRenderTextureViewLookup {
        self.provider.try_texture_view_lookup(handle)
    }

    /// Освобождает frame через backend provider, который создал texture handle.
    pub fn release_frame(&self, handle: video_core::FrameTextureHandle) {
        self.provider.release_frame(handle);
    }
}
