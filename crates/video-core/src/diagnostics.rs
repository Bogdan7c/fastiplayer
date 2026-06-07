use std::time::Duration;

/// Codec-neutral timings одного decoded video frame.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct VideoFrameTimingDiagnostics {
    /// Время ожидания packet-а в bounded decoder packet channel.
    pub decoder_packet_receive_latency: Option<Duration>,

    /// Время submit-а bitstream packet-а в hardware decoder.
    pub decoder_submit_latency: Option<Duration>,

    /// Время обработки decoder events после submit-а.
    pub decoder_event_drain_latency: Option<Duration>,

    /// Время ожидания готовности hardware-decoded surface.
    pub hardware_sync_latency: Option<Duration>,

    /// Время экспорта decoded surface в external memory descriptor.
    pub dma_buf_export_latency: Option<Duration>,

    /// Время импорта external memory в renderer-visible GPU texture.
    pub dma_buf_import_latency: Option<Duration>,

    /// Время публикации decoded frame через bounded frame channel.
    pub decoded_frame_publish_latency: Option<Duration>,
}

/// Codec-neutral pressure resource/surface pool-а рядом с кадром.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct VideoResourcePoolDiagnostics {
    /// Максимальное количество слотов, разрешённое backend pool-ом.
    pub capacity: usize,

    /// Количество физически созданных slots.
    pub slots: usize,

    /// Количество slots, удерживаемых live кадрами.
    pub in_use: usize,

    /// Количество persistent imports, доступных для reuse.
    pub free_surfaces: usize,

    /// Количество releases, ожидающих GPU completion.
    pub waiting_gpu_completion: usize,

    /// Количество releases, ожидающих возврата decoded surface decoder-у.
    pub waiting_decoder_reuse: usize,

    /// Количество failed external imports.
    pub import_failures: u64,

    /// Количество созданных external imports.
    pub imports_created: u64,

    /// Количество reuse hits без нового external import-а.
    pub imports_reused: u64,

    /// Количество replacements free import-а после смены descriptor/object identity.
    pub imports_replaced: u64,
}

impl VideoResourcePoolDiagnostics {
    /// Возвращает доступный запас slots без underflow.
    #[must_use]
    pub const fn available_slots(self) -> usize {
        self.capacity.saturating_sub(self.in_use)
    }
}

/// Diagnostics payload, который backend может прикрепить к decoded frame.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct VideoFrameDiagnostics {
    /// Timings стадий decode/export/import для этого кадра.
    pub timings: VideoFrameTimingDiagnostics,

    /// Глубина backend ready queue после enqueue этого кадра.
    pub decoder_ready_queue_depth: Option<usize>,

    /// Resource/surface pressure после zero-copy export этого кадра.
    pub resource_pool: Option<VideoResourcePoolDiagnostics>,
}

/// Причина frame drop-а внутри decoder/backend boundary.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum VideoDecoderDropReason {
    /// Backend ready queue вытеснила старый decoded frame.
    ReadyQueueOverflow,
}

/// Накопительная диагностика давления на публикацию decoded frames.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct VideoFramePublishPressureDiagnostics {
    /// Сколько раз bounded decoder->worker frame channel был заполнен.
    pub frame_publish_channel_full_count: u64,

    /// Максимальная latency publish stage среди успешно опубликованных кадров.
    pub max_decoded_frame_publish_latency: Duration,

    /// Суммарная latency publish stage среди успешно опубликованных кадров.
    pub total_decoded_frame_publish_latency: Duration,

    /// Сколько повторных publish attempts сделано для уже pending frame.
    pub pending_publish_retry_count: u64,
}

/// Typed diagnostics event от video backend-а в player-core.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum VideoDecoderDiagnosticEvent {
    /// Decoder/backend удалил frame до передачи в player scheduler.
    FrameDropped {
        /// Presentation timestamp удалённого кадра.
        pts: Duration,

        /// Backend-local typed причина удаления.
        reason: VideoDecoderDropReason,
    },

    /// Accurate seek preroll подавил frame ниже decoder-side output floor.
    SeekPrerollFrameSuppressed {
        /// Presentation timestamp подавленного preroll frame-а.
        pts: Duration,

        /// Seek generation, к которой относится active floor.
        generation: u64,

        /// Минимальный presentation timestamp, разрешённый для публикации.
        floor_pts: Duration,
    },

    /// Decoder thread встретил pressure на bounded decoded-frame publish channel.
    DecodedFramePublishPressure {
        /// Накопительные counters publish boundary.
        pressure: VideoFramePublishPressureDiagnostics,
    },
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Проверяет, что seek preroll diagnostic остаётся Copy/Eq и не теряет payload.
    #[test]
    fn seek_preroll_frame_suppressed_event_is_copy_eq_and_carries_payload() {
        let event = VideoDecoderDiagnosticEvent::SeekPrerollFrameSuppressed {
            pts: Duration::from_millis(900),
            generation: 12,
            floor_pts: Duration::from_secs(1),
        };
        let copied_event = event;

        assert_eq!(event, copied_event);
        assert_eq!(
            copied_event,
            VideoDecoderDiagnosticEvent::SeekPrerollFrameSuppressed {
                pts: Duration::from_millis(900),
                generation: 12,
                floor_pts: Duration::from_secs(1),
            }
        );
    }
}
