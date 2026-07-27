use std::sync::{Arc, Mutex};
use std::time::Duration;

use codec_core::VideoColorMetadata;
use video_core::{
    DecodePacket, DecodeThreadError, VideoDecoderActivityNotifier,
    VideoDecoderEndOfStreamDrainState,
};
#[cfg(feature = "ffmpeg")]
use video_core::{SoftwareDecodeThreadBudget, VideoStreamDecodeConfig};

#[cfg(feature = "ffmpeg")]
use crate::codec_adapter::{
    FfmpegDecoderId, color_metadata_plan_from_ffmpeg_frame, plan_ffmpeg_software_decode,
};
#[cfg(feature = "ffmpeg")]
use crate::ffi::codec_context::{CodecContext, FfmpegCodecContextRequest};
#[cfg(feature = "ffmpeg")]
use crate::ffi::error::FfmpegError;
#[cfg(feature = "ffmpeg")]
use crate::ffi::error::FfmpegErrorKind;
use crate::ffi::frame::FrameTimestamps;
#[cfg(feature = "ffmpeg")]
use crate::ffi::frame::OwnedAvFrame;
#[cfg(feature = "ffmpeg")]
use crate::ffi::packet::{OwnedAvPacket, PacketTimestamps};

#[cfg(feature = "ffmpeg")]
use super::FfmpegOpenDecoderError;
use super::color_metadata::frame_color_or_context_color;
#[cfg(feature = "ffmpeg")]
use super::stream_config::{
    extradata_for_stream_config, video_decode_requirement_from_stream_config,
};
use super::{FfmpegDecoderThreadError, set_eof_drain_state};

/// FFmpeg `AV_NOPTS_VALUE` без зависимости default build-а от headers/libs.
#[cfg(any(test, feature = "ffmpeg"))]
pub(super) const NO_TIMESTAMP: i64 = i64::MIN;

/// Real FFmpeg implementation of the testable send/receive API.
#[cfg(feature = "ffmpeg")]
pub(super) struct RealFfmpegDecodeApi {
    /// Safe owner around `AVCodecContext`.
    codec_context: CodecContext,

    /// Reusable receive frame allocation.
    receive_frame: OwnedAvFrame,

    /// Last known stream time base for AVPacket timestamp conversion.
    stream_time_base: Option<StreamTimeBase>,
}

#[cfg(feature = "ffmpeg")]
impl RealFfmpegDecodeApi {
    pub(super) fn open(
        config: &VideoStreamDecodeConfig,
        software_decode_thread_budget: SoftwareDecodeThreadBudget,
    ) -> Result<Self, FfmpegOpenDecoderError> {
        let requirement = video_decode_requirement_from_stream_config(config);
        let adapter_plan = plan_ffmpeg_software_decode(&requirement, config.frame_contract)
            .map_err(|error| {
                FfmpegOpenDecoderError::Unsupported(
                    video_core::VideoStreamConfigRejection::BackendUnsupported {
                        reason: error.to_string(),
                    },
                )
            })?;
        let mut request = FfmpegCodecContextRequest::for_decoder_id(
            adapter_plan.decoder_id(),
            adapter_plan.accepted_pixel_formats().clone(),
        );
        if adapter_plan.decoder_id() == FfmpegDecoderId::Av1 {
            request = request.with_max_frame_delay(1);
        }
        request = request.with_software_decode_thread_budget(software_decode_thread_budget);
        if let Some(extradata) = extradata_for_stream_config(config)? {
            request = request.with_extradata(extradata);
        }
        let codec_context = CodecContext::open(&request)
            .map_err(FfmpegDecoderThreadError::from)
            .map_err(FfmpegOpenDecoderError::Fatal)?;
        let receive_frame = OwnedAvFrame::allocate_for_decode()
            .map_err(FfmpegDecoderThreadError::from)
            .map_err(FfmpegOpenDecoderError::Fatal)?;

        Ok(Self {
            codec_context,
            receive_frame,
            stream_time_base: None,
        })
    }
}

#[cfg(feature = "ffmpeg")]
impl SendReceiveCodecApi for RealFfmpegDecodeApi {
    type Packet = OwnedAvPacket;

    fn create_packet(
        &mut self,
        packet: &DecodePacket,
    ) -> Result<Self::Packet, FfmpegDecoderThreadError> {
        let mut av_packet = OwnedAvPacket::new(packet.encoded_bytes.as_ref())?;
        let packet_time_base = packet
            .track_dts
            .map(|track_dts| {
                StreamTimeBase::new(track_dts.time_base.numer, track_dts.time_base.denom)
            })
            .or(self.stream_time_base);
        let packet_timestamps = packet_time_base.map(|time_base| {
            let pts = duration_to_units_saturating(packet.pts, time_base);
            let dts = packet
                .track_dts
                .map(|track_dts| track_dts.units.get())
                .or_else(|| {
                    packet
                        .dts
                        .map(|dts| duration_to_units_saturating(dts, time_base))
                });

            PacketTimestamps {
                pts: Some(pts),
                dts,
                duration: None,
            }
        });

        if let Some(time_base) = packet_time_base {
            self.stream_time_base = Some(time_base);
        }

        av_packet.set_timestamps(packet_timestamps.unwrap_or_default());
        av_packet.set_keyframe(packet.keyframe);

        Ok(av_packet)
    }

    fn send_packet(&mut self, packet: &Self::Packet) -> Result<(), DecodeApiError> {
        self.codec_context
            .send_packet(packet)
            .map_err(decode_api_error_from_ffi)
    }

    fn send_end_of_stream(&mut self) -> Result<(), DecodeApiError> {
        self.codec_context
            .send_flush_packet()
            .map_err(decode_api_error_from_ffi)
    }

    fn receive_frame(&mut self) -> Result<ReceivedFrameMetadata, DecodeApiError> {
        self.receive_frame.unref();
        self.codec_context
            .receive_frame(&mut self.receive_frame)
            .map_err(decode_api_error_from_ffi)?;

        let timestamps = self.receive_frame.timestamps();
        let color = color_metadata_plan_from_ffmpeg_frame(self.receive_frame.color_metadata())
            .metadata()
            .cloned();
        let frame_ref = match self.receive_frame.try_clone_ref() {
            Ok(frame_ref) => frame_ref,
            Err(error) => {
                self.receive_frame.unref();
                return Err(DecodeApiError::Fatal(FfmpegDecoderThreadError::from(error)));
            }
        };
        self.receive_frame.unref();

        Ok(ReceivedFrameMetadata {
            timestamps,
            frame_ref: Some(frame_ref),
            color,
        })
    }

    fn flush_buffers(&mut self) -> Result<(), FfmpegDecoderThreadError> {
        self.codec_context.flush_buffers();
        self.receive_frame.unref();
        Ok(())
    }
}

/// Testable state machine around FFmpeg send/receive semantics.
#[cfg(any(test, feature = "ffmpeg"))]
pub(super) struct SendReceiveDecodeLoop<A: SendReceiveCodecApi> {
    /// Concrete or fake API implementation.
    pub(super) codec_api: A,

    /// PTS policy state kept outside FFmpeg raw structs.
    pts_resolver: FramePtsResolver,

    /// Last generation whose packet was accepted by the decoder.
    pub(super) current_generation: Option<u64>,

    /// Last stream/context color metadata accepted with a packet.
    current_context_color: Option<VideoColorMetadata>,

    /// Shared EOF/DPB drain state.
    eof_drain_state: Arc<Mutex<VideoDecoderEndOfStreamDrainState>>,

    /// Generation, для которого FFmpeg уже принял единственный NULL EOF packet.
    eof_sent_generation: Option<u64>,

    /// Activity notifier for frame/packet/drain progress.
    activity_notifier: VideoDecoderActivityNotifier,

    /// Completed packet counter for focused tests.
    pub(super) completed_packet_count: usize,
}

#[cfg(any(test, feature = "ffmpeg"))]
impl<A: SendReceiveCodecApi> SendReceiveDecodeLoop<A> {
    pub(super) fn new(
        codec_api: A,
        activity_notifier: VideoDecoderActivityNotifier,
        eof_drain_state: Arc<Mutex<VideoDecoderEndOfStreamDrainState>>,
    ) -> Self {
        Self {
            codec_api,
            pts_resolver: FramePtsResolver::default(),
            current_generation: None,
            current_context_color: None,
            eof_drain_state,
            eof_sent_generation: None,
            activity_notifier,
            completed_packet_count: 0,
        }
    }

    /// Sends one packet and drains up to `receive_budget` frames.
    ///
    /// `receive_budget` — число свободных host-upload pool slots на момент
    /// вызова. Pool не мутируется внутри этого метода (вставка в таблицу идёт
    /// позже, в `publish_decoded_frames`), поэтому бюджет считается по всему
    /// вызову и гарантирует, что мы не произведём больше кадров, чем влезет в
    /// pool. Если бюджет исчерпан до того, как FFmpeg принял packet (EAGAIN, а
    /// сливать output больше некуда), packet возвращается как `Deferred` и
    /// воркер повторит его после освобождения slot-а — вместо fatal table-full.
    pub(super) fn send_packet(
        &mut self,
        packet: DecodePacket,
        receive_budget: usize,
    ) -> Result<SendPacketOutcome, FfmpegDecoderThreadError> {
        let prepared_packet = self.codec_api.create_packet(&packet)?;
        let mut progress_report = DecodeProgressReport::default();
        let mut eagain_without_progress_count = 0usize;

        loop {
            let remaining_budget = receive_budget.saturating_sub(progress_report.frames.len());

            match self.codec_api.send_packet(&prepared_packet) {
                Ok(()) => {
                    self.current_generation = Some(packet.generation);
                    self.current_context_color = packet.resolved_color.clone();
                    self.pts_resolver.observe_accepted_packet(&packet);
                    let drain_report = self.receive_until_blocked(
                        packet.generation,
                        Some(packet.pts),
                        self.current_context_color.clone(),
                        remaining_budget,
                    )?;
                    progress_report.extend(drain_report);
                    progress_report.packet_completed = true;
                    self.completed_packet_count = self.completed_packet_count.saturating_add(1);
                    let _ = self.activity_notifier.notify_activity();
                    return Ok(SendPacketOutcome::Consumed(progress_report));
                }
                Err(DecodeApiError::Again) => {
                    if remaining_budget == 0 {
                        // FFmpeg отказывается принять packet, пока не сольём output,
                        // но pool budget исчерпан. Откладываем packet целиком.
                        return Ok(SendPacketOutcome::Deferred {
                            progress: progress_report,
                            packet,
                        });
                    }

                    let drain_generation = self.current_generation.unwrap_or(packet.generation);
                    let drain_report = self.receive_until_blocked(
                        drain_generation,
                        None,
                        self.current_context_color.clone(),
                        remaining_budget,
                    )?;
                    let made_progress = drain_report.made_progress();
                    progress_report.extend(drain_report);

                    if !made_progress {
                        eagain_without_progress_count =
                            eagain_without_progress_count.saturating_add(1);
                        if eagain_without_progress_count > 1 {
                            return Err(FfmpegDecoderThreadError::ProtocolViolation {
                                reason: "avcodec_send_packet returned EAGAIN twice without receive-side progress".to_owned(),
                            });
                        }
                    } else {
                        eagain_without_progress_count = 0;
                    }
                }
                Err(DecodeApiError::EndOfFile) => {
                    return Err(FfmpegDecoderThreadError::ProtocolViolation {
                        reason: "avcodec_send_packet returned EOF for a normal packet; decoder must be flushed before reuse".to_owned(),
                    });
                }
                Err(DecodeApiError::Fatal(error)) => return Err(error),
            }
        }
    }

    pub(super) fn flush_for_seek(&mut self) -> Result<(), FfmpegDecoderThreadError> {
        self.codec_api.flush_buffers()?;
        self.pts_resolver = FramePtsResolver::default();
        self.current_generation = None;
        self.current_context_color = None;
        self.eof_sent_generation = None;
        set_eof_drain_state(
            &self.eof_drain_state,
            VideoDecoderEndOfStreamDrainState::Idle,
            &self.activity_notifier,
        )?;
        let _ = self.activity_notifier.notify_activity();
        Ok(())
    }

    pub(super) fn begin_end_of_stream_drain(
        &mut self,
        generation: u64,
        receive_budget: usize,
    ) -> Result<EofDrainProgressReport, FfmpegDecoderThreadError> {
        if self.eof_sent_generation == Some(generation) {
            return self.continue_end_of_stream_drain(generation, receive_budget);
        }

        let mut eagain_without_progress_count = 0usize;
        let mut frames = Vec::new();

        loop {
            let remaining_budget = receive_budget.saturating_sub(frames.len());
            if remaining_budget == 0 {
                // Pool budget исчерпан; tail-кадры остаются в FFmpeg. Возвращаем
                // Draining, чтобы player перевыдал drain после освобождения slots.
                set_eof_drain_state(
                    &self.eof_drain_state,
                    VideoDecoderEndOfStreamDrainState::Draining { generation },
                    &self.activity_notifier,
                )?;
                return Ok(EofDrainProgressReport {
                    state: VideoDecoderEndOfStreamDrainState::Draining { generation },
                    frames,
                });
            }

            match self.codec_api.send_end_of_stream() {
                Ok(()) => {
                    self.eof_sent_generation = Some(generation);
                    set_eof_drain_state(
                        &self.eof_drain_state,
                        VideoDecoderEndOfStreamDrainState::Draining { generation },
                        &self.activity_notifier,
                    )?;
                    let drain_report = self.receive_until_blocked(
                        generation,
                        None,
                        self.current_context_color.clone(),
                        remaining_budget,
                    )?;
                    let stop_reason = drain_report.stop_reason;
                    frames.extend(drain_report.frames);

                    let state = match stop_reason {
                        ReceiveStopReason::EndOfFile => {
                            VideoDecoderEndOfStreamDrainState::Drained { generation }
                        }
                        ReceiveStopReason::NeedMoreInput
                        | ReceiveStopReason::ResourceBudgetReached => {
                            VideoDecoderEndOfStreamDrainState::Draining { generation }
                        }
                    };

                    set_eof_drain_state(
                        &self.eof_drain_state,
                        state.clone(),
                        &self.activity_notifier,
                    )?;

                    return Ok(EofDrainProgressReport { state, frames });
                }
                Err(DecodeApiError::Again) => {
                    let drain_generation = self.current_generation.unwrap_or(generation);
                    let drain_report = self.receive_until_blocked(
                        drain_generation,
                        None,
                        self.current_context_color.clone(),
                        remaining_budget,
                    )?;
                    let made_progress = drain_report.made_progress();
                    frames.extend(drain_report.frames);

                    if !made_progress {
                        eagain_without_progress_count =
                            eagain_without_progress_count.saturating_add(1);
                        if eagain_without_progress_count > 1 {
                            return Err(FfmpegDecoderThreadError::ProtocolViolation {
                                reason: "avcodec_send_packet(NULL) returned EAGAIN twice without receive-side progress".to_owned(),
                            });
                        }
                    } else {
                        eagain_without_progress_count = 0;
                    }
                }
                Err(DecodeApiError::EndOfFile) => {
                    self.eof_sent_generation = Some(generation);
                    let drained = VideoDecoderEndOfStreamDrainState::Drained { generation };
                    set_eof_drain_state(
                        &self.eof_drain_state,
                        drained.clone(),
                        &self.activity_notifier,
                    )?;
                    return Ok(EofDrainProgressReport {
                        state: drained,
                        frames,
                    });
                }
                Err(DecodeApiError::Fatal(error)) => return Err(error),
            }
        }
    }

    /// Продолжает receive-side EOF drain после освобождения bounded pool slots.
    fn continue_end_of_stream_drain(
        &mut self,
        generation: u64,
        receive_budget: usize,
    ) -> Result<EofDrainProgressReport, FfmpegDecoderThreadError> {
        let drain_report = self.receive_until_blocked(
            generation,
            None,
            self.current_context_color.clone(),
            receive_budget,
        )?;
        let state = match drain_report.stop_reason {
            ReceiveStopReason::EndOfFile => {
                VideoDecoderEndOfStreamDrainState::Drained { generation }
            }
            ReceiveStopReason::NeedMoreInput | ReceiveStopReason::ResourceBudgetReached => {
                VideoDecoderEndOfStreamDrainState::Draining { generation }
            }
        };
        set_eof_drain_state(
            &self.eof_drain_state,
            state.clone(),
            &self.activity_notifier,
        )?;
        Ok(EofDrainProgressReport {
            state,
            frames: drain_report.frames,
        })
    }

    pub(super) fn end_of_stream_drain_state(&self) -> VideoDecoderEndOfStreamDrainState {
        self.eof_drain_state
            .lock()
            .map(|state| state.clone())
            .unwrap_or_else(|_| VideoDecoderEndOfStreamDrainState::Fatal {
                generation: None,
                error: DecodeThreadError::new("FFmpeg EOF drain state lock is poisoned"),
            })
    }

    fn receive_until_blocked(
        &mut self,
        generation: u64,
        packet_pts_seed: Option<Duration>,
        context_color: Option<VideoColorMetadata>,
        receive_budget: usize,
    ) -> Result<ReceiveDrainReport, FfmpegDecoderThreadError> {
        let mut drain_report = ReceiveDrainReport::default();

        loop {
            if drain_report.frames.len() >= receive_budget {
                // Host-upload pool is full for this drain pass: leave the rest of
                // FFmpeg's buffered frames in place and resume after a release.
                drain_report.stop_reason = ReceiveStopReason::ResourceBudgetReached;
                return Ok(drain_report);
            }

            match self.codec_api.receive_frame() {
                Ok(frame_metadata) => {
                    let pts = self
                        .pts_resolver
                        .resolve_frame_pts(frame_metadata.timestamps, packet_pts_seed);
                    let color = frame_color_or_context_color(&frame_metadata, &context_color);
                    drain_report.frames.push(DecodedFrameRecord {
                        generation,
                        pts,
                        #[cfg(feature = "ffmpeg")]
                        frame_ref: frame_metadata.frame_ref,
                        color,
                    });
                    let _ = self.activity_notifier.notify_activity();
                }
                Err(DecodeApiError::Again) => {
                    drain_report.stop_reason = ReceiveStopReason::NeedMoreInput;
                    return Ok(drain_report);
                }
                Err(DecodeApiError::EndOfFile) => {
                    let drained = VideoDecoderEndOfStreamDrainState::Drained { generation };
                    set_eof_drain_state(&self.eof_drain_state, drained, &self.activity_notifier)?;
                    drain_report.stop_reason = ReceiveStopReason::EndOfFile;
                    return Ok(drain_report);
                }
                Err(DecodeApiError::Fatal(error)) => return Err(error),
            }
        }
    }
}

/// Minimal API surface needed by the send/receive state machine.
#[cfg(any(test, feature = "ffmpeg"))]
pub(super) trait SendReceiveCodecApi {
    /// Concrete packet owner type. Real implementation uses `OwnedAvPacket`.
    type Packet;

    /// Creates a padded input packet for one `DecodePacket`.
    fn create_packet(
        &mut self,
        packet: &DecodePacket,
    ) -> Result<Self::Packet, FfmpegDecoderThreadError>;

    /// Calls the FFmpeg-equivalent send-packet operation.
    fn send_packet(&mut self, packet: &Self::Packet) -> Result<(), DecodeApiError>;

    /// Calls the FFmpeg-equivalent NULL-packet EOF drain operation.
    fn send_end_of_stream(&mut self) -> Result<(), DecodeApiError>;

    /// Calls the FFmpeg-equivalent receive-frame operation.
    fn receive_frame(&mut self) -> Result<ReceivedFrameMetadata, DecodeApiError>;

    /// Clears decoder buffers for seek/lifecycle reset.
    fn flush_buffers(&mut self) -> Result<(), FfmpegDecoderThreadError>;
}

/// Normalized receive result used by tests and the real FFmpeg adapter.
#[derive(Debug)]
#[cfg(any(test, feature = "ffmpeg"))]
pub(super) struct ReceivedFrameMetadata {
    /// Timestamp fields copied from the received frame.
    pub(super) timestamps: FrameTimestamps,

    /// Refcounted frame reference kept alive for provider-owned HostPlanar access.
    #[cfg(feature = "ffmpeg")]
    pub(super) frame_ref: Option<OwnedAvFrame>,

    /// Frame-level color metadata normalized from FFmpeg fields when available.
    #[cfg(feature = "ffmpeg")]
    pub(super) color: Option<VideoColorMetadata>,
}

/// Internal send/receive status preserving EAGAIN vs EOF.
#[derive(Debug, Clone, PartialEq, Eq)]
#[cfg(any(test, feature = "ffmpeg"))]
pub(super) enum DecodeApiError {
    /// Caller must receive output and retry the same input packet.
    Again,

    /// Decoder is drained and cannot accept more normal packets before flush.
    EndOfFile,

    /// Fatal backend error.
    Fatal(FfmpegDecoderThreadError),
}

/// Metadata report for decoded frames observed by the state machine.
#[derive(Debug)]
#[cfg(any(test, feature = "ffmpeg"))]
pub(super) struct DecodedFrameRecord {
    /// Seek generation assigned by the decode loop.
    pub(super) generation: u64,

    /// Resolved presentation timestamp.
    pub(super) pts: Duration,

    /// Provider-owned AVFrame reference, present only in real FFmpeg builds.
    #[cfg(feature = "ffmpeg")]
    pub(super) frame_ref: Option<OwnedAvFrame>,

    /// Frame-level color metadata or packet/context fallback metadata.
    pub(super) color: Option<VideoColorMetadata>,
}

/// Outcome of one `send_packet` attempt under host-upload pool backpressure.
#[cfg(any(test, feature = "ffmpeg"))]
pub(super) enum SendPacketOutcome {
    /// FFmpeg accepted the packet; `frames` were drained within the pool budget.
    Consumed(DecodeProgressReport),

    /// Pool budget was exhausted before FFmpeg accepted the packet. The packet
    /// is returned for retry after the renderer releases a slot; `progress`
    /// still carries any frames drained so far.
    Deferred {
        progress: DecodeProgressReport,
        packet: DecodePacket,
    },
}

/// Report returned after send/drain progress.
#[derive(Debug)]
#[cfg(any(test, feature = "ffmpeg"))]
pub(super) struct DecodeProgressReport {
    /// Frames produced while processing the operation.
    pub(super) frames: Vec<DecodedFrameRecord>,

    /// True only after FFmpeg accepted the input packet.
    pub(super) packet_completed: bool,

    /// Why receive loop stopped.
    pub(super) stop_reason: ReceiveStopReason,
}

#[cfg(any(test, feature = "ffmpeg"))]
impl Default for DecodeProgressReport {
    fn default() -> Self {
        Self {
            frames: Vec::new(),
            packet_completed: false,
            stop_reason: ReceiveStopReason::NeedMoreInput,
        }
    }
}

#[cfg(any(test, feature = "ffmpeg"))]
impl DecodeProgressReport {
    fn extend(&mut self, drain_report: ReceiveDrainReport) {
        self.frames.extend(drain_report.frames);
        self.stop_reason = drain_report.stop_reason;
    }
}

/// Report returned by one receive loop.
#[derive(Debug)]
#[cfg(any(test, feature = "ffmpeg"))]
struct ReceiveDrainReport {
    /// Frames received before EAGAIN/EOF/fatal.
    frames: Vec<DecodedFrameRecord>,

    /// Non-fatal receive-loop stop reason.
    stop_reason: ReceiveStopReason,
}

/// Report returned after explicit EOF/DPB drain.
#[derive(Debug)]
#[cfg(any(test, feature = "ffmpeg"))]
pub(super) struct EofDrainProgressReport {
    /// Publicly visible drain lifecycle state.
    pub(super) state: VideoDecoderEndOfStreamDrainState,

    /// Tail frames produced while draining decoder buffers.
    pub(super) frames: Vec<DecodedFrameRecord>,
}

#[cfg(any(test, feature = "ffmpeg"))]
impl Default for ReceiveDrainReport {
    fn default() -> Self {
        Self {
            frames: Vec::new(),
            stop_reason: ReceiveStopReason::NeedMoreInput,
        }
    }
}

#[cfg(any(test, feature = "ffmpeg"))]
impl ReceiveDrainReport {
    fn made_progress(&self) -> bool {
        !self.frames.is_empty() || self.stop_reason == ReceiveStopReason::EndOfFile
    }
}

/// Non-fatal reason why receive loop stopped.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[cfg(any(test, feature = "ffmpeg"))]
pub(super) enum ReceiveStopReason {
    /// FFmpeg returned EAGAIN; caller may send more input.
    NeedMoreInput,

    /// FFmpeg returned EOF; drain is complete.
    EndOfFile,

    /// Host-upload pool budget exhausted; more frames remain buffered inside
    /// FFmpeg and must be drained after the renderer releases pool slots.
    ResourceBudgetReached,
}

/// PTS resolver policy: best_effort -> pts -> interpolation.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
#[cfg(any(test, feature = "ffmpeg"))]
struct FramePtsResolver {
    /// Last known stream time base.
    time_base: Option<StreamTimeBase>,

    /// Next timestamp predicted from the previous frame duration.
    next_interpolated_pts: Option<Duration>,

    /// Last non-zero frame duration seen in frame metadata.
    last_frame_duration: Option<Duration>,
}

#[cfg(any(test, feature = "ffmpeg"))]
impl FramePtsResolver {
    fn observe_accepted_packet(&mut self, packet: &DecodePacket) {
        if let Some(track_dts) = packet.track_dts {
            self.time_base = Some(StreamTimeBase::new(
                track_dts.time_base.numer,
                track_dts.time_base.denom,
            ));
        }

        if self.next_interpolated_pts.is_none() {
            self.next_interpolated_pts = Some(packet.pts);
        }
    }

    fn resolve_frame_pts(
        &mut self,
        timestamps: FrameTimestamps,
        packet_pts_seed: Option<Duration>,
    ) -> Duration {
        let explicit_pts = self
            .timestamp_units_to_duration(timestamps.best_effort_timestamp)
            .or_else(|| self.timestamp_units_to_duration(timestamps.pts));
        let frame_duration = self.frame_duration(timestamps.duration);
        let resolved_pts = explicit_pts
            .or(self.next_interpolated_pts)
            .or(packet_pts_seed)
            .unwrap_or(Duration::ZERO);

        if let Some(frame_duration) = frame_duration {
            self.last_frame_duration = Some(frame_duration);
        }

        if let Some(duration_step) = frame_duration.or(self.last_frame_duration) {
            self.next_interpolated_pts = Some(resolved_pts.saturating_add(duration_step));
        } else {
            self.next_interpolated_pts = Some(resolved_pts);
        }

        resolved_pts
    }

    fn timestamp_units_to_duration(self, units: i64) -> Option<Duration> {
        if units == NO_TIMESTAMP {
            return None;
        }

        let time_base = self.time_base?;

        Some(units_to_duration_saturating(units, time_base))
    }

    fn frame_duration(self, units: i64) -> Option<Duration> {
        if units <= 0 {
            return None;
        }

        let time_base = self.time_base?;

        Some(units_to_duration_saturating(units, time_base))
    }
}

/// Compact copy of media-core time base fields, avoiding a new public dependency leak.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[cfg(any(test, feature = "ffmpeg"))]
struct StreamTimeBase {
    /// Numerator from container track time base.
    numer: u32,

    /// Denominator from container track time base.
    denom: u32,
}

#[cfg(any(test, feature = "ffmpeg"))]
impl StreamTimeBase {
    fn new(numer: u32, denom: u32) -> Self {
        Self { numer, denom }
    }
}

#[cfg(any(test, feature = "ffmpeg"))]
fn units_to_duration_saturating(units: i64, time_base: StreamTimeBase) -> Duration {
    if units <= 0 || time_base.denom == 0 {
        return Duration::ZERO;
    }

    let total_nanoseconds = (units as u128)
        .saturating_mul(u128::from(time_base.numer))
        .saturating_mul(1_000_000_000)
        / u128::from(time_base.denom);
    let clamped_nanoseconds = total_nanoseconds.min(u128::from(u64::MAX));

    Duration::from_nanos(clamped_nanoseconds as u64)
}

#[cfg(feature = "ffmpeg")]
fn duration_to_units_saturating(duration: Duration, time_base: StreamTimeBase) -> i64 {
    if time_base.numer == 0 {
        return 0;
    }

    let duration_nanoseconds = u128::from(duration.as_secs())
        .saturating_mul(1_000_000_000)
        .saturating_add(u128::from(duration.subsec_nanos()));
    let units = duration_nanoseconds.saturating_mul(u128::from(time_base.denom))
        / u128::from(time_base.numer)
        / 1_000_000_000;

    if units > i64::MAX as u128 {
        i64::MAX
    } else {
        units as i64
    }
}

#[cfg(feature = "ffmpeg")]
fn decode_api_error_from_ffi(error: FfmpegError) -> DecodeApiError {
    match error.status_kind() {
        Some(FfmpegErrorKind::Again) => DecodeApiError::Again,
        Some(FfmpegErrorKind::EndOfFile) => DecodeApiError::EndOfFile,
        _ => DecodeApiError::Fatal(FfmpegDecoderThreadError::from(error)),
    }
}
