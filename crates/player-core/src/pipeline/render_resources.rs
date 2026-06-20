use super::*;

impl PlaybackPipeline {
    /// Регистрирует render lease только для актуального поколения renderer resources.
    pub(crate) fn try_register_render_lease(
        &mut self,
        render_generation: u64,
        resource_handle: video_core::FrameResourceHandle,
    ) -> bool {
        if render_generation != self.render_generation {
            return false;
        }

        let lease_key = (render_generation, resource_handle.0);
        let lease_count = self.leased_video_textures.entry(lease_key).or_insert(0);
        *lease_count = lease_count.saturating_add(1);
        true
    }

    /// Снимает один render lease и возвращает точный accounting outcome.
    pub(crate) fn release_render_lease_accounting(
        &mut self,
        render_generation: u64,
        resource_handle: video_core::FrameResourceHandle,
    ) -> RenderLeaseReleaseEffect {
        let lease_key = (render_generation, resource_handle.0);

        let Some(lease_count) = self.leased_video_textures.get_mut(&lease_key) else {
            return RenderLeaseReleaseEffect::UnknownLease;
        };

        if *lease_count > 1 {
            *lease_count -= 1;
            return RenderLeaseReleaseEffect::LeaseStillActive;
        }

        self.leased_video_textures.remove(&lease_key);
        if self.deferred_video_texture_releases.remove(&lease_key) {
            self.rendered_video_texture_release_providers
                .remove(&lease_key);
            RenderLeaseReleaseEffect::DeferredTextureReady
        } else {
            RenderLeaseReleaseEffect::ReleasedWithoutDeferredTexture
        }
    }

    /// Запоминает release provider кадра, который renderer уже использовал и отпустил.
    pub(crate) fn remember_rendered_texture_release_provider(
        &mut self,
        render_generation: u64,
        resource_handle: video_core::FrameResourceHandle,
        resource_provider: &PresentFrameResourceProviderHandle,
    ) {
        if render_generation != self.render_generation {
            return;
        }

        let lease_key = (render_generation, resource_handle.0);
        if self.leased_video_textures.contains_key(&lease_key)
            || self.deferred_video_texture_releases.contains(&lease_key)
        {
            return;
        }

        self.rendered_video_texture_release_providers
            .insert(lease_key, resource_provider.clone());
    }

    /// Помечает texture handle текущего поколения как deferred, если renderer держит lease.
    pub(crate) fn request_video_texture_release(
        &mut self,
        resource_handle: video_core::FrameResourceHandle,
    ) -> VideoTextureReleaseEffect {
        let lease_key = (self.render_generation, resource_handle.0);
        if self.leased_video_textures.contains_key(&lease_key) {
            self.deferred_video_texture_releases.insert(lease_key);
            return VideoTextureReleaseEffect::DeferredUntilRenderLeaseDrop;
        }

        if let Some(resource_provider) = self
            .rendered_video_texture_release_providers
            .remove(&lease_key)
        {
            return VideoTextureReleaseEffect::ReleaseViaRenderProvider(resource_provider);
        }

        VideoTextureReleaseEffect::ReleaseNow
    }

    /// Возвращает количество texture handles, удерживаемых render leases.
    #[must_use]
    pub(crate) fn active_render_lease_count(&self) -> usize {
        self.leased_video_textures.len()
    }

    /// Проверяет, отложен ли release конкретного texture handle текущего поколения.
    #[cfg(test)]
    #[must_use]
    pub(crate) fn has_deferred_video_texture_release(
        &self,
        resource_handle: video_core::FrameResourceHandle,
    ) -> bool {
        self.deferred_video_texture_releases
            .contains(&(self.render_generation, resource_handle.0))
    }

    /// Возвращает количество отложенных texture releases.
    #[must_use]
    pub(crate) fn deferred_render_release_count(&self) -> usize {
        self.deferred_video_texture_releases.len()
    }
}
