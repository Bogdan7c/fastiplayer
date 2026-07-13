use std::collections::hash_map::Entry;

use crate::PlayerVideoDecoderThreadHandle;

use super::PlaybackPipeline;

impl PlaybackPipeline {
    /// Передаёт old decoder bounded deferred-owner-у, только если его lease ещё жив.
    pub(crate) fn retain_retired_video_decoder_for_outstanding_leases(
        &mut self,
        render_generation: u64,
        retired_decoder: Option<Box<PlayerVideoDecoderThreadHandle>>,
    ) {
        let Some(retired_decoder) = retired_decoder else {
            return;
        };
        if !self.has_render_leases_for_generation(render_generation) {
            return;
        }

        match self.retired_video_decoders.entry(render_generation) {
            Entry::Vacant(entry) => {
                entry.insert(retired_decoder);
            }
            Entry::Occupied(_) => {
                tracing::error!(
                    render_generation,
                    "duplicate retired video decoder generation нарушает ownership invariant"
                );
            }
        }
    }

    /// Возвращает deferred old-generation frame decoder-у, который его создал.
    pub(crate) fn release_retired_video_frame(
        &mut self,
        render_generation: u64,
        resource_handle: video_core::FrameResourceHandle,
    ) {
        if let Some(retired_decoder) = self.retired_video_decoders.get(&render_generation) {
            retired_decoder.release_frame(resource_handle);
        } else {
            tracing::error!(
                render_generation,
                resource_handle = resource_handle.0,
                "deferred old-generation frame потерял decoder owner-а"
            );
        }
        self.release_retired_video_decoder_if_idle(render_generation);
    }

    /// Завершает old decoder ownership после provider-owned release последнего lease-а.
    pub(crate) fn release_retired_video_decoder_if_idle(&mut self, render_generation: u64) {
        if !self.has_render_leases_for_generation(render_generation) {
            self.retired_video_decoders.remove(&render_generation);
        }
    }

    /// Проверяет outstanding lease-ы exact generation без раскрытия accounting storage.
    fn has_render_leases_for_generation(&self, render_generation: u64) -> bool {
        self.leased_video_textures
            .keys()
            .any(|(lease_generation, _)| *lease_generation == render_generation)
    }
}
