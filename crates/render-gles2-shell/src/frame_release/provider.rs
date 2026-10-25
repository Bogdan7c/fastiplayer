//! Обёртка провайдера ресурсов кадра: lookup-ы как есть, освобождение — через привязку.

use video_backend_api::{
    PresentFrameResourceDescriptorLookup, PresentFrameResourceIdentityLookup,
    PresentFrameResourceProvider, PresentFrameResourceProviderHandle,
    PresentFrameResourceProviderLookup, VideoBackendLifetimeGuard,
};
use video_core::FrameResourceHandle;

use super::binding::Gles2FrameReleaseBinding;

/// Провайдер для render leases плеера: освобождение ждёт fence кадра показа.
pub(super) struct Gles2FenceReleaseProvider {
    /// Настоящий провайдер декодера: владелец ресурса и фактического освобождения.
    inner_provider: PresentFrameResourceProviderHandle,
    /// Правило «когда можно вернуть кадр».
    binding: Gles2FrameReleaseBinding,
    /// Декодер живёт, пока живы записи, переживающие handle плеера.
    backend_lifetime: VideoBackendLifetimeGuard,
}

impl Gles2FenceReleaseProvider {
    pub(super) fn new(
        inner_provider: PresentFrameResourceProviderHandle,
        binding: Gles2FrameReleaseBinding,
        backend_lifetime: VideoBackendLifetimeGuard,
    ) -> Self {
        Self {
            inner_provider,
            binding,
            backend_lifetime,
        }
    }
}

impl PresentFrameResourceProvider for Gles2FenceReleaseProvider {
    fn resource_lookup(&self, handle: FrameResourceHandle) -> PresentFrameResourceProviderLookup {
        self.inner_provider.resource_lookup(handle)
    }

    fn try_resource_lookup(
        &self,
        handle: FrameResourceHandle,
    ) -> PresentFrameResourceProviderLookup {
        self.inner_provider.try_resource_lookup(handle)
    }

    fn resource_descriptor_lookup(
        &self,
        handle: FrameResourceHandle,
    ) -> PresentFrameResourceDescriptorLookup {
        self.inner_provider.resource_descriptor_lookup(handle)
    }

    fn try_resource_descriptor_lookup(
        &self,
        handle: FrameResourceHandle,
    ) -> PresentFrameResourceDescriptorLookup {
        self.inner_provider.try_resource_descriptor_lookup(handle)
    }

    /// Проброс обязателен: у трейта дефолт `Unsupported`, и забытый проброс молча выключил бы
    /// кэш импортов рендера (тот же класс дыры, что в wgpu-обёртке).
    fn try_resource_identity_lookup(
        &self,
        handle: FrameResourceHandle,
    ) -> PresentFrameResourceIdentityLookup {
        self.inner_provider.try_resource_identity_lookup(handle)
    }

    fn release_frame(&self, handle: FrameResourceHandle) {
        self.binding.schedule_release(
            self.inner_provider.clone(),
            handle,
            self.backend_lifetime.clone(),
        );
    }
}
