use std::sync::Arc;

use crate::decoder_boundary::{
    DecodeSendError, DecodeThreadError, DecoderResourceSnapshot, PlayerDecodePacket,
    WgpuRenderTextureProvider, WgpuRenderTextureProviderHandle, WgpuRenderTextureViewLookup,
    WgpuRenderTextureViews,
};
use video_core::VideoDecoderThreadHandle;

use super::config::VideoBackendStartupRequest;

/// Player-core adapter вокруг concrete VA-API decoder thread.
struct VaapiVideoDecoderThreadHandle {
    /// Concrete backend остаётся скрыт за neutral contract boundary.
    decoder_thread: video_vaapi::VideoDecodeThread,
}

impl VaapiVideoDecoderThreadHandle {
    /// Оборачивает запущенный VA-API thread без изменения lifecycle ownership.
    fn new(decoder_thread: video_vaapi::VideoDecodeThread) -> Self {
        Self { decoder_thread }
    }
}

/// Стартует текущий production decoder backend за neutral factory boundary.
pub(super) fn start_video_decoder_thread(
    startup_request: &VideoBackendStartupRequest<'_>,
) -> anyhow::Result<
    impl VideoDecoderThreadHandle<TextureViewProvider = WgpuRenderTextureProviderHandle> + 'static,
> {
    let wgpu_context = startup_request.wgpu_context();
    let device = Arc::new(wgpu_context.device().clone());
    let queue = Arc::new(wgpu_context.queue().clone());

    let decoder_thread = video_vaapi::VideoDecodeThread::new_with_config(
        device,
        queue,
        wgpu_context.instance().clone(),
        wgpu_context.adapter().clone(),
        startup_request.decoder_thread_config().into(),
    )?;

    Ok(VaapiVideoDecoderThreadHandle::new(decoder_thread))
}

impl WgpuRenderTextureProvider for video_vaapi::VideoTextureViewProvider {
    /// Делегирует texture view lookup и lock timing в текущий VA-API production provider.
    fn texture_view_lookup(
        &self,
        handle: video_core::FrameTextureHandle,
    ) -> WgpuRenderTextureViewLookup {
        let lookup = video_vaapi::VideoTextureViewProvider::texture_view_lookup(self, handle);

        wgpu_render_texture_view_lookup_from_vaapi(lookup)
    }

    /// Делегирует non-blocking lookup в VA-API provider без раскрытия backend enum-а.
    fn try_texture_view_lookup(
        &self,
        handle: video_core::FrameTextureHandle,
    ) -> WgpuRenderTextureViewLookup {
        let lookup = video_vaapi::VideoTextureViewProvider::try_texture_view_lookup(self, handle);

        wgpu_render_texture_view_lookup_from_vaapi(lookup)
    }

    /// Делегирует renderer-owned release в текущий VA-API production provider.
    fn release_frame(&self, handle: video_core::FrameTextureHandle) {
        video_vaapi::VideoTextureViewProvider::release_frame(self, handle);
    }
}

/// Конвертирует VA-API typed lookup в WGPU-specific player-core boundary.
fn wgpu_render_texture_view_lookup_from_vaapi(
    lookup: video_vaapi::VideoTextureViewLookup,
) -> WgpuRenderTextureViewLookup {
    match lookup {
        video_vaapi::VideoTextureViewLookup::Ready {
            views,
            lock_diagnostics,
        } => WgpuRenderTextureViewLookup::Ready {
            views: WgpuRenderTextureViews {
                y_view: views.y_view,
                uv_view: views.uv_view,
            },
            texture_pool_lock_wait: lock_diagnostics.wait,
        },
        video_vaapi::VideoTextureViewLookup::Busy { lock_diagnostics } => {
            WgpuRenderTextureViewLookup::Busy {
                texture_pool_lock_wait: lock_diagnostics.wait,
            }
        }
        video_vaapi::VideoTextureViewLookup::Missing { lock_diagnostics } => {
            WgpuRenderTextureViewLookup::Missing {
                texture_pool_lock_wait: lock_diagnostics.wait,
            }
        }
        video_vaapi::VideoTextureViewLookup::Error { lock_diagnostics } => {
            WgpuRenderTextureViewLookup::Error {
                texture_pool_lock_wait: lock_diagnostics.wait,
            }
        }
    }
}

impl VideoDecoderThreadHandle for VaapiVideoDecoderThreadHandle {
    type TextureViewProvider = WgpuRenderTextureProviderHandle;

    fn backend_name(&self) -> &'static str {
        video_vaapi::VideoDecodeThread::backend_name(&self.decoder_thread)
    }

    fn send_packet(&self, packet: PlayerDecodePacket) -> Result<(), DecodeSendError> {
        video_vaapi::VideoDecodeThread::send_packet(&self.decoder_thread, packet.into())
            .map_err(Into::into)
    }

    fn release_frame(&self, handle: video_core::FrameTextureHandle) {
        video_vaapi::VideoDecodeThread::release_frame(&self.decoder_thread, handle);
    }

    fn try_recv_frame(&self) -> Option<video_core::DecodedFrame> {
        video_vaapi::VideoDecodeThread::try_recv_frame(&self.decoder_thread)
    }

    fn try_recv_diagnostic_event(&self) -> Option<video_core::VideoDecoderDiagnosticEvent> {
        video_vaapi::VideoDecodeThread::try_recv_diagnostic_event(&self.decoder_thread)
    }

    fn try_recv_error(&self) -> Option<DecodeThreadError> {
        video_vaapi::VideoDecodeThread::try_recv_error(&self.decoder_thread).map(Into::into)
    }

    fn flush(&self) -> anyhow::Result<()> {
        video_vaapi::VideoDecodeThread::flush(&self.decoder_thread)
    }

    fn texture_view_provider(&self) -> WgpuRenderTextureProviderHandle {
        WgpuRenderTextureProviderHandle::new(video_vaapi::VideoDecodeThread::texture_view_provider(
            &self.decoder_thread,
        ))
    }

    fn decoder_resource_snapshot(&self) -> Option<DecoderResourceSnapshot> {
        video_vaapi::VideoDecodeThread::texture_pool_stats(&self.decoder_thread).map(Into::into)
    }

    fn decoder_control_channel_pressure(
        &self,
    ) -> Option<video_core::VideoDecoderControlChannelPressureSnapshot> {
        let pressure =
            video_vaapi::VideoDecodeThread::control_channel_pressure_stats(&self.decoder_thread);

        Some(pressure.into())
    }

    fn packet_queue_depth(&self) -> usize {
        video_vaapi::VideoDecodeThread::packet_queue_depth(&self.decoder_thread)
    }

    fn drain_completed_packet_count(&self) -> usize {
        video_vaapi::VideoDecodeThread::drain_completed_packet_count(&self.decoder_thread)
    }
}

#[cfg(test)]
mod tests {
    use std::time::Duration;

    use bytes::Bytes;
    use media_core::TrackId;

    use crate::decoder_boundary::{DecodeBackpressureReason, PlayerVideoDecoderThreadConfig};

    use super::*;

    /// Проверяет, что packet adapter не теряет ownership payload-а и timing metadata.
    #[test]
    fn decode_packet_adapter_preserves_core_fields() {
        let packet = PlayerDecodePacket {
            track_id: TrackId::new(7),
            pts: Duration::from_millis(42),
            encoded_bytes: Bytes::from_static(b"encoded-vp9-packet"),
            keyframe: true,
            resolved_color: None,
        };

        let vaapi_packet: video_vaapi::DecodePacket = packet.clone().into();
        let neutral_packet = PlayerDecodePacket::from(vaapi_packet);

        assert_eq!(neutral_packet.track_id, packet.track_id);
        assert_eq!(neutral_packet.pts, packet.pts);
        assert_eq!(neutral_packet.encoded_bytes, packet.encoded_bytes);
        assert_eq!(neutral_packet.keyframe, packet.keyframe);
        assert_eq!(neutral_packet.resolved_color, packet.resolved_color);
    }

    /// Проверяет, что neutral defaults совпадают с текущими VA-API production defaults.
    #[test]
    fn decoder_thread_config_default_matches_vaapi_default() {
        let vaapi_config =
            video_vaapi::VideoDecodeThreadConfig::from(PlayerVideoDecoderThreadConfig::default());

        assert_eq!(
            vaapi_config,
            video_vaapi::VideoDecodeThreadConfig::default()
        );
    }

    /// Проверяет, что config adapter не теряет ни один startup/runtime limit.
    #[test]
    fn decoder_thread_config_adapter_preserves_limits() {
        let config = PlayerVideoDecoderThreadConfig {
            packet_channel_frames: 2,
            frame_channel_frames: 3,
            control_channel_frames: 4,
            decoder_ready_queue_frames: 5,
            decoder_surface_pool_frames: 6,
            zero_copy_surface_pool_slots: 7,
            flush_timeout: Duration::from_millis(75),
        };

        let vaapi_config = video_vaapi::VideoDecodeThreadConfig::from(config);
        let roundtrip_config = PlayerVideoDecoderThreadConfig::from(vaapi_config);

        assert_eq!(roundtrip_config, config);
    }

    /// Проверяет, что typed backpressure остаётся отличимым от fatal send failure.
    #[test]
    fn send_error_adapter_preserves_backpressure_reason() {
        let vaapi_error = video_vaapi::DecodeThreadSendError::Backpressure(
            video_vaapi::DecodeThreadBackpressureReason::PacketQueueFull {
                queued_packets: 5,
                capacity: 8,
            },
        );

        let neutral_error = DecodeSendError::from(vaapi_error);

        match neutral_error {
            DecodeSendError::Backpressure(DecodeBackpressureReason::PacketQueueFull {
                queued_packets,
                capacity,
            }) => {
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
        let vaapi_error = video_vaapi::DecodeThreadError::new("decoder failed");

        let neutral_error = DecodeThreadError::from(vaapi_error);
        let roundtrip_error = video_vaapi::DecodeThreadError::from(neutral_error.clone());

        assert_eq!(neutral_error.message(), "decoder failed");
        assert_eq!(roundtrip_error.message(), "decoder failed");
    }

    /// Проверяет, что resource snapshot не теряет counters, нужные backpressure/UI.
    #[test]
    fn resource_snapshot_adapter_preserves_texture_pool_counters() {
        let stats = video_vaapi::texture_cache::TexturePoolStats {
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

        let snapshot = DecoderResourceSnapshot::from(stats);
        let roundtrip_stats = video_vaapi::texture_cache::TexturePoolStats::from(snapshot);

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
}
