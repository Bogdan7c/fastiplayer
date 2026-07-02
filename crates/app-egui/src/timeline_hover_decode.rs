//! App-owned active-playback hover decode session and incremental feed/drain driver.
//!
//! Здесь живёт исполнение уже resolved `DecodeDependencySpan`-а: пакеты
//! независимого hover demuxer-а кормятся в отдельный hover decoder thread,
//! pre-target кадры освобождаются, а первый target-or-after кадр превращается
//! в `VideoFrameLease` и вставляется в shared prepared working set через
//! production handoff boundary. Никакого CPU thumbnail path и никакого
//! переиспользования playback demuxer/decoder здесь нет.

use frame_server_core::{
    HoverResolvedBudget, TimelineHoverPrepareAdmissionMode, TimelineHoverPrepareAdmissionRequest,
    TimelineHoverPrepareProviderBudget,
};
use media_core::{DemuxReadEvent, MediaTime, Packet, PacketKeyframe, TrackKind, TrackTimestamp};
use player_core::{
    PlayerHoverStreamDecodeContext, PlayerTimelineHoverPrepareHandoff,
    PlayerTimelineHoverPrepareInsertOutcome,
};
use tracing::{debug, trace, warn};
use video_backend_api::{PresentFrameResourceProviderHandle, VideoBackendDecoderThreadHandle};
use video_core::{
    DecodePacket, DecodeSendError, DecodedFrame, VideoDecoderEndOfStreamDrainState,
    VideoStreamConfigResult,
};
use video_ffmpeg::FfmpegSoftwareHoverReservation;
use video_present_core::{
    VideoFrameLease, VideoFrameLeaseConfig, VideoFrameRelease, VideoFrameReleaseOutcome,
    VideoFrameReleaseSink,
};

use crate::timeline_hover_prepare::{
    TimelineHoverPrepareExecutorRequest, TimelineHoverPrepareIncompleteReason,
    TimelineHoverPreparePlaybackMode, TimelineHoverPreparePressure,
    TimelineHoverPrepareSpanDiagnostics, TimelineHoverPrepareSpanId,
};
use crate::timeline_hover_source::TimelineHoverOpenedSource;

/// Бюджет пакетов, скармливаемых decoder-у за один UI-кадр.
///
/// Feed — это только push в decoder channel (декод идёт в отдельном потоке),
/// но чтение пакетов local demuxer-а выполняется на UI-потоке, поэтому объём
/// на кадр ограничен. 32 пакета на UI-кадр при 60 FPS дают ~1900 пакетов/с —
/// с запасом покрывают типичный GOP до target-а за несколько кадров.
const HOVER_DECODE_FEED_PACKETS_PER_UI_FRAME: u32 = 32;

/// Активная hover decode session над отдельным software backend-ом.
///
/// Владеет decoder thread handle, provider-ом для lease lookup/release и
/// software budget reservation. Живёт от backend build до backend switch.
pub(crate) struct TimelineHoverDecodeSession {
    decoder_thread: Box<VideoBackendDecoderThreadHandle>,
    resource_provider: PresentFrameResourceProviderHandle,
    release_sink: std::sync::Arc<HoverProviderReleaseSink>,
    _reservation: FfmpegSoftwareHoverReservation,
    resolved_budget: HoverResolvedBudget,
    configured_stream: Option<PlayerHoverStreamDecodeContext>,
    generation: u64,
    fatal_error: Option<String>,
}

impl TimelineHoverDecodeSession {
    /// Создаёт session поверх уже wrapped (WGPU release boundary) hover backend-а.
    #[must_use]
    pub(crate) fn new(
        decoder_thread: Box<VideoBackendDecoderThreadHandle>,
        resource_provider: PresentFrameResourceProviderHandle,
        reservation: FfmpegSoftwareHoverReservation,
        resolved_budget: HoverResolvedBudget,
    ) -> Self {
        let release_sink = std::sync::Arc::new(HoverProviderReleaseSink {
            provider: resource_provider.clone(),
        });
        Self {
            decoder_thread,
            resource_provider,
            release_sink,
            _reservation: reservation,
            resolved_budget,
            configured_stream: None,
            generation: 0,
            fatal_error: None,
        }
    }

    /// Provider hover session-а для выбора matching materializer-а per lease.
    #[must_use]
    pub(crate) fn resource_provider(&self) -> &PresentFrameResourceProviderHandle {
        &self.resource_provider
    }

    /// Возвращает resolved hover budget для diagnostics.
    #[must_use]
    pub(crate) fn resolved_budget(&self) -> &HoverResolvedBudget {
        &self.resolved_budget
    }

    /// Session стала непригодной после fatal decoder error.
    #[must_use]
    pub(crate) fn is_failed(&self) -> bool {
        self.fatal_error.is_some()
    }

    fn release_decoded_frame(&self, frame: &DecodedFrame) {
        self.decoder_thread.release_frame(frame.resource_handle);
    }
}

impl std::fmt::Debug for TimelineHoverDecodeSession {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("TimelineHoverDecodeSession")
            .field("backend_name", &self.decoder_thread.backend_name())
            .field("generation", &self.generation)
            .field("fatal_error", &self.fatal_error)
            .finish_non_exhaustive()
    }
}

/// Release sink hover leases: возвращает host frame обратно в hover decoder pool.
struct HoverProviderReleaseSink {
    provider: PresentFrameResourceProviderHandle,
}

impl VideoFrameReleaseSink for HoverProviderReleaseSink {
    fn release_frame(&self, release: VideoFrameRelease) -> VideoFrameReleaseOutcome {
        self.provider.release_frame(release.resource_handle());
        VideoFrameReleaseOutcome::Accepted
    }
}

/// Продолжаемое состояние decode одного in-flight dependency span-а.
#[derive(Debug)]
pub(crate) struct HoverSpanDecodeState {
    span_id: TimelineHoverPrepareSpanId,
    generation: u64,
    pending_packet: Option<DecodePacket>,
    packets_fed: u32,
    frames_drained: u32,
    demux_eof: bool,
}

impl HoverSpanDecodeState {
    fn diagnostics(
        &self,
        post_target_reorder_drain_frames: u16,
    ) -> TimelineHoverPrepareSpanDiagnostics {
        TimelineHoverPrepareSpanDiagnostics::new(
            self.packets_fed,
            self.frames_drained,
            post_target_reorder_drain_frames,
        )
    }
}

/// Typed итог одного driver pass-а; executor превращает его в executor outcome.
pub(crate) enum HoverSpanDecodeDrive {
    /// Первый target-or-after кадр вставлен в shared working set.
    PreparedHit {
        actual_pts: TrackTimestamp,
        diagnostics: TimelineHoverPrepareSpanDiagnostics,
    },
    /// Работа осталась; продолжение на следующем UI-кадре без reseek-а.
    Incomplete {
        reason: TimelineHoverPrepareIncompleteReason,
        diagnostics: TimelineHoverPrepareSpanDiagnostics,
    },
    /// Провайдер/working set не принял подготовленный кадр.
    Pressure {
        pressure: TimelineHoverPreparePressure,
    },
    /// Session/demux больше не могут исполнять span (typed деградация).
    Failed { failure: HoverSpanDecodeFailure },
}

/// Typed причина, по которой decode span не может продолжаться.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum HoverSpanDecodeFailure {
    /// Hover decoder отклонил published stream config.
    StreamConfigRejected,
    /// Decoder thread остановился с fatal ошибкой.
    DecoderFatal,
    /// Чтение hover demuxer-а завершилось ошибкой.
    DemuxReadFailed,
    /// Container сменил track list; старый span больше не валиден.
    TracksChanged,
}

/// Один инкрементальный feed/drain pass для активного dependency span-а.
///
/// Вызывается executor-ом на каждом UI-кадре, пока span не завершится,
/// не будет superseded или не деградирует typed-ом. Demux позиция сохраняется
/// между вызовами: resolver уже выполнил `decode_point_before` seek при
/// установке span-а, и повторный seek внутри same-span продолжения запрещён.
pub(crate) fn drive_hover_span_decode(
    session: &mut TimelineHoverDecodeSession,
    span_state: &mut Option<HoverSpanDecodeState>,
    hover_source: &mut TimelineHoverOpenedSource,
    handoff: &PlayerTimelineHoverPrepareHandoff,
    stream_context: &PlayerHoverStreamDecodeContext,
    request: &TimelineHoverPrepareExecutorRequest,
) -> HoverSpanDecodeDrive {
    if session.fatal_error.is_some() {
        return HoverSpanDecodeDrive::Failed {
            failure: HoverSpanDecodeFailure::DecoderFatal,
        };
    }

    let span = request.span;
    let target = request.target;
    let post_target_reorder = span.post_target_reorder_drain_frames();

    // Начало нового span-а: реконфигурация stream-а (если поменялся) + flush.
    let needs_new_state = !matches!(span_state, Some(state) if state.span_id == request.span_id);
    if needs_new_state {
        if session.configured_stream.as_ref() != Some(stream_context) {
            match session
                .decoder_thread
                .configure_stream(stream_context.stream_config.clone())
            {
                VideoStreamConfigResult::Configured
                | VideoStreamConfigResult::Unchanged
                | VideoStreamConfigResult::Cleared => {
                    session.configured_stream = Some(stream_context.clone());
                }
                VideoStreamConfigResult::AbsentDecoder => {
                    warn!("Hover decoder thread absent during stream configuration");
                    return HoverSpanDecodeDrive::Failed {
                        failure: HoverSpanDecodeFailure::StreamConfigRejected,
                    };
                }
                VideoStreamConfigResult::Unsupported(rejection) => {
                    warn!(
                        rejection = %rejection,
                        "Hover decoder rejected playback stream config"
                    );
                    return HoverSpanDecodeDrive::Failed {
                        failure: HoverSpanDecodeFailure::StreamConfigRejected,
                    };
                }
                VideoStreamConfigResult::Backpressure(reason) => {
                    // Control channel забит: не помечаем session failed, просто
                    // повторим конфигурацию на следующем UI-кадре.
                    trace!(%reason, "Hover decoder stream configuration backpressure");
                    return HoverSpanDecodeDrive::Pressure {
                        pressure: TimelineHoverPreparePressure::ResourceBusy,
                    };
                }
                VideoStreamConfigResult::Fatal(error) => {
                    warn!(error = %error.message(), "Hover decoder stream configuration failed");
                    session.fatal_error = Some(error.message().to_owned());
                    return HoverSpanDecodeDrive::Failed {
                        failure: HoverSpanDecodeFailure::DecoderFatal,
                    };
                }
            }
        }

        if let Err(error) = session.decoder_thread.flush() {
            warn!(error = %error, "Hover decoder flush failed");
            session.fatal_error = Some(error.to_string());
            return HoverSpanDecodeDrive::Failed {
                failure: HoverSpanDecodeFailure::DecoderFatal,
            };
        }
        session.generation = session.generation.wrapping_add(1);
        *span_state = Some(HoverSpanDecodeState {
            span_id: request.span_id,
            generation: session.generation,
            pending_packet: None,
            packets_fed: 0,
            frames_drained: 0,
            demux_eof: false,
        });
        trace!(
            span_id = ?request.span_id,
            generation = session.generation,
            "Hover decode span started"
        );
    }
    let state = span_state
        .as_mut()
        .expect("hover span decode state must exist after span start");

    let video_track_id = stream_context.stream_config.track_id;
    let mut fed_this_pass: u32 = 0;

    loop {
        // 1. Сначала выгребаем готовые кадры: pre-target release, target-or-after — hit.
        while let Some(frame) = session.decoder_thread.try_recv_frame() {
            if frame.generation != state.generation {
                session.release_decoded_frame(&frame);
                continue;
            }
            state.frames_drained = state.frames_drained.saturating_add(1);

            let actual_pts = target.pts_from_media_time(MediaTime::from_duration(frame.pts));
            if target.actual_pts_is_before_target(actual_pts) {
                session.release_decoded_frame(&frame);
                continue;
            }

            return insert_target_or_after_frame(
                session,
                handoff,
                request,
                state,
                frame,
                actual_pts,
                post_target_reorder,
            );
        }

        // 2. Fatal decoder error делает session непригодной typed-ом, не паникой.
        if let Some(error) = session.decoder_thread.try_recv_error() {
            warn!(error = %error.message(), "Hover decoder thread reported fatal error");
            session.fatal_error = Some(error.message().to_owned());
            return HoverSpanDecodeDrive::Failed {
                failure: HoverSpanDecodeFailure::DecoderFatal,
            };
        }

        // 3. После demux EOF остаётся только дождаться decoder drain-а.
        if state.demux_eof {
            return match session.decoder_thread.end_of_stream_drain_state() {
                VideoDecoderEndOfStreamDrainState::Drained { .. } => {
                    HoverSpanDecodeDrive::Incomplete {
                        reason: TimelineHoverPrepareIncompleteReason::EndOfStreamBeforeTarget,
                        diagnostics: state.diagnostics(post_target_reorder),
                    }
                }
                _ => HoverSpanDecodeDrive::Incomplete {
                    reason: TimelineHoverPrepareIncompleteReason::DecodeBudgetExhausted,
                    diagnostics: state.diagnostics(post_target_reorder),
                },
            };
        }

        // 4. Feed budget на этот UI-кадр исчерпан — честный incomplete, продолжим.
        if fed_this_pass >= HOVER_DECODE_FEED_PACKETS_PER_UI_FRAME {
            return HoverSpanDecodeDrive::Incomplete {
                reason: TimelineHoverPrepareIncompleteReason::DecodeBudgetExhausted,
                diagnostics: state.diagnostics(post_target_reorder),
            };
        }

        // 5. Берём отложенный (backpressure) пакет или читаем следующий из demuxer-а.
        let decode_packet = match state.pending_packet.take() {
            Some(packet) => packet,
            None => match hover_source.demuxer_mut().next_event() {
                Ok(DemuxReadEvent::Packet(packet)) => {
                    if packet.kind != TrackKind::Video || packet.track_id != video_track_id {
                        continue;
                    }
                    decode_packet_from_demux_packet(packet, state.generation, stream_context)
                }
                Ok(DemuxReadEvent::EndOfStream) => {
                    state.demux_eof = true;
                    let _ = session
                        .decoder_thread
                        .begin_end_of_stream_drain(state.generation);
                    continue;
                }
                Ok(DemuxReadEvent::TracksChanged(_)) => {
                    debug!("Hover demuxer reported tracks change; superseding decode span");
                    return HoverSpanDecodeDrive::Failed {
                        failure: HoverSpanDecodeFailure::TracksChanged,
                    };
                }
                Err(error) => {
                    warn!(error = %error, "Hover demuxer packet read failed");
                    return HoverSpanDecodeDrive::Failed {
                        failure: HoverSpanDecodeFailure::DemuxReadFailed,
                    };
                }
            },
        };

        // 6. Push в decoder channel; на backpressure пакет сохраняется без потери.
        match session.decoder_thread.send_packet(decode_packet.clone()) {
            Ok(()) => {
                state.packets_fed = state.packets_fed.saturating_add(1);
                fed_this_pass += 1;
            }
            Err(DecodeSendError::Backpressure(reason)) => {
                trace!(reason = %reason, "Hover decoder packet channel backpressure");
                state.pending_packet = Some(decode_packet);
                return HoverSpanDecodeDrive::Pressure {
                    pressure: TimelineHoverPreparePressure::DecoderBackpressure,
                };
            }
            Err(DecodeSendError::Fatal(error)) => {
                warn!(error = %error, "Hover decoder thread stopped before accepting packet");
                session.fatal_error = Some(error.to_string());
                return HoverSpanDecodeDrive::Failed {
                    failure: HoverSpanDecodeFailure::DecoderFatal,
                };
            }
        }
    }
}

/// Отменяет decode continuation активного span-а, не разрушая session.
pub(crate) fn cancel_hover_span_decode(
    session: &mut TimelineHoverDecodeSession,
    span_state: &mut Option<HoverSpanDecodeState>,
) {
    if span_state.take().is_some()
        && session.fatal_error.is_none()
        && let Err(error) = session.decoder_thread.flush()
    {
        warn!(error = %error, "Hover decoder flush failed during span cancel");
        session.fatal_error = Some(error.to_string());
    }
}

fn insert_target_or_after_frame(
    session: &TimelineHoverDecodeSession,
    handoff: &PlayerTimelineHoverPrepareHandoff,
    request: &TimelineHoverPrepareExecutorRequest,
    state: &HoverSpanDecodeState,
    frame: DecodedFrame,
    actual_pts: TrackTimestamp,
    post_target_reorder: u16,
) -> HoverSpanDecodeDrive {
    let lease = VideoFrameLease::new(
        VideoFrameLeaseConfig::new(frame.generation, frame, session.release_sink.clone())
            .with_resource_provider(session.resource_provider.clone()),
    );

    let admission_mode = match request.target.playback_mode() {
        TimelineHoverPreparePlaybackMode::ResumePendingAfterSeek { .. } => {
            TimelineHoverPrepareAdmissionMode::ResumePendingAfterSeekPin
        }
        _ => TimelineHoverPrepareAdmissionMode::NormalHover,
    };
    let prepared_key = request.target.prepared_key();
    let admission = TimelineHoverPrepareAdmissionRequest::new(
        prepared_key,
        prepared_key,
        admission_mode,
        TimelineHoverPrepareProviderBudget::SpareSlotAvailable,
    );

    match handoff.insert_hover_prepared_frame(admission, lease, actual_pts) {
        PlayerTimelineHoverPrepareInsertOutcome::Inserted {
            evicted_primary_byproducts,
        } => {
            trace!(
                ?actual_pts,
                evicted_primary_byproducts,
                packets_fed = state.packets_fed,
                frames_drained = state.frames_drained,
                "Hover decode span prepared exact frame"
            );
            HoverSpanDecodeDrive::PreparedHit {
                actual_pts,
                diagnostics: state.diagnostics(post_target_reorder),
            }
        }
        PlayerTimelineHoverPrepareInsertOutcome::NoOp { reason } => {
            debug!(?reason, "Hover prepared frame insert rejected by admission");
            HoverSpanDecodeDrive::Pressure {
                pressure: TimelineHoverPreparePressure::ProviderBudgetExhausted,
            }
        }
    }
}

fn decode_packet_from_demux_packet(
    packet: Packet,
    generation: u64,
    stream_context: &PlayerHoverStreamDecodeContext,
) -> DecodePacket {
    DecodePacket {
        track_id: packet.track_id,
        pts: packet.pts,
        dts: packet.dts,
        track_dts: packet.track_dts,
        generation,
        encoded_bytes: packet.data,
        keyframe: matches!(packet.keyframe, PacketKeyframe::Keyframe),
        resolved_color: stream_context.resolved_color.clone(),
    }
}
