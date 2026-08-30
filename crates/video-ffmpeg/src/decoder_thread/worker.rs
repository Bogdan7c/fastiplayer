//! Orchestration рабочего FFmpeg decoder thread.
//!
//! Родительский модуль владеет playback-facing handle, bounded channels и
//! spawn/lifecycle construction. Этот приватный модуль владеет только owner-loop:
//! control/configure/flush/EOF-drain, публикацией кадров и completion accounting.

use codec_core::VideoColorMetadata;
use crossbeam_channel::{Receiver, TryRecvError, TrySendError};
use video_backend_api::PresentFrameResourceProvider;
use video_core::{
    DecodePacket, DecodedFrame, VideoDecoderEndOfStreamDrainResult,
    VideoDecoderEndOfStreamDrainState, VideoFrameDiagnostics, VideoStreamConfigResult,
    VideoStreamDecodeConfig,
};

use super::host_resources::invalid_avframe_resource;
use super::send_receive::{
    DecodedFrameRecord, RealFfmpegDecodeApi, SendPacketOutcome, SendReceiveDecodeLoop,
};
use super::{
    ConfiguredFfmpegDecoder, FfmpegDecoderControl, FfmpegDecoderThreadError, FfmpegDecoderWorker,
    FfmpegOpenDecoderError, decode_thread_error_from_ffmpeg,
    eof_drain_result_requires_owner_reentry, set_eof_drain_state,
};

impl FfmpegDecoderWorker {
    /// Запускает единственный owner-loop codec context-а до terminal shutdown/disconnect.
    pub(super) fn run(
        mut self,
        packet_rx: Receiver<DecodePacket>,
        control_rx: Receiver<FfmpegDecoderControl>,
        shutdown_rx: Receiver<()>,
    ) {
        loop {
            // Проверяем shutdown до остальных ready operations: disconnected
            // control receiver не должен даже теоретически вытеснять teardown.
            match shutdown_rx.try_recv() {
                Ok(()) | Err(TryRecvError::Disconnected) => break,
                Err(TryRecvError::Empty) => {}
            }

            // Host-upload pool — единственный hard-источник backpressure: пока в
            // нём нет свободных slots, мы не забираем новые packet-ы и не
            // дренируем кадры (они ждут внутри FFmpeg), иначе таблица
            // переполнится fatal-ом. Control обслуживается всегда; release pool
            // slot-а будит reception через release_notify_rx.
            if self.resource_provider.free_slots() == 0 {
                #[cfg(test)]
                self.publish_full_pool_wait_entry();
                crossbeam_channel::select! {
                    recv(shutdown_rx) -> _ => break,
                    recv(control_rx) -> control_message => {
                        match control_message {
                            Ok(control) => self.handle_control(control, &packet_rx),
                            // Единственный control sender принадлежит frontend;
                            // disconnect поэтому является terminal lifecycle signal.
                            Err(_) => break,
                        }
                    }
                    recv(self.release_notify_rx) -> _ => {}
                }
                continue;
            }

            // Pool освободился: сначала дотолкаем отложенный packet, чтобы не
            // нарушить порядок и слить буферизованные внутри FFmpeg кадры.
            if let Some(packet) = self.pending_packet.take() {
                self.handle_packet(packet);
                continue;
            }

            if let Some(generation) = self.pending_eof_drain_generation {
                let drain_result = self.drive_end_of_stream_drain(generation);
                if eof_drain_result_requires_owner_reentry(&drain_result) {
                    // Не засыпаем в select только на packet/control: после EOF
                    // их больше не будет. Верхняя pool-проверка сама дождётся
                    // release, если slots закончились; уже свободные slots
                    // используются сразу, чтобы не потерять coalesced release
                    // edge перед terminal AVERROR_EOF.
                    continue;
                }
            }

            crossbeam_channel::select! {
                recv(shutdown_rx) -> _ => break,
                recv(control_rx) -> control_message => {
                    match control_message {
                        Ok(control) => self.handle_control(control, &packet_rx),
                        // Queued packets старого frontend-а не продлевают worker lifecycle.
                        Err(_) => break,
                    }
                }
                recv(packet_rx) -> packet_message => {
                    match packet_message {
                        Ok(packet) => self.handle_packet(packet),
                        Err(_) if control_rx.is_empty() => break,
                        Err(_) => {}
                    }
                }
            }
        }
    }

    /// Публикует test-only causal ack прямо перед блокировкой в full-pool select.
    #[cfg(test)]
    fn publish_full_pool_wait_entry(&self) {
        // Full означает уже опубликованный coalesced ack, а disconnected observer
        // больше никого не может уведомить; ни один исход не меняет worker lifecycle.
        let _test_observation_outcome = self.full_pool_wait_observer_tx.try_send(());
    }

    fn handle_control(
        &mut self,
        control: FfmpegDecoderControl,
        packet_rx: &Receiver<DecodePacket>,
    ) {
        match control {
            FfmpegDecoderControl::Configure { config, reply_tx } => {
                // Любой отложенный packet принадлежит прошлой конфигурации.
                self.pending_packet = None;
                self.pending_eof_drain_generation = None;
                let result = self.configure_stream(config);
                let _ = reply_tx.try_send(result);
            }
            FfmpegDecoderControl::Clear { reply_tx } => {
                self.pending_packet = None;
                self.pending_eof_drain_generation = None;
                let result = self.clear_stream();
                let _ = reply_tx.try_send(result);
            }
            FfmpegDecoderControl::Flush { reply_tx } => {
                // Seek flush сбрасывает очередь packet-ов; pending packet тоже
                // относится к до-seek потоку и должен быть отброшен.
                self.pending_packet = None;
                self.pending_eof_drain_generation = None;
                self.drop_queued_packets_after_seek_flush(packet_rx);
                let result = self
                    .flush_for_seek()
                    .map_err(decode_thread_error_from_ffmpeg);
                let _ = reply_tx.try_send(result);
            }
            FfmpegDecoderControl::BeginEndOfStreamDrain {
                generation,
                reply_tx,
            } => {
                let result = self.begin_end_of_stream_drain(generation);
                let _ = reply_tx.try_send(result);
            }
        }
    }

    fn configure_stream(&mut self, config: VideoStreamDecodeConfig) -> VideoStreamConfigResult {
        if self
            .active_decoder
            .as_ref()
            .is_some_and(|active_decoder| active_decoder.config == config)
        {
            return VideoStreamConfigResult::Unchanged;
        }

        let codec_api = match RealFfmpegDecodeApi::open(&config, self.software_decode_thread_budget)
        {
            Ok(codec_api) => codec_api,
            Err(FfmpegOpenDecoderError::Unsupported(rejection)) => {
                return VideoStreamConfigResult::Unsupported(rejection);
            }
            Err(FfmpegOpenDecoderError::Fatal(error)) => {
                return VideoStreamConfigResult::Fatal(decode_thread_error_from_ffmpeg(error));
            }
        };

        let decode_loop = SendReceiveDecodeLoop::new(
            codec_api,
            self.activity_notifier.clone(),
            self.eof_drain_state.clone(),
        );

        self.active_decoder = Some(ConfiguredFfmpegDecoder {
            config,
            decode_loop,
        });
        let _ = self.activity_notifier.notify_activity();

        VideoStreamConfigResult::Configured
    }

    fn clear_stream(&mut self) -> VideoStreamConfigResult {
        if self.active_decoder.take().is_none() {
            return VideoStreamConfigResult::Unchanged;
        }

        if let Err(error) = set_eof_drain_state(
            &self.eof_drain_state,
            VideoDecoderEndOfStreamDrainState::Idle,
            &self.activity_notifier,
        ) {
            return VideoStreamConfigResult::Fatal(decode_thread_error_from_ffmpeg(error));
        }

        let _ = self.activity_notifier.notify_activity();

        VideoStreamConfigResult::Cleared
    }

    fn flush_for_seek(&mut self) -> Result<(), FfmpegDecoderThreadError> {
        if let Some(active_decoder) = self.active_decoder.as_mut() {
            active_decoder.decode_loop.flush_for_seek()?;
        } else {
            set_eof_drain_state(
                &self.eof_drain_state,
                VideoDecoderEndOfStreamDrainState::Idle,
                &self.activity_notifier,
            )?;
        }

        let _ = self.activity_notifier.notify_activity();
        Ok(())
    }

    fn begin_end_of_stream_drain(&mut self, generation: u64) -> VideoDecoderEndOfStreamDrainResult {
        let current_state = self
            .active_decoder
            .as_ref()
            .map(|decoder| decoder.decode_loop.end_of_stream_drain_state());
        if matches!(
            current_state,
            Some(VideoDecoderEndOfStreamDrainState::Draining {
                generation: active_generation,
            } | VideoDecoderEndOfStreamDrainState::Drained {
                generation: active_generation,
            }) if active_generation == generation
        ) {
            return VideoDecoderEndOfStreamDrainResult::Unchanged(
                current_state.expect("matched state is present"),
            );
        }

        self.drive_end_of_stream_drain(generation)
    }

    /// Делает один bounded receive-pass; run-loop повторит его после release notification.
    fn drive_end_of_stream_drain(&mut self, generation: u64) -> VideoDecoderEndOfStreamDrainResult {
        let drain_result = {
            let Some(active_decoder) = self.active_decoder.as_mut() else {
                let drained = VideoDecoderEndOfStreamDrainState::Drained { generation };
                if let Err(error) = set_eof_drain_state(
                    &self.eof_drain_state,
                    drained.clone(),
                    &self.activity_notifier,
                ) {
                    return VideoDecoderEndOfStreamDrainResult::Fatal(
                        decode_thread_error_from_ffmpeg(error),
                    );
                }
                self.pending_eof_drain_generation = None;
                return VideoDecoderEndOfStreamDrainResult::Started(drained);
            };

            let receive_budget = self.resource_provider.free_slots();
            let config = active_decoder.config.clone();
            active_decoder
                .decode_loop
                .begin_end_of_stream_drain(generation, receive_budget)
                .map(|progress_report| (config, progress_report))
        };

        match drain_result {
            Ok((config, progress_report)) => {
                if let Err(error) = self.publish_decoded_frames(&config, progress_report.frames) {
                    let thread_error = decode_thread_error_from_ffmpeg(error.clone());
                    let _ = set_eof_drain_state(
                        &self.eof_drain_state,
                        VideoDecoderEndOfStreamDrainState::Fatal {
                            generation: Some(generation),
                            error: thread_error.clone(),
                        },
                        &self.activity_notifier,
                    );
                    self.pending_eof_drain_generation = None;
                    VideoDecoderEndOfStreamDrainResult::Fatal(thread_error)
                } else {
                    self.pending_eof_drain_generation = matches!(
                        progress_report.state,
                        VideoDecoderEndOfStreamDrainState::Draining { .. }
                    )
                    .then_some(generation);
                    VideoDecoderEndOfStreamDrainResult::Started(progress_report.state)
                }
            }
            Err(error) => {
                let thread_error = decode_thread_error_from_ffmpeg(error.clone());
                let _ = set_eof_drain_state(
                    &self.eof_drain_state,
                    VideoDecoderEndOfStreamDrainState::Fatal {
                        generation: Some(generation),
                        error: thread_error.clone(),
                    },
                    &self.activity_notifier,
                );
                self.pending_eof_drain_generation = None;
                VideoDecoderEndOfStreamDrainResult::Fatal(thread_error)
            }
        }
    }

    fn handle_packet(&mut self, packet: DecodePacket) {
        // Receive budget = свободные pool slots: decode loop не произведёт больше
        // кадров, чем влезет в таблицу. Если pool полон до приёма packet-а,
        // packet откладывается в pending_packet и повторяется после release.
        let receive_budget = self.resource_provider.free_slots();

        let decode_result = {
            let Some(active_decoder) = self.active_decoder.as_mut() else {
                self.report_fatal_error(FfmpegDecoderThreadError::DecoderNotConfigured);
                return;
            };

            let config = active_decoder.config.clone();
            active_decoder
                .decode_loop
                .send_packet(packet, receive_budget)
                .map(|outcome| (config, outcome))
        };

        match decode_result {
            Ok((config, SendPacketOutcome::Consumed(progress_report))) => {
                if let Err(error) = self.publish_decoded_frames(&config, progress_report.frames) {
                    self.report_fatal_error(error);
                    return;
                }

                if progress_report.packet_completed {
                    self.packet_completion_counter.record_completion();
                }
                let _ = self.activity_notifier.notify_activity();
            }
            Ok((config, SendPacketOutcome::Deferred { progress, packet })) => {
                if let Err(error) = self.publish_decoded_frames(&config, progress.frames) {
                    self.report_fatal_error(error);
                    return;
                }

                self.pending_packet = Some(packet);
                let _ = self.activity_notifier.notify_activity();
            }
            Err(error) => self.report_fatal_error(error),
        }
    }

    fn publish_decoded_frames(
        &self,
        config: &VideoStreamDecodeConfig,
        frames: Vec<DecodedFrameRecord>,
    ) -> Result<(), FfmpegDecoderThreadError> {
        for frame_record in frames {
            let decoded_frame = self.decoded_frame_from_record(config, frame_record)?;
            let resource_handle = decoded_frame.resource_handle;

            match self.frame_tx.try_send(decoded_frame) {
                Ok(()) => {}
                Err(TrySendError::Full(_)) => {
                    self.resource_provider.release_frame(resource_handle);
                    self.resource_provider.record_upload_failure();
                    return Err(FfmpegDecoderThreadError::ProtocolViolation {
                        reason: format!(
                            "FFmpeg decoded-frame channel is full: {}/{} frames queued",
                            self.frame_tx.len(),
                            self.frame_tx.capacity().unwrap_or(0)
                        ),
                    });
                }
                Err(TrySendError::Disconnected(_)) => {
                    self.resource_provider.release_frame(resource_handle);
                    self.resource_provider.record_upload_failure();
                    return Err(FfmpegDecoderThreadError::ProtocolViolation {
                        reason: "FFmpeg decoded-frame channel is disconnected".to_owned(),
                    });
                }
            }
        }

        Ok(())
    }

    fn decoded_frame_from_record(
        &self,
        config: &VideoStreamDecodeConfig,
        frame_record: DecodedFrameRecord,
    ) -> Result<DecodedFrame, FfmpegDecoderThreadError> {
        let frame_ref =
            frame_record
                .frame_ref
                .ok_or_else(|| FfmpegDecoderThreadError::ProtocolViolation {
                    reason: "FFmpeg receive loop produced metadata without an AVFrame reference"
                        .to_owned(),
                })?;
        let color = frame_record
            .color
            .unwrap_or_else(VideoColorMetadata::sdr_bt709_limited);
        let publication = self.resource_provider.insert_frame(
            frame_record.generation,
            frame_ref,
            config.frame_contract,
        )?;

        let decoded_frame = DecodedFrame {
            generation: frame_record.generation,
            pts: frame_record.pts,
            frame_contract: config.frame_contract,
            width: publication.width,
            height: publication.height,
            render_width: publication.width,
            render_height: publication.height,
            display_orientation: config.display_orientation,
            color,
            resource_handle: publication.handle,
            diagnostics: VideoFrameDiagnostics::default(),
        };

        decoded_frame
            .validate_against_expected_contract(config.frame_contract)
            .map_err(|error| {
                invalid_avframe_resource("FFmpeg decoded frame validation", error.to_string())
            })?;

        Ok(decoded_frame)
    }

    fn drop_queued_packets_after_seek_flush(&self, packet_rx: &Receiver<DecodePacket>) {
        while packet_rx.try_recv().is_ok() {}
    }

    fn report_fatal_error(&self, error: FfmpegDecoderThreadError) {
        let thread_error = decode_thread_error_from_ffmpeg(error);
        let _ = self.error_tx.try_send(thread_error);
        let _ = self.activity_notifier.notify_activity();
    }
}
