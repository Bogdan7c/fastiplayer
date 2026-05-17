use std::sync::Arc;
use std::time::Duration;

use bytes::Bytes;
use codec_core::VideoColorMetadata;
use media_core::TrackId;

/// Encoded video packet на границе player-core -> decoder backend.
#[derive(Debug, Clone)]
pub(crate) struct PlayerDecodePacket {
    /// Track ID выбранного video stream.
    pub(crate) track_id: TrackId,

    /// Presentation timestamp packet-а.
    pub(crate) pts: Duration,

    /// Encoded bytes без привязки к конкретному hardware backend-у.
    pub(crate) encoded_bytes: Bytes,

    /// Keyframe flag из container/demuxer.
    pub(crate) keyframe: bool,

    /// Resolved color metadata из player/capability layer для decoded frame contract.
    pub(crate) resolved_color: Option<VideoColorMetadata>,
}

/// Fatal ошибка decoder thread, которую player layer должен показать как runtime failure.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct DecodeThreadError {
    /// Человекочитаемая причина остановки decoder backend-а.
    message: String,
}

impl DecodeThreadError {
    /// Создаёт ошибку decoder boundary без backend-specific типа.
    #[must_use]
    pub(crate) fn new(message: impl Into<String>) -> Self {
        Self {
            message: message.into(),
        }
    }

    /// Возвращает текст ошибки для player-core/UI.
    #[must_use]
    pub(crate) fn message(&self) -> &str {
        &self.message
    }

    /// Передаёт ownership строки adapter-у, которому нужен внешний error type.
    #[must_use]
    fn into_message(self) -> String {
        self.message
    }
}

impl std::fmt::Display for DecodeThreadError {
    /// Печатает только полезный текст ошибки без Debug-шума.
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter.write_str(&self.message)
    }
}

impl std::error::Error for DecodeThreadError {}

/// Typed причина, по которой decoder backend временно не принимает packet.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum DecodeBackpressureReason {
    /// Bounded packet queue заполнена: decoder ещё не забрал старые packets.
    PacketQueueFull {
        /// Текущая глубина packet queue.
        queued_packets: usize,

        /// Bounded capacity packet queue.
        capacity: usize,
    },
}

impl std::fmt::Display for DecodeBackpressureReason {
    /// Печатает причину backpressure без потери чисел очереди.
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::PacketQueueFull {
                queued_packets,
                capacity,
            } => write!(
                formatter,
                "decoder packet channel is full: queued={queued_packets}, capacity={capacity}"
            ),
        }
    }
}

/// Ошибка постановки packet-а в decoder backend.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum DecodeSendError {
    /// Decoder backend жив, но bounded queue сейчас заполнена.
    Backpressure(DecodeBackpressureReason),

    /// Decoder backend уже fail-closed или receiver отключён.
    Fatal(DecodeThreadError),
}

impl std::fmt::Display for DecodeSendError {
    /// Печатает machine-actionable причину отправки packet-а.
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Backpressure(reason) => write!(formatter, "{reason}"),
            Self::Fatal(error) => write!(formatter, "{error}"),
        }
    }
}

impl std::error::Error for DecodeSendError {}

/// Backend-neutral snapshot decoder/render ресурсов для diagnostics и backpressure.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) struct DecoderResourceSnapshot {
    /// Максимальное число persistent texture/import slots.
    pub(crate) capacity: usize,

    /// Сколько persistent texture/import slots сейчас создано.
    pub(crate) slots: usize,

    /// Сколько surfaces сейчас нельзя переиспользовать decoder-у.
    pub(crate) in_use: usize,

    /// Сколько imported surfaces свободно для reuse.
    pub(crate) free_surfaces: usize,

    /// Сколько releases ждёт GPU completion callback.
    pub(crate) waiting_gpu_completion: usize,

    /// Сколько releases ждёт возврата decoded handle в decoder pool.
    pub(crate) waiting_decoder_reuse: usize,

    /// Сколько external imports завершилось ошибкой.
    pub(crate) import_failures: u64,

    /// Сколько external imports реально создано.
    pub(crate) imports_created: u64,

    /// Сколько кадров переиспользовало existing import.
    pub(crate) imports_reused: u64,

    /// Сколько free imports было заменено из-за смены backing object/layout.
    pub(crate) imports_replaced: u64,
}

impl DecoderResourceSnapshot {
    /// Возвращает число slots, которые ещё можно занять или переиспользовать.
    #[must_use]
    pub(crate) const fn available_slots(self) -> usize {
        self.capacity.saturating_sub(self.in_use)
    }
}

/// WGPU texture views, полученные render thread-ом по opaque frame handle.
pub(crate) struct RenderTextureViews {
    /// Texture view с luma/Y plane.
    pub(crate) y_view: wgpu::TextureView,

    /// Texture view с chroma/UV plane.
    pub(crate) uv_view: wgpu::TextureView,
}

/// Backend-neutral render-side provider для texture views и renderer-owned release.
pub(crate) trait RenderTextureProvider: Send + Sync {
    /// Получает WGPU views для frame handle на render thread.
    fn texture_views(&self, handle: video_core::FrameTextureHandle) -> Option<RenderTextureViews>;

    /// Освобождает renderer-owned frame после submitted GPU work или fallback release.
    fn release_frame(&self, handle: video_core::FrameTextureHandle);
}

/// Clone-able handle, который скрывает конкретный backend provider за trait boundary.
#[derive(Clone)]
pub(crate) struct RenderTextureProviderHandle {
    /// Shared provider живёт столько же, сколько render leases, которые его держат.
    provider: Arc<dyn RenderTextureProvider>,
}

impl RenderTextureProviderHandle {
    /// Оборачивает concrete backend provider в neutral render boundary handle.
    #[must_use]
    pub(crate) fn new(provider: impl RenderTextureProvider + 'static) -> Self {
        Self {
            provider: Arc::new(provider),
        }
    }

    /// Получает WGPU views через backend provider без раскрытия backend type.
    #[must_use]
    pub(crate) fn texture_views(
        &self,
        handle: video_core::FrameTextureHandle,
    ) -> Option<RenderTextureViews> {
        self.provider.texture_views(handle)
    }

    /// Освобождает frame через backend provider, который создал texture handle.
    pub(crate) fn release_frame(&self, handle: video_core::FrameTextureHandle) {
        self.provider.release_frame(handle);
    }
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
    /// Возвращает VA-API packet в neutral форму для tests/adapter coverage.
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

impl From<video_vaapi::DecodeThreadError> for DecodeThreadError {
    /// Сохраняет текст fatal ошибки без привязки player-core к VA-API error type.
    fn from(error: video_vaapi::DecodeThreadError) -> Self {
        Self::new(error.message().to_owned())
    }
}

impl From<DecodeThreadError> for video_vaapi::DecodeThreadError {
    /// Адаптирует neutral fatal error для VA-API-facing fake/adapter paths.
    fn from(error: DecodeThreadError) -> Self {
        Self::new(error.into_message())
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
    /// Адаптирует neutral send error к VA-API-facing fake/adapter paths.
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

#[cfg(test)]
mod tests {
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
