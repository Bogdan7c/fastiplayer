use std::sync::Arc;

use crate::decoder_boundary::{
    DecodeBackpressureReason, DecodeSendError, DecodeThreadError, DecoderResourceSnapshot,
    PlayerDecodePacket, PlayerVideoDecoderThreadConfig, RenderTextureProvider,
    RenderTextureProviderHandle, RenderTextureViews,
};
use crate::pipeline::VideoDecoderThreadHandle;

use super::config::VideoBackendStartupRequest;

/// Стартует текущий production decoder backend за neutral factory boundary.
pub(super) fn start_video_decoder_thread(
    startup_request: &VideoBackendStartupRequest<'_>,
) -> anyhow::Result<impl VideoDecoderThreadHandle + 'static> {
    let wgpu_context = startup_request.wgpu_context();
    let device = Arc::new(wgpu_context.device().clone());
    let queue = Arc::new(wgpu_context.queue().clone());

    video_vaapi::VideoDecodeThread::new_with_config(
        device,
        queue,
        wgpu_context.instance().clone(),
        wgpu_context.adapter().clone(),
        startup_request.decoder_thread_config().into(),
    )
}

impl From<PlayerDecodePacket> for video_vaapi::DecodePacket {
    /// Адаптирует neutral packet к текущему production VA-API backend-у.
    fn from(packet: PlayerDecodePacket) -> Self {
        Self {
            track_id: packet.track_id,
            pts: packet.pts,
            encoded_bytes: packet.encoded_bytes,
            keyframe: packet.keyframe,
            resolved_color: packet.resolved_color,
        }
    }
}

impl From<video_vaapi::DecodePacket> for PlayerDecodePacket {
    /// Возвращает VA-API packet в neutral форму для adapter coverage.
    fn from(packet: video_vaapi::DecodePacket) -> Self {
        Self {
            track_id: packet.track_id,
            pts: packet.pts,
            encoded_bytes: packet.encoded_bytes,
            keyframe: packet.keyframe,
            resolved_color: packet.resolved_color,
        }
    }
}

impl From<PlayerVideoDecoderThreadConfig> for video_vaapi::VideoDecodeThreadConfig {
    /// Адаптирует neutral decoder-thread limits к текущему VA-API production backend-у.
    fn from(config: PlayerVideoDecoderThreadConfig) -> Self {
        Self {
            packet_channel_frames: config.packet_channel_frames,
            frame_channel_frames: config.frame_channel_frames,
            control_channel_frames: config.control_channel_frames,
            decoder_ready_queue_frames: config.decoder_ready_queue_frames,
            decoder_surface_pool_frames: config.decoder_surface_pool_frames,
            zero_copy_surface_pool_slots: config.zero_copy_surface_pool_slots,
            flush_timeout: config.flush_timeout,
        }
    }
}

impl From<video_vaapi::VideoDecodeThreadConfig> for PlayerVideoDecoderThreadConfig {
    /// Возвращает VA-API config в neutral форму для compatibility и adapter tests.
    fn from(config: video_vaapi::VideoDecodeThreadConfig) -> Self {
        Self {
            packet_channel_frames: config.packet_channel_frames,
            frame_channel_frames: config.frame_channel_frames,
            control_channel_frames: config.control_channel_frames,
            decoder_ready_queue_frames: config.decoder_ready_queue_frames,
            decoder_surface_pool_frames: config.decoder_surface_pool_frames,
            zero_copy_surface_pool_slots: config.zero_copy_surface_pool_slots,
            flush_timeout: config.flush_timeout,
        }
    }
}

impl From<video_vaapi::DecodeThreadError> for DecodeThreadError {
    /// Сохраняет текст fatal ошибки без привязки player-core к VA-API error type.
    fn from(error: video_vaapi::DecodeThreadError) -> Self {
        Self::new(error.message().to_owned())
    }
}

impl From<DecodeThreadError> for video_vaapi::DecodeThreadError {
    /// Адаптирует neutral fatal error для VA-API-facing adapter paths.
    fn from(error: DecodeThreadError) -> Self {
        Self::new(error.message().to_owned())
    }
}

impl From<video_vaapi::DecodeThreadBackpressureReason> for DecodeBackpressureReason {
    /// Сохраняет typed backpressure reason и queue accounting.
    fn from(reason: video_vaapi::DecodeThreadBackpressureReason) -> Self {
        match reason {
            video_vaapi::DecodeThreadBackpressureReason::PacketQueueFull {
                queued_packets,
                capacity,
            } => Self::PacketQueueFull {
                queued_packets,
                capacity,
            },
        }
    }
}

impl From<DecodeBackpressureReason> for video_vaapi::DecodeThreadBackpressureReason {
    /// Адаптирует neutral backpressure reason к текущему VA-API send error.
    fn from(reason: DecodeBackpressureReason) -> Self {
        match reason {
            DecodeBackpressureReason::PacketQueueFull {
                queued_packets,
                capacity,
            } => Self::PacketQueueFull {
                queued_packets,
                capacity,
            },
        }
    }
}

impl From<video_vaapi::DecodeThreadSendError> for DecodeSendError {
    /// Сохраняет различие backpressure/fatal на player-core boundary.
    fn from(error: video_vaapi::DecodeThreadSendError) -> Self {
        match error {
            video_vaapi::DecodeThreadSendError::Backpressure(reason) => {
                Self::Backpressure(reason.into())
            }
            video_vaapi::DecodeThreadSendError::Fatal(error) => Self::Fatal(error.into()),
        }
    }
}

impl From<DecodeSendError> for video_vaapi::DecodeThreadSendError {
    /// Адаптирует neutral send error к VA-API-facing adapter paths.
    fn from(error: DecodeSendError) -> Self {
        match error {
            DecodeSendError::Backpressure(reason) => Self::Backpressure(reason.into()),
            DecodeSendError::Fatal(error) => Self::Fatal(error.into()),
        }
    }
}

impl From<video_vaapi::texture_cache::TexturePoolStats> for DecoderResourceSnapshot {
    /// Копирует VA-API texture pool counters в backend-neutral diagnostics snapshot.
    fn from(stats: video_vaapi::texture_cache::TexturePoolStats) -> Self {
        Self {
            capacity: stats.capacity,
            slots: stats.slots,
            in_use: stats.in_use,
            free_surfaces: stats.free_surfaces,
            waiting_gpu_completion: stats.waiting_gpu_completion,
            waiting_decoder_reuse: stats.waiting_decoder_reuse,
            import_failures: stats.import_failures,
            imports_created: stats.imports_created,
            imports_reused: stats.imports_reused,
            imports_replaced: stats.imports_replaced,
        }
    }
}

impl From<DecoderResourceSnapshot> for video_vaapi::texture_cache::TexturePoolStats {
    /// Адаптирует neutral diagnostics snapshot обратно к текущему VA-API stats type.
    fn from(stats: DecoderResourceSnapshot) -> Self {
        Self {
            capacity: stats.capacity,
            slots: stats.slots,
            in_use: stats.in_use,
            free_surfaces: stats.free_surfaces,
            waiting_gpu_completion: stats.waiting_gpu_completion,
            waiting_decoder_reuse: stats.waiting_decoder_reuse,
            import_failures: stats.import_failures,
            imports_created: stats.imports_created,
            imports_reused: stats.imports_reused,
            imports_replaced: stats.imports_replaced,
        }
    }
}

impl RenderTextureProvider for video_vaapi::VideoTextureViewProvider {
    /// Делегирует texture view lookup в текущий VA-API production provider.
    fn texture_views(&self, handle: video_core::FrameTextureHandle) -> Option<RenderTextureViews> {
        video_vaapi::VideoTextureViewProvider::texture_views(self, handle).map(|views| {
            RenderTextureViews {
                y_view: views.y_view,
                uv_view: views.uv_view,
            }
        })
    }

    /// Делегирует renderer-owned release в текущий VA-API production provider.
    fn release_frame(&self, handle: video_core::FrameTextureHandle) {
        video_vaapi::VideoTextureViewProvider::release_frame(self, handle);
    }
}

impl VideoDecoderThreadHandle for video_vaapi::VideoDecodeThread {
    fn backend_name(&self) -> &'static str {
        video_vaapi::VideoDecodeThread::backend_name(self)
    }

    fn send_packet(&self, packet: PlayerDecodePacket) -> Result<(), DecodeSendError> {
        video_vaapi::VideoDecodeThread::send_packet(self, packet.into()).map_err(Into::into)
    }

    fn release_frame(&self, handle: video_core::FrameTextureHandle) {
        video_vaapi::VideoDecodeThread::release_frame(self, handle);
    }

    fn try_recv_frame(&self) -> Option<video_core::DecodedFrame> {
        video_vaapi::VideoDecodeThread::try_recv_frame(self)
    }

    fn try_recv_diagnostic_event(&self) -> Option<video_core::VideoDecoderDiagnosticEvent> {
        video_vaapi::VideoDecodeThread::try_recv_diagnostic_event(self)
    }

    fn try_recv_error(&self) -> Option<DecodeThreadError> {
        video_vaapi::VideoDecodeThread::try_recv_error(self).map(Into::into)
    }

    fn flush(&self) -> anyhow::Result<()> {
        video_vaapi::VideoDecodeThread::flush(self)
    }

    fn texture_view_provider(&self) -> RenderTextureProviderHandle {
        RenderTextureProviderHandle::new(video_vaapi::VideoDecodeThread::texture_view_provider(
            self,
        ))
    }

    fn decoder_resource_snapshot(&self) -> Option<DecoderResourceSnapshot> {
        video_vaapi::VideoDecodeThread::texture_pool_stats(self).map(Into::into)
    }

    fn packet_queue_depth(&self) -> usize {
        video_vaapi::VideoDecodeThread::packet_queue_depth(self)
    }

    fn drain_completed_packet_count(&self) -> usize {
        video_vaapi::VideoDecodeThread::drain_completed_packet_count(self)
    }
}

#[cfg(test)]
mod tests {
    use std::time::Duration;

    use bytes::Bytes;
    use media_core::TrackId;

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
