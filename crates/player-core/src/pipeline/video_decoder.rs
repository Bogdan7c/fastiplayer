use super::*;

impl PlaybackPipeline {
    /// Сохраняет запущенный video backend без раскрытия backend-specific init в session.
    #[cfg(test)]
    pub(crate) fn set_video_decoder_thread(
        &mut self,
        decoder_thread: impl VideoDecoderThreadHandle<
            ResourceProvider = PresentFrameResourceProviderHandle,
        > + 'static,
    ) {
        self.set_video_decoder_thread_handle(Box::new(decoder_thread));
    }

    #[cfg(test)]
    pub(crate) fn set_video_decoder_thread_handle(
        &mut self,
        decoder_thread: Box<PlayerVideoDecoderThreadHandle>,
    ) {
        let _previous_decoder = self.replace_video_decoder_thread_handle(decoder_thread);
    }

    /// Атомарно заменяет decoder handle и возвращает прежний для compensating rollback.
    pub(crate) fn replace_video_decoder_thread_handle(
        &mut self,
        decoder_thread: Box<PlayerVideoDecoderThreadHandle>,
    ) -> Option<Box<PlayerVideoDecoderThreadHandle>> {
        self.cancel_video_backlog_recovery_scan_for_decoder_replacement();
        self.video_backend = decoder_thread.backend_name();
        let previous_decoder = self.video_decoder_thread.replace(decoder_thread);
        self.reset_video_decode_in_flight();
        previous_decoder
    }

    /// Проверяет, есть ли active decoder для операций presentation/render handoff.
    #[must_use]
    pub(crate) fn has_active_video_decoder(&self) -> bool {
        self.video_decoder_thread.is_some()
    }

    /// Возвращает имя текущего video backend-а без раскрытия runtime slot-а.
    #[must_use]
    pub(crate) const fn video_backend_name(&self) -> &'static str {
        self.video_backend
    }

    /// Проверяет, можно ли отправлять encoded packets через decoder I/O boundary.
    ///
    /// Tick-код использует этот метод как send-side readiness и не зависит от
    /// того, каким полем pipeline владеет активным decoder backend-ом.
    // Send-side readiness boundary; сейчас зафиксирован только focused tests
    // (decoder_boundary), поэтому в non-test сборке метод не имеет вызовов.
    #[allow(dead_code)]
    #[must_use]
    pub(crate) fn can_send_video_decode_packets(&self) -> bool {
        self.video_decoder_send_backpressure(usize::MAX).is_none()
    }

    /// Возвращает typed причину send-side backpressure без доступа к backend internals.
    ///
    /// `UnsupportedBackend` у host-upload snapshot-а означает hardware/old backend и
    /// оставляет старую VA-API surface accounting ветку активной. `AbsentResource`
    /// остаётся отличимым через `host_upload_resource_snapshot()` и не маскируется
    /// под свободные upload slots.
    #[must_use]
    pub(crate) fn video_decoder_send_backpressure(
        &self,
        host_upload_ready_queue_capacity: usize,
    ) -> Option<VideoDecoderSendBackpressure> {
        if self.video_decoder_thread.is_none() {
            return Some(VideoDecoderSendBackpressure::AbsentDecoder);
        }

        match self.host_upload_resource_snapshot() {
            HostUploadResourceSnapshotStatus::Available(snapshot) => {
                if let Some(reason) = self.decoder_control_backpressure_reason() {
                    return Some(VideoDecoderSendBackpressure::DecoderControl(reason));
                }

                snapshot
                    .backpressure_reason(host_upload_ready_queue_capacity)
                    .map(VideoDecoderSendBackpressure::HostUpload)
            }
            HostUploadResourceSnapshotStatus::AbsentDecoder => {
                Some(VideoDecoderSendBackpressure::AbsentDecoder)
            }
            HostUploadResourceSnapshotStatus::AbsentResource
            | HostUploadResourceSnapshotStatus::UnsupportedBackend => None,
        }
    }

    /// Проверяет, можно ли принимать decoded frames через decoder I/O boundary.
    ///
    /// Receive-side readiness сейчас совпадает с наличием active backend-а, но
    /// call sites больше не читают внутреннее устройство decoder thread-а.
    #[must_use]
    pub(crate) fn can_receive_decoded_video_frames(&self) -> bool {
        self.has_active_video_decoder()
    }

    /// Возвращает глубину send queue decoder thread-а, если backend запущен.
    #[must_use]
    pub(crate) fn video_decoder_packet_queue_depth(&self) -> Option<usize> {
        self.video_decoder_thread
            .as_ref()
            .map(|decoder_thread| decoder_thread.packet_queue_depth())
    }

    /// Возвращает snapshot texture pool-а, не раскрывая decoder thread наружу.
    #[must_use]
    pub(crate) fn video_decoder_resource_snapshot(&self) -> Option<DecoderResourceSnapshot> {
        self.video_decoder_thread
            .as_ref()
            .and_then(|decoder_thread| decoder_thread.decoder_resource_snapshot())
    }

    /// Возвращает typed snapshot software host-upload ресурсов.
    // S20 подключит этот boundary к scheduler/tick; сейчас его фиксируют focused tests.
    #[must_use]
    pub(crate) fn host_upload_resource_snapshot(&self) -> HostUploadResourceSnapshotStatus {
        let Some(decoder_thread) = self.video_decoder_thread.as_ref() else {
            return HostUploadResourceSnapshotStatus::AbsentDecoder;
        };

        decoder_thread.host_upload_resource_snapshot()
    }

    /// Возвращает pressure snapshot decoder control channel-а, если backend его поддерживает.
    pub(crate) fn video_decoder_control_channel_pressure(
        &self,
    ) -> Option<DecoderControlChannelPressureSnapshot> {
        self.video_decoder_thread
            .as_ref()
            .and_then(|decoder_thread| decoder_thread.decoder_control_channel_pressure())
    }

    /// Возвращает typed control-channel backpressure, если neutral snapshot показывает full queue.
    #[must_use]
    pub(crate) fn decoder_control_backpressure_reason(
        &self,
    ) -> Option<VideoDecoderControlBackpressureReason> {
        let pressure = self.video_decoder_control_channel_pressure()?;
        if pressure.control_channel_capacity == 0
            || pressure.control_channel_len < pressure.control_channel_capacity
        {
            return None;
        }

        Some(VideoDecoderControlBackpressureReason::ControlChannelFull {
            queued_messages: pressure.control_channel_len,
            capacity: pressure.control_channel_capacity,
        })
    }

    /// Возвращает typed status neutral decoder activity boundary-а.
    ///
    /// Planner/worker видят только намерение и typed unavailable reason, а не
    /// concrete decoder thread storage или backend-specific channels.
    #[must_use]
    pub(crate) fn video_decoder_activity_status(&self) -> VideoDecoderActivityStatus {
        let Some(decoder_thread) = self.video_decoder_thread.as_ref() else {
            return VideoDecoderActivityStatus::AbsentDecoder;
        };

        match decoder_thread.decoder_activity_snapshot() {
            VideoDecoderActivitySnapshot::Available {
                captured_epoch,
                subscription,
            } => VideoDecoderActivityStatus::Available {
                snapshot: VideoDecoderActivitySnapshot::Available {
                    captured_epoch,
                    subscription,
                },
            },
            VideoDecoderActivitySnapshot::Unavailable {
                reason: VideoDecoderActivityUnavailableReason::UnsupportedNotifier,
            } => VideoDecoderActivityStatus::Unsupported,
            VideoDecoderActivitySnapshot::Unavailable { reason } => {
                VideoDecoderActivityStatus::Unavailable(reason)
            }
        }
    }

    /// Возвращает renderer-neutral provider активного decoder thread-а.
    #[must_use]
    pub(crate) fn video_decoder_resource_provider(
        &self,
    ) -> Option<PresentFrameResourceProviderHandle> {
        self.video_decoder_thread
            .as_ref()
            .map(|decoder_thread| decoder_thread.resource_provider())
    }

    /// Немедленно отдаёт texture slot активному decoder thread-у.
    ///
    /// Метод намеренно не знает про deferred render leases: это решение остаётся
    /// в `PlayerSession`, потому что только session видит поколение renderer-а.
    pub(crate) fn release_frame_to_video_decoder(
        &self,
        resource_handle: video_core::FrameResourceHandle,
    ) -> bool {
        let Some(decoder_thread) = self.video_decoder_thread.as_ref() else {
            return false;
        };

        decoder_thread.release_frame(resource_handle);
        true
    }

    /// Конфигурирует active decoder stream без seek flush/generation side effects.
    pub(crate) fn configure_video_decoder_stream(
        &self,
        config: VideoStreamDecodeConfig,
    ) -> VideoStreamConfigResult {
        let Some(decoder_thread) = self.video_decoder_thread.as_ref() else {
            return VideoStreamConfigResult::AbsentDecoder;
        };

        decoder_thread.configure_stream(config)
    }

    /// Очищает stream config активного decoder-а как отдельный media lifecycle step.
    pub(crate) fn clear_video_decoder_stream(&self) -> VideoStreamConfigResult {
        let Some(decoder_thread) = self.video_decoder_thread.as_ref() else {
            return VideoStreamConfigResult::AbsentDecoder;
        };

        decoder_thread.clear_stream()
    }

    /// Устанавливает decoder-side floor для Accurate preroll без знания concrete backend-а.
    pub(crate) fn set_video_decoder_preroll_output_floor(
        &self,
        floor: VideoPrerollOutputFloor,
    ) -> VideoPrerollOutputFloorResult {
        let Some(decoder_thread) = self.video_decoder_thread.as_ref() else {
            return VideoPrerollOutputFloorResult::AbsentDecoder;
        };

        decoder_thread.set_preroll_output_floor(floor)
    }

    /// Очищает decoder-side Accurate preroll floor через нейтральный decoder boundary.
    pub(crate) fn clear_video_decoder_preroll_output_floor(
        &self,
        clear: VideoPrerollOutputFloorClear,
    ) -> VideoPrerollOutputFloorResult {
        let Some(decoder_thread) = self.video_decoder_thread.as_ref() else {
            return VideoPrerollOutputFloorResult::AbsentDecoder;
        };

        decoder_thread.clear_preroll_output_floor(clear)
    }

    /// Запускает decoder EOF/DPB drain без превращения его в seek flush.
    pub(crate) fn begin_video_decoder_end_of_stream_drain(
        &self,
        generation: u64,
    ) -> VideoDecoderEndOfStreamDrainResult {
        let Some(decoder_thread) = self.video_decoder_thread.as_ref() else {
            return VideoDecoderEndOfStreamDrainResult::AbsentDecoder;
        };

        decoder_thread.begin_end_of_stream_drain(generation)
    }

    /// Возвращает explicit decoder EOF/DPB drain state без чтения decoder storage.
    pub(crate) fn video_decoder_end_of_stream_drain_state(
        &self,
    ) -> VideoDecoderEndOfStreamDrainState {
        self.video_decoder_thread
            .as_ref()
            .map(|decoder_thread| decoder_thread.end_of_stream_drain_state())
            .unwrap_or(VideoDecoderEndOfStreamDrainState::Idle)
    }

    /// Сбрасывает decoder thread перед seek/media reset.
    ///
    /// Отсутствующий decoder thread остаётся успешным no-op, как и прежний
    /// прямой вызов из session.
    pub(crate) fn flush_video_decoder_thread(&self) -> anyhow::Result<()> {
        let Some(decoder_thread) = self.video_decoder_thread.as_ref() else {
            return Ok(());
        };

        decoder_thread.flush()
    }

    /// Забирает один decoded frame без блокировки worker-а.
    pub(crate) fn try_recv_decoded_video_frame(&self) -> Option<video_core::DecodedFrame> {
        self.video_decoder_thread
            .as_ref()
            .and_then(|decoder_thread| decoder_thread.try_recv_frame())
    }

    /// Забирает один diagnostics event от decoder/backend boundary.
    pub(crate) fn try_recv_video_decoder_diagnostic_event(
        &self,
    ) -> Option<video_core::VideoDecoderDiagnosticEvent> {
        self.video_decoder_thread
            .as_ref()
            .and_then(|decoder_thread| decoder_thread.try_recv_diagnostic_event())
    }

    /// Забирает один fatal decoder-thread error, если backend уже остановился.
    pub(crate) fn try_recv_video_decoder_error(&self) -> Option<DecodeThreadError> {
        self.video_decoder_thread
            .as_ref()
            .and_then(|decoder_thread| decoder_thread.try_recv_error())
    }

    /// Забирает packet ack-и decoder thread-а без изменения player-side accounting.
    #[must_use]
    pub(crate) fn drain_completed_video_decode_packet_count(&self) -> usize {
        self.video_decoder_thread
            .as_ref()
            .map(|decoder_thread| decoder_thread.drain_completed_packet_count())
            .unwrap_or(0)
    }

    /// Отправляет encoded packet в активный decoder thread.
    ///
    /// `None` означает, что decoder thread отсутствует. `Some(Ok(()))`
    /// означает принятую отправку, а `Some(Err(_))` сохраняет различие между
    /// backpressure и fatal send failure.
    pub(crate) fn send_video_decode_packet(
        &self,
        packet: PlayerDecodePacket,
    ) -> Option<Result<(), DecodeSendError>> {
        self.video_decoder_thread
            .as_ref()
            .map(|decoder_thread| decoder_thread.send_packet(packet))
    }

    /// Сбрасывает счётчик packets, которые могли остаться внутри decoder после flush/seek.
    pub(crate) fn reset_video_decode_in_flight(&mut self) {
        self.video_decode_in_flight_packets = 0;
    }

    /// Отмечает packet, успешно переданный через worker -> decoder boundary.
    pub(crate) fn note_video_packet_sent_to_decoder(&mut self) {
        self.video_decode_in_flight_packets = self.video_decode_in_flight_packets.saturating_add(1);
    }

    /// Отмечает packets, которые decoder thread обработал без привязки к числу output frames.
    pub(crate) fn note_video_packets_completed_by_decoder(&mut self, packet_count: usize) {
        self.video_decode_in_flight_packets = self
            .video_decode_in_flight_packets
            .saturating_sub(packet_count);
    }

    /// Возвращает приблизительное число packets, которые decoder уже забрал, но ещё не ack-нул.
    #[must_use]
    pub(crate) const fn video_decode_in_flight_packets(&self) -> usize {
        self.video_decode_in_flight_packets
    }
}
