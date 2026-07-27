mod activity;
mod config;
mod control;
mod protocol;

pub use activity::{
    VideoDecoderActivityEpoch, VideoDecoderActivityNotifier, VideoDecoderActivitySnapshot,
    VideoDecoderActivitySubscription, VideoDecoderActivityUnavailableReason,
    VideoDecoderActivityWaitOutcome,
};
pub use config::{SoftwareDecodeThreadBudget, VideoDecoderThreadConfig};
pub use control::{
    DecoderResourceSnapshot, HostUploadBackpressureReason, HostUploadResourceSnapshot,
    HostUploadResourceSnapshotStatus, VideoDecoderControlChannelPressureSnapshot,
};
pub use protocol::{
    DecodeBackpressureReason, DecodePacket, DecodeSendError, DecodeThreadError,
    VideoDecoderControlBackpressureReason, VideoDecoderEndOfStreamDrainResult,
    VideoDecoderEndOfStreamDrainState, VideoPrerollOutputFloor, VideoPrerollOutputFloorClear,
    VideoPrerollOutputFloorResult, VideoStreamConfigRejection, VideoStreamConfigResult,
    VideoStreamDecodeConfig, VideoStreamPacketization,
};

pub trait VideoDecoderThreadHandle: Send + Sync {
    /// Renderer/resource provider, который decoder отдаёт владельцу presentation path.
    type ResourceProvider: Clone + Send + Sync + 'static;

    /// Возвращает человекочитаемое имя backend-а для snapshot/diagnostics.
    fn backend_name(&self) -> &'static str;

    /// Отправляет encoded packet в decoder thread.
    fn send_packet(&self, packet: DecodePacket) -> Result<(), DecodeSendError>;

    /// Настраивает codec-specific stream state без изменения seek generation или pending queues.
    fn configure_stream(&self, _config: VideoStreamDecodeConfig) -> VideoStreamConfigResult {
        VideoStreamConfigResult::Unsupported(VideoStreamConfigRejection::BackendUnsupported {
            reason: format!(
                "{} decoder handle does not implement stream configuration",
                self.backend_name()
            ),
        })
    }

    /// Очищает codec-specific stream state при media switch/backend lifecycle reset.
    fn clear_stream(&self) -> VideoStreamConfigResult {
        VideoStreamConfigResult::Unchanged
    }

    /// Устанавливает decoder-side output floor для accurate seek preroll.
    fn set_preroll_output_floor(
        &self,
        _floor: VideoPrerollOutputFloor,
    ) -> VideoPrerollOutputFloorResult {
        VideoPrerollOutputFloorResult::Unsupported
    }

    /// Очищает decoder-side output floor без изменения seek generation.
    fn clear_preroll_output_floor(
        &self,
        _clear: VideoPrerollOutputFloorClear,
    ) -> VideoPrerollOutputFloorResult {
        VideoPrerollOutputFloorResult::Unchanged
    }

    /// Запускает explicit EOF/DPB drain отдельно от seek `flush`.
    fn begin_end_of_stream_drain(&self, generation: u64) -> VideoDecoderEndOfStreamDrainResult {
        VideoDecoderEndOfStreamDrainResult::Started(VideoDecoderEndOfStreamDrainState::Drained {
            generation,
        })
    }

    /// Возвращает текущее состояние explicit EOF/DPB drain.
    fn end_of_stream_drain_state(&self) -> VideoDecoderEndOfStreamDrainState {
        VideoDecoderEndOfStreamDrainState::Idle
    }

    /// Освобождает texture/surface handle после presentation/drop.
    fn release_frame(&self, handle: crate::FrameResourceHandle);

    /// Забирает следующий decoded frame без блокировки worker-а.
    fn try_recv_frame(&self) -> Option<crate::DecodedFrame>;

    /// Забирает backend diagnostics event без блокировки worker-а.
    fn try_recv_diagnostic_event(&self) -> Option<crate::VideoDecoderDiagnosticEvent>;

    /// Забирает fatal decoder-thread error, если backend остановился.
    fn try_recv_error(&self) -> Option<DecodeThreadError>;

    /// Сбрасывает decoder state перед seek transaction.
    fn flush(&self) -> anyhow::Result<()>;

    /// Возвращает provider для renderer-side resource lookup/release path.
    fn resource_provider(&self) -> Self::ResourceProvider;

    /// Возвращает snapshot texture/resource pool-а для UI/backpressure diagnostics.
    fn decoder_resource_snapshot(&self) -> Option<DecoderResourceSnapshot>;

    /// Возвращает typed snapshot software host-upload ресурсов.
    fn host_upload_resource_snapshot(&self) -> HostUploadResourceSnapshotStatus {
        HostUploadResourceSnapshotStatus::UnsupportedBackend
    }

    /// Возвращает snapshot bounded control channel-а для diagnostics.
    fn decoder_control_channel_pressure(
        &self,
    ) -> Option<VideoDecoderControlChannelPressureSnapshot> {
        None
    }

    /// Возвращает snapshot нейтрального activity notifier-а для event-driven waits.
    fn decoder_activity_snapshot(&self) -> VideoDecoderActivitySnapshot {
        VideoDecoderActivitySnapshot::unsupported()
    }

    /// Возвращает глубину packet channel-а внутри decoder thread.
    fn packet_queue_depth(&self) -> usize;

    /// Забирает количество packets, обработанных decoder thread-ом.
    fn drain_completed_packet_count(&self) -> usize;
}

#[cfg(test)]
mod tests;
