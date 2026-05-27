use std::sync::Arc;

use video_backend_api::{
    PresentFrameResourceProvider, PresentFrameResourceProviderHandle,
    PresentFrameResourceProviderLookup, StartedVideoBackend, VideoBackendFactory,
};
use video_core::VideoDecoderThreadHandle;

use crate::{
    VideoDecodeThread, VideoDecodeThreadConfig, VideoTextureViewLookup, VideoTextureViewProvider,
};

/// Factory текущего production VA-API backend-а для app composition layer-а.
///
/// Тип живёт в concrete backend crate, а `player-core` получает только
/// `StartedVideoBackend` с playback-facing decoder handle.
pub struct VaapiWgpuVideoBackendFactory {
    /// WGPU instance для zero-copy import path.
    instance: wgpu::Instance,

    /// WGPU adapter для backend capability matching.
    adapter: wgpu::Adapter,

    /// WGPU device для texture allocation.
    device: wgpu::Device,

    /// WGPU queue для texture upload/release callbacks.
    queue: wgpu::Queue,

    /// Backend-neutral runtime limits, которые адаптируются к VA-API config при startup.
    decoder_thread_config: video_core::VideoDecoderThreadConfig,
}

impl VaapiWgpuVideoBackendFactory {
    /// Создаёт factory из GPU handles, которыми владеет shell/render layer.
    #[must_use]
    pub fn new(
        instance: &wgpu::Instance,
        adapter: &wgpu::Adapter,
        device: &wgpu::Device,
        queue: &wgpu::Queue,
    ) -> Self {
        Self::new_with_decoder_config(
            instance,
            adapter,
            device,
            queue,
            video_core::VideoDecoderThreadConfig::default(),
        )
    }

    /// Создаёт factory с явным decoder-thread config из validated app config.
    #[must_use]
    pub fn new_with_decoder_config(
        instance: &wgpu::Instance,
        adapter: &wgpu::Adapter,
        device: &wgpu::Device,
        queue: &wgpu::Queue,
        decoder_thread_config: impl Into<video_core::VideoDecoderThreadConfig>,
    ) -> Self {
        Self {
            instance: instance.clone(),
            adapter: adapter.clone(),
            device: device.clone(),
            queue: queue.clone(),
            decoder_thread_config: decoder_thread_config.into(),
        }
    }

    /// Стартует VA-API/WGPU backend и возвращает player/backend части отдельно.
    pub fn start_for_composition(&self) -> anyhow::Result<StartedVaapiWgpuVideoBackend> {
        let decoder_thread = VideoDecodeThread::new_with_config(
            Arc::new(self.device.clone()),
            Arc::new(self.queue.clone()),
            self.instance.clone(),
            self.adapter.clone(),
            VideoDecodeThreadConfig::from(self.decoder_thread_config),
        )?;
        let texture_view_provider = VideoDecodeThread::texture_view_provider(&decoder_thread);
        let player_backend = StartedVideoBackend::from_decoder_thread(
            VaapiVideoDecoderThreadHandle::new(decoder_thread),
        );

        Ok(StartedVaapiWgpuVideoBackend {
            player_backend,
            texture_view_provider,
        })
    }

    /// Compatibility helper для callers, которым не нужен WGPU materializer.
    pub fn start_video_backend(&self) -> anyhow::Result<StartedVideoBackend> {
        <Self as VideoBackendFactory>::start_video_backend(self)
    }
}

impl VideoBackendFactory for VaapiWgpuVideoBackendFactory {
    /// Стартует concrete VA-API backend через тот же startup path, что и app composition.
    fn start_video_backend(&self) -> anyhow::Result<StartedVideoBackend> {
        Ok(self.start_for_composition()?.into_player_backend())
    }
}

/// Старое public имя оставлено в concrete crate как compatibility alias.
#[deprecated(note = "use VaapiWgpuVideoBackendFactory; deprecated alias stays in video-vaapi")]
pub type WgpuVideoBackendFactory = VaapiWgpuVideoBackendFactory;

/// Запущенный VA-API/WGPU backend, разделённый на playback и render-facing части.
pub struct StartedVaapiWgpuVideoBackend {
    /// Playback-facing backend отдаётся `player-core`.
    player_backend: StartedVideoBackend,

    /// WGPU-facing provider остаётся в app/render composition layer-е.
    texture_view_provider: VideoTextureViewProvider,
}

impl StartedVaapiWgpuVideoBackend {
    /// Разделяет startup artifact без передачи WGPU materializer-а в `player-core`.
    #[must_use]
    pub fn into_parts(self) -> (StartedVideoBackend, VideoTextureViewProvider) {
        (self.player_backend, self.texture_view_provider)
    }

    /// Возвращает только playback-facing backend для compatibility helper-а.
    #[must_use]
    pub fn into_player_backend(self) -> StartedVideoBackend {
        self.player_backend
    }
}

/// Adapter вокруг concrete VA-API decoder thread для neutral `video-core` contract.
struct VaapiVideoDecoderThreadHandle {
    /// Concrete backend остаётся скрыт за neutral contract boundary.
    decoder_thread: VideoDecodeThread,
}

impl VaapiVideoDecoderThreadHandle {
    /// Оборачивает запущенный VA-API thread без изменения lifecycle ownership.
    fn new(decoder_thread: VideoDecodeThread) -> Self {
        Self { decoder_thread }
    }
}

impl PresentFrameResourceProvider for VideoTextureViewProvider {
    /// Делегирует resource status lookup и lock timing в текущий VA-API provider.
    fn resource_lookup(
        &self,
        handle: video_core::FrameTextureHandle,
    ) -> PresentFrameResourceProviderLookup {
        let lookup = VideoTextureViewProvider::texture_view_lookup(self, handle);

        resource_provider_lookup_from_vaapi(lookup)
    }

    /// Делегирует non-blocking lookup в VA-API provider без раскрытия backend enum-а.
    fn try_resource_lookup(
        &self,
        handle: video_core::FrameTextureHandle,
    ) -> PresentFrameResourceProviderLookup {
        let lookup = VideoTextureViewProvider::try_texture_view_lookup(self, handle);

        resource_provider_lookup_from_vaapi(lookup)
    }

    /// Делегирует renderer-owned release в текущий VA-API production provider.
    fn release_frame(&self, handle: video_core::FrameTextureHandle) {
        VideoTextureViewProvider::release_frame(self, handle);
    }
}

/// Renderer-neutral state lookup-а без GPU handles.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum VaapiResourceLookupState {
    /// Backend вернул валидный resource для materialization.
    Ready,

    /// Backend pool занят и render hot path не должен ждать.
    Busy,

    /// Backend pool доступен, но handle отсутствует.
    Missing,

    /// Backend сообщил poisoned/fatal состояние.
    Error,
}

/// Конвертирует VA-API typed lookup в renderer-neutral player-core boundary.
fn resource_provider_lookup_from_vaapi(
    lookup: VideoTextureViewLookup,
) -> PresentFrameResourceProviderLookup {
    match lookup {
        VideoTextureViewLookup::Ready {
            lock_diagnostics, ..
        } => resource_provider_lookup_from_vaapi_state(
            VaapiResourceLookupState::Ready,
            lock_diagnostics.wait,
        ),
        VideoTextureViewLookup::Busy { lock_diagnostics } => {
            resource_provider_lookup_from_vaapi_state(
                VaapiResourceLookupState::Busy,
                lock_diagnostics.wait,
            )
        }
        VideoTextureViewLookup::Missing { lock_diagnostics } => {
            resource_provider_lookup_from_vaapi_state(
                VaapiResourceLookupState::Missing,
                lock_diagnostics.wait,
            )
        }
        VideoTextureViewLookup::Error { lock_diagnostics } => {
            resource_provider_lookup_from_vaapi_state(
                VaapiResourceLookupState::Error,
                lock_diagnostics.wait,
            )
        }
    }
}

/// Конвертирует state и timing без materialized WGPU handles.
fn resource_provider_lookup_from_vaapi_state(
    state: VaapiResourceLookupState,
    texture_pool_lock_wait: std::time::Duration,
) -> PresentFrameResourceProviderLookup {
    match state {
        VaapiResourceLookupState::Ready => PresentFrameResourceProviderLookup::Ready {
            texture_pool_lock_wait,
        },
        VaapiResourceLookupState::Busy => PresentFrameResourceProviderLookup::Busy {
            texture_pool_lock_wait,
        },
        VaapiResourceLookupState::Missing => PresentFrameResourceProviderLookup::Missing {
            texture_pool_lock_wait,
        },
        VaapiResourceLookupState::Error => PresentFrameResourceProviderLookup::Error {
            texture_pool_lock_wait,
        },
    }
}

impl VideoDecoderThreadHandle for VaapiVideoDecoderThreadHandle {
    type ResourceProvider = PresentFrameResourceProviderHandle;

    fn backend_name(&self) -> &'static str {
        VideoDecodeThread::backend_name(&self.decoder_thread)
    }

    fn send_packet(
        &self,
        packet: video_core::DecodePacket,
    ) -> Result<(), video_core::DecodeSendError> {
        VideoDecodeThread::send_packet(&self.decoder_thread, packet.into()).map_err(Into::into)
    }

    fn release_frame(&self, handle: video_core::FrameTextureHandle) {
        VideoDecodeThread::release_frame(&self.decoder_thread, handle);
    }

    fn try_recv_frame(&self) -> Option<video_core::DecodedFrame> {
        VideoDecodeThread::try_recv_frame(&self.decoder_thread)
    }

    fn try_recv_diagnostic_event(&self) -> Option<video_core::VideoDecoderDiagnosticEvent> {
        VideoDecodeThread::try_recv_diagnostic_event(&self.decoder_thread)
    }

    fn try_recv_error(&self) -> Option<video_core::DecodeThreadError> {
        VideoDecodeThread::try_recv_error(&self.decoder_thread).map(Into::into)
    }

    fn flush(&self) -> anyhow::Result<()> {
        VideoDecodeThread::flush(&self.decoder_thread)
    }

    fn resource_provider(&self) -> PresentFrameResourceProviderHandle {
        PresentFrameResourceProviderHandle::new(VideoDecodeThread::texture_view_provider(
            &self.decoder_thread,
        ))
    }

    fn decoder_resource_snapshot(&self) -> Option<video_core::DecoderResourceSnapshot> {
        VideoDecodeThread::texture_pool_stats(&self.decoder_thread).map(Into::into)
    }

    fn decoder_control_channel_pressure(
        &self,
    ) -> Option<video_core::VideoDecoderControlChannelPressureSnapshot> {
        let pressure = VideoDecodeThread::control_channel_pressure_stats(&self.decoder_thread);

        Some(pressure.into())
    }

    fn packet_queue_depth(&self) -> usize {
        VideoDecodeThread::packet_queue_depth(&self.decoder_thread)
    }

    fn drain_completed_packet_count(&self) -> usize {
        VideoDecodeThread::drain_completed_packet_count(&self.decoder_thread)
    }
}

#[cfg(test)]
mod tests {
    use std::time::Duration;

    use bytes::Bytes;
    use media_core::TrackId;

    use super::*;

    /// Compile-time helper фиксирует, что factory реализует neutral backend API.
    fn assert_video_backend_factory<T: VideoBackendFactory>() {}

    /// Проверяет dependency boundary: VA-API factory доступна через `video-backend-api`.
    #[test]
    fn vaapi_factory_implements_video_backend_factory_contract() {
        assert_video_backend_factory::<VaapiWgpuVideoBackendFactory>();
    }

    /// Проверяет, что packet adapter не теряет ownership payload-а и timing metadata.
    #[test]
    fn decode_packet_adapter_preserves_core_fields() {
        let packet = video_core::DecodePacket {
            track_id: TrackId::new(7),
            pts: Duration::from_millis(42),
            generation: 11,
            encoded_bytes: Bytes::from_static(b"encoded-vp9-packet"),
            keyframe: true,
            resolved_color: None,
        };

        let vaapi_packet: crate::DecodePacket = packet.clone().into();
        let neutral_packet = video_core::DecodePacket::from(vaapi_packet);

        assert_eq!(neutral_packet.track_id, packet.track_id);
        assert_eq!(neutral_packet.pts, packet.pts);
        assert_eq!(neutral_packet.generation, packet.generation);
        assert_eq!(neutral_packet.encoded_bytes, packet.encoded_bytes);
        assert_eq!(neutral_packet.keyframe, packet.keyframe);
        assert_eq!(neutral_packet.resolved_color, packet.resolved_color);
    }

    /// Проверяет, что neutral defaults совпадают с текущими VA-API production defaults.
    #[test]
    fn decoder_thread_config_default_matches_vaapi_default() {
        let vaapi_config =
            VideoDecodeThreadConfig::from(video_core::VideoDecoderThreadConfig::default());

        assert_eq!(vaapi_config, VideoDecodeThreadConfig::default());
    }

    /// Проверяет, что config adapter не теряет ни один startup/runtime limit.
    #[test]
    fn decoder_thread_config_adapter_preserves_limits() {
        let config = video_core::VideoDecoderThreadConfig {
            packet_channel_frames: 2,
            frame_channel_frames: 3,
            control_channel_frames: 4,
            decoder_ready_queue_frames: 5,
            decoder_surface_pool_frames: 6,
            zero_copy_surface_pool_slots: 7,
            flush_timeout: Duration::from_millis(75),
        };

        let vaapi_config = VideoDecodeThreadConfig::from(config);
        let roundtrip_config = video_core::VideoDecoderThreadConfig::from(vaapi_config);

        assert_eq!(roundtrip_config, config);
    }

    /// Проверяет, что typed backpressure остаётся отличимым от fatal send failure.
    #[test]
    fn send_error_adapter_preserves_backpressure_reason() {
        let vaapi_error = crate::DecodeThreadSendError::Backpressure(
            crate::DecodeThreadBackpressureReason::PacketQueueFull {
                queued_packets: 5,
                capacity: 8,
            },
        );

        let neutral_error = video_core::DecodeSendError::from(vaapi_error);

        match neutral_error {
            video_core::DecodeSendError::Backpressure(
                video_core::DecodeBackpressureReason::PacketQueueFull {
                    queued_packets,
                    capacity,
                },
            ) => {
                assert_eq!(queued_packets, 5);
                assert_eq!(capacity, 8);
            }
            unexpected_error => {
                panic!("expected packet queue backpressure, got {unexpected_error:?}");
            }
        }
    }

    /// Проверяет, что fatal adapter сохраняет сообщение для player error model.
    #[test]
    fn fatal_error_adapter_preserves_message() {
        let vaapi_error = crate::DecodeThreadError::new("decoder failed");

        let neutral_error = video_core::DecodeThreadError::from(vaapi_error);
        let roundtrip_error = crate::DecodeThreadError::from(neutral_error.clone());

        assert_eq!(neutral_error.message(), "decoder failed");
        assert_eq!(roundtrip_error.message(), "decoder failed");
    }

    /// Проверяет, что resource snapshot не теряет counters, нужные backpressure/UI.
    #[test]
    fn resource_snapshot_adapter_preserves_texture_pool_counters() {
        let stats = crate::texture_cache::TexturePoolStats {
            capacity: 10,
            slots: 6,
            in_use: 4,
            free_surfaces: 2,
            waiting_gpu_completion: 1,
            waiting_decoder_reuse: 1,
            import_failures: 3,
            imports_created: 7,
            imports_reused: 11,
            imports_replaced: 13,
        };

        let snapshot = video_core::DecoderResourceSnapshot::from(stats);
        let roundtrip_stats = crate::texture_cache::TexturePoolStats::from(snapshot);

        assert_eq!(snapshot.available_slots(), 6);
        assert_eq!(roundtrip_stats.capacity, stats.capacity);
        assert_eq!(roundtrip_stats.slots, stats.slots);
        assert_eq!(roundtrip_stats.in_use, stats.in_use);
        assert_eq!(roundtrip_stats.free_surfaces, stats.free_surfaces);
        assert_eq!(
            roundtrip_stats.waiting_gpu_completion,
            stats.waiting_gpu_completion
        );
        assert_eq!(
            roundtrip_stats.waiting_decoder_reuse,
            stats.waiting_decoder_reuse
        );
        assert_eq!(roundtrip_stats.import_failures, stats.import_failures);
        assert_eq!(roundtrip_stats.imports_created, stats.imports_created);
        assert_eq!(roundtrip_stats.imports_reused, stats.imports_reused);
        assert_eq!(roundtrip_stats.imports_replaced, stats.imports_replaced);
    }

    /// Проверяет, что control-channel diagnostics сохраняют все counters.
    #[test]
    fn control_channel_pressure_adapter_preserves_counters() {
        let stats = crate::VideoDecoderControlChannelPressureStats {
            control_channel_len: 3,
            control_channel_capacity: 5,
            control_channel_full_count: 7,
            release_control_send_fail_count: 11,
            flush_control_send_fail_count: 13,
        };

        let snapshot = video_core::VideoDecoderControlChannelPressureSnapshot::from(stats);
        let roundtrip_stats = crate::VideoDecoderControlChannelPressureStats::from(snapshot);

        assert_eq!(roundtrip_stats, stats);
    }

    /// Проверяет, что renderer-neutral adapter не схлопывает lookup states и timing.
    #[test]
    fn resource_lookup_state_adapter_preserves_all_outcomes_and_lock_wait() {
        let lock_wait = Duration::from_micros(42);

        let ready =
            resource_provider_lookup_from_vaapi_state(VaapiResourceLookupState::Ready, lock_wait);
        let busy =
            resource_provider_lookup_from_vaapi_state(VaapiResourceLookupState::Busy, lock_wait);
        let missing =
            resource_provider_lookup_from_vaapi_state(VaapiResourceLookupState::Missing, lock_wait);
        let error =
            resource_provider_lookup_from_vaapi_state(VaapiResourceLookupState::Error, lock_wait);

        assert!(matches!(
            ready,
            PresentFrameResourceProviderLookup::Ready {
                texture_pool_lock_wait
            } if texture_pool_lock_wait == lock_wait
        ));
        assert!(matches!(
            busy,
            PresentFrameResourceProviderLookup::Busy {
                texture_pool_lock_wait
            } if texture_pool_lock_wait == lock_wait
        ));
        assert!(matches!(
            missing,
            PresentFrameResourceProviderLookup::Missing {
                texture_pool_lock_wait
            } if texture_pool_lock_wait == lock_wait
        ));
        assert!(matches!(
            error,
            PresentFrameResourceProviderLookup::Error {
                texture_pool_lock_wait
            } if texture_pool_lock_wait == lock_wait
        ));
    }
}
