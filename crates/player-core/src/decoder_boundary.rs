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

/// Production default packet channel capacity между worker и decoder thread.
const DEFAULT_DECODER_PACKET_CHANNEL_FRAMES: usize = 32;

/// Production default decoded frame channel capacity между decoder thread и worker.
const DEFAULT_DECODER_FRAME_CHANNEL_FRAMES: usize = 8;

/// Production default control/release channel capacity decoder thread-а.
const DEFAULT_DECODER_CONTROL_CHANNEL_FRAMES: usize = 32;

/// Production default backend-local ready queue capacity.
const DEFAULT_DECODER_READY_QUEUE_FRAMES: usize = 8;

/// Production default output surface descriptor pool size.
const DEFAULT_DECODER_SURFACE_POOL_FRAMES: usize = 24;

/// Production default zero-copy external import slot capacity.
const DEFAULT_ZERO_COPY_SURFACE_POOL_SLOTS: usize = 24;

/// Env-переменная для настройки flush timeout-а без перекомпиляции приложения.
const DECODER_FLUSH_TIMEOUT_ENV_VAR: &str = "VIDEOPLAYER_DECODER_FLUSH_TIMEOUT_MS";

/// Production default flush timeout decoder thread-а в миллисекундах.
const DEFAULT_DECODER_FLUSH_TIMEOUT_MS: u64 = 2_000;

/// Backend-neutral runtime limits decoder thread-а.
///
/// Этот тип принадлежит player-core startup/config boundary. Concrete backend
/// получает его только через adapter conversion, поэтому worker/session callers
/// не зависят от конкретного decoder implementation.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct PlayerVideoDecoderThreadConfig {
    /// Packet channel capacity между worker и decoder thread.
    pub packet_channel_frames: usize,

    /// Decoded frame channel capacity между decoder thread и worker.
    pub frame_channel_frames: usize,

    /// Control/release channel capacity для release/flush сообщений.
    pub control_channel_frames: usize,

    /// Backend-local ready queue capacity внутри decoder wrapper-а.
    pub decoder_ready_queue_frames: usize,

    /// Hardware decoder output surface descriptor pool size.
    pub decoder_surface_pool_frames: usize,

    /// Zero-copy external import slot capacity.
    pub zero_copy_surface_pool_slots: usize,

    /// Максимальное время ожидания подтверждения flush от decoder thread.
    pub flush_timeout: Duration,
}

impl PlayerVideoDecoderThreadConfig {
    /// Загружает production defaults и overlay flush timeout-а из окружения.
    #[must_use]
    pub fn from_env() -> Self {
        let mut config = Self::default();
        config.flush_timeout = match std::env::var(DECODER_FLUSH_TIMEOUT_ENV_VAR) {
            Ok(raw_value) => match Self::parse_flush_timeout(&raw_value) {
                Ok(timeout) => timeout,
                Err(error) => {
                    tracing::warn!(
                        error = %error,
                        env_var = DECODER_FLUSH_TIMEOUT_ENV_VAR,
                        default_timeout_ms = DEFAULT_DECODER_FLUSH_TIMEOUT_MS,
                        "Invalid decoder flush timeout config; using default"
                    );
                    Self::default_flush_timeout()
                }
            },
            Err(std::env::VarError::NotPresent) => Self::default_flush_timeout(),
            Err(error) => {
                tracing::warn!(
                    error = %error,
                    env_var = DECODER_FLUSH_TIMEOUT_ENV_VAR,
                    default_timeout_ms = DEFAULT_DECODER_FLUSH_TIMEOUT_MS,
                    "Cannot read decoder flush timeout config; using default"
                );
                Self::default_flush_timeout()
            }
        };
        config
    }

    /// Возвращает default timeout как `Duration`.
    const fn default_flush_timeout() -> Duration {
        Duration::from_millis(DEFAULT_DECODER_FLUSH_TIMEOUT_MS)
    }

    /// Парсит env timeout в миллисекундах без изменения process env в тестах.
    fn parse_flush_timeout(raw_value: &str) -> anyhow::Result<Duration> {
        let timeout_ms = raw_value.trim().parse::<u64>().map_err(|error| {
            anyhow::anyhow!(
                "expected positive integer milliseconds, got {:?}: {}",
                raw_value,
                error
            )
        })?;
        if timeout_ms == 0 {
            anyhow::bail!("decoder flush timeout must be greater than 0 ms");
        }
        Ok(Duration::from_millis(timeout_ms))
    }

    /// Нормализует direct API values, чтобы startup не получил zero-capacity queues.
    #[must_use]
    pub fn normalized(self) -> Self {
        Self {
            packet_channel_frames: self.packet_channel_frames.max(1),
            frame_channel_frames: self.frame_channel_frames.max(1),
            control_channel_frames: self.control_channel_frames.max(1),
            decoder_ready_queue_frames: self.decoder_ready_queue_frames.max(1),
            decoder_surface_pool_frames: self.decoder_surface_pool_frames.max(1),
            zero_copy_surface_pool_slots: self.zero_copy_surface_pool_slots.max(1),
            flush_timeout: self.flush_timeout.max(Duration::from_millis(1)),
        }
    }
}

impl Default for PlayerVideoDecoderThreadConfig {
    /// Возвращает production defaults без unbounded очередей.
    fn default() -> Self {
        Self {
            packet_channel_frames: DEFAULT_DECODER_PACKET_CHANNEL_FRAMES,
            frame_channel_frames: DEFAULT_DECODER_FRAME_CHANNEL_FRAMES,
            control_channel_frames: DEFAULT_DECODER_CONTROL_CHANNEL_FRAMES,
            decoder_ready_queue_frames: DEFAULT_DECODER_READY_QUEUE_FRAMES,
            decoder_surface_pool_frames: DEFAULT_DECODER_SURFACE_POOL_FRAMES,
            zero_copy_surface_pool_slots: DEFAULT_ZERO_COPY_SURFACE_POOL_SLOTS,
            flush_timeout: PlayerVideoDecoderThreadConfig::default_flush_timeout(),
        }
    }
}

/// WGPU texture views, полученные render thread-ом по opaque frame handle.
pub(crate) struct RenderTextureViews {
    /// Texture view с luma/Y plane.
    pub(crate) y_view: wgpu::TextureView,

    /// Texture view с chroma/UV plane.
    pub(crate) uv_view: wgpu::TextureView,
}

/// Результат renderer-side lookup-а texture views без раскрытия backend pool-а.
pub(crate) struct RenderTextureViewLookup {
    /// Views для renderer-а; `None` сохраняет прежнюю missing-resource семантику.
    pub(crate) views: Option<RenderTextureViews>,

    /// Сколько render thread ждал lock texture pool-а внутри backend provider-а.
    pub(crate) texture_pool_lock_wait: Duration,
}

/// Backend-neutral render-side provider для texture views и renderer-owned release.
pub(crate) trait RenderTextureProvider: Send + Sync {
    /// Получает WGPU views и lock diagnostics для frame handle на render thread.
    fn texture_view_lookup(
        &self,
        handle: video_core::FrameTextureHandle,
    ) -> RenderTextureViewLookup;

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

    /// Получает WGPU views и lock diagnostics через backend provider.
    #[must_use]
    pub(crate) fn texture_view_lookup(
        &self,
        handle: video_core::FrameTextureHandle,
    ) -> RenderTextureViewLookup {
        self.provider.texture_view_lookup(handle)
    }

    /// Освобождает frame через backend provider, который создал texture handle.
    pub(crate) fn release_frame(&self, handle: video_core::FrameTextureHandle) {
        self.provider.release_frame(handle);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Проверяет, что direct API caller не может создать zero-capacity очереди.
    #[test]
    fn decoder_thread_config_normalizes_zero_limits() {
        let config = PlayerVideoDecoderThreadConfig {
            packet_channel_frames: 0,
            frame_channel_frames: 0,
            control_channel_frames: 0,
            decoder_ready_queue_frames: 0,
            decoder_surface_pool_frames: 0,
            zero_copy_surface_pool_slots: 0,
            flush_timeout: Duration::ZERO,
        }
        .normalized();

        assert_eq!(config.packet_channel_frames, 1);
        assert_eq!(config.frame_channel_frames, 1);
        assert_eq!(config.control_channel_frames, 1);
        assert_eq!(config.decoder_ready_queue_frames, 1);
        assert_eq!(config.decoder_surface_pool_frames, 1);
        assert_eq!(config.zero_copy_surface_pool_slots, 1);
        assert_eq!(config.flush_timeout, Duration::from_millis(1));
    }

    /// Проверяет parsing policy env timeout-а без изменения process env.
    #[test]
    fn decoder_thread_config_flush_timeout_parser_rejects_invalid_values() {
        assert!(PlayerVideoDecoderThreadConfig::parse_flush_timeout("0").is_err());
        assert!(PlayerVideoDecoderThreadConfig::parse_flush_timeout("abc").is_err());
        assert_eq!(
            PlayerVideoDecoderThreadConfig::parse_flush_timeout("25").unwrap(),
            Duration::from_millis(25)
        );
    }
}
