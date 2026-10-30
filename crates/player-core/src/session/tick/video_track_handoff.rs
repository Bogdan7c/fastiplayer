//! Передача video decoder-а новому описанию дорожки на границе показа (эстафета 03d).
//!
//! Первый packet очереди помечен описанием новее, чем у decoder-а: это граница
//! смены качества или кодека. Send loop его не отдаёт
//! (`front_sendable_video_packet`), а этот шаг раз в tick
//! двигает передачу:
//! 1. ждёт, пока decoder обработает все уже отправленные packet-ы старого
//!    описания (packets и control идут разными каналами без общего порядка);
//! 2. выжимает decoder тем же EOF/DPB drain, что и конец файла: B-кадры из
//!    DPB выходят и показываются как обычно;
//! 3. ждёт `Drained` — по договору `video-core` это значит, что каждый кадр
//!    старого описания уже забран из канала в очередь показа;
//! 4. flush (decoder снова не «выжат», generation не меняется) и настройка на
//!    новое описание тем же выбором трека, что при открытии media; новый
//!    decoder ждёт ключевой кадр.
//!
//! Кадры в очереди показа, звук и часы этот шаг не трогает: старые кадры
//! доигрываются, пока новый decoder стартует.

use media_core::TrackInfo;
use tracing::{debug, info, warn};
use video_core::{VideoDecoderEndOfStreamDrainResult, VideoDecoderEndOfStreamDrainState};

use crate::{PlayerError, PlayerErrorKind};

use super::super::capability_selection::VideoTrackReconfiguration;
use super::PlayerSession;

/// Где сейчас передача video decoder-а.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) enum VideoTrackHandoffProgress {
    /// Первый packet очереди относится к описанию decoder-а: передавать нечего.
    NotNeeded,

    /// Передача идёт: ждём отправленные packet-ы, выжимание или забор кадров.
    Waiting,

    /// Decoder настроен на новое описание; packet-ы нового эпизода можно слать.
    Completed,

    /// Передача сорвалась; session уже в fatal error.
    Failed,
}

/// Делает один шаг передачи video decoder-а новому описанию дорожки.
pub(super) fn advance_video_track_handoff(
    session: &mut PlayerSession,
) -> VideoTrackHandoffProgress {
    let Some(next_track) = session.pipeline.pending_video_track_handoff().cloned() else {
        if session
            .pipeline
            .front_video_packet_has_unknown_track_description()
        {
            fail_handoff(
                session,
                "the first queued video packet belongs to an unknown track description",
            );
            return VideoTrackHandoffProgress::Failed;
        }
        return VideoTrackHandoffProgress::NotNeeded;
    };
    // Без decoder thread packet-ы и так не уходят; другой backend ставится со своей
    // перемоткой, а она сама настроит decoder на последнее описание.
    if !session.pipeline.has_active_video_decoder()
        || session.has_pending_video_backend_reselection()
    {
        return VideoTrackHandoffProgress::Waiting;
    }
    // Шаг 1: drain, обогнавший packet, перевёл бы decoder в EOF раньше времени.
    // Свежие ACK-и забираются здесь, чтобы не терять на этом лишний tick.
    let completed_packets = session.pipeline.drain_completed_video_decode_packet_count();
    session
        .pipeline
        .note_video_packets_completed_by_decoder(completed_packets);
    if session.pipeline.video_decode_in_flight_packets() > 0 {
        return VideoTrackHandoffProgress::Waiting;
    }

    let generation = session.pipeline.seek_generation();
    match session.pipeline.video_decoder_end_of_stream_drain_state() {
        VideoDecoderEndOfStreamDrainState::Drained {
            generation: drained_generation,
        } if drained_generation == generation => hand_decoder_to(session, &next_track),
        VideoDecoderEndOfStreamDrainState::Draining {
            generation: draining_generation,
        } if draining_generation == generation => VideoTrackHandoffProgress::Waiting,
        VideoDecoderEndOfStreamDrainState::Fatal { error, .. } => {
            fail_handoff(session, &format!("decoder drain failed: {error}"));
            VideoTrackHandoffProgress::Failed
        }
        VideoDecoderEndOfStreamDrainState::Idle
        | VideoDecoderEndOfStreamDrainState::Draining { .. }
        | VideoDecoderEndOfStreamDrainState::Drained { .. } => {
            begin_decoder_drain(session, generation, &next_track)
        }
    }
}

/// Шаг 2: выжимает decoder до последнего кадра старого описания.
fn begin_decoder_drain(
    session: &mut PlayerSession,
    generation: u64,
    next_track: &TrackInfo,
) -> VideoTrackHandoffProgress {
    match session
        .pipeline
        .begin_video_decoder_end_of_stream_drain(generation)
    {
        // Decoder пропал между проверками: старых кадров у него нет.
        VideoDecoderEndOfStreamDrainResult::AbsentDecoder => hand_decoder_to(session, next_track),
        VideoDecoderEndOfStreamDrainResult::Started(state)
        | VideoDecoderEndOfStreamDrainResult::Unchanged(state) => {
            debug!(
                state = ?state,
                generation,
                track_id = %next_track.id,
                "Video decoder drains the previous track description before handoff"
            );
            VideoTrackHandoffProgress::Waiting
        }
        VideoDecoderEndOfStreamDrainResult::Backpressure(reason) => {
            debug!(
                reason = %reason,
                generation,
                "Video decoder handoff drain hit control-channel backpressure; retry next tick"
            );
            VideoTrackHandoffProgress::Waiting
        }
        VideoDecoderEndOfStreamDrainResult::Fatal(error) => {
            fail_handoff(session, &format!("decoder drain failed: {error}"));
            VideoTrackHandoffProgress::Failed
        }
    }
}

/// Шаг 4: старый decoder пуст — flush и настройка на новое описание.
fn hand_decoder_to(
    session: &mut PlayerSession,
    next_track: &TrackInfo,
) -> VideoTrackHandoffProgress {
    // Ошибка flush фатальна так же, как при перемотке: decoder в неизвестном состоянии.
    if let Err(error) = session.pipeline.flush_video_decoder_thread() {
        fail_handoff(session, &format!("decoder flush failed: {error:#}"));
        return VideoTrackHandoffProgress::Failed;
    }
    // Сброшенный decoder начинает с ключевого кадра; учёт «в полёте» уже нулевой.
    session.pipeline.require_video_decoder_keyframe();
    session.pipeline.reset_video_decode_in_flight();
    match session.reconfigure_video_decoder_for_track(next_track) {
        Ok(VideoTrackReconfiguration::Configured) => {
            session.pipeline.complete_video_track_handoff();
            info!(
                track_id = %next_track.id,
                codec_id = %next_track.codec_id,
                "Video decoder handed over to the next track description at the presentation boundary"
            );
            VideoTrackHandoffProgress::Completed
        }
        // Шлагбаум остаётся закрытым. Канал занят — следующий tick повторит шаги
        // (выжимание пустого decoder-а мгновенно); новый backend — его перемотка
        // настроит decoder сама.
        Ok(
            VideoTrackReconfiguration::ControlChannelBusy
            | VideoTrackReconfiguration::BackendReselectionRequested,
        ) => VideoTrackHandoffProgress::Waiting,
        Err(error) => {
            warn!(error = %error, "Video decoder rejected the next track description at handoff");
            session.mark_fatal_error(error);
            VideoTrackHandoffProgress::Failed
        }
    }
}

fn fail_handoff(session: &mut PlayerSession, reason: &str) {
    warn!(reason, "Video decoder handoff failed");
    session.mark_fatal_error(PlayerError::new(
        PlayerErrorKind::RuntimeError,
        format!("Video decoder handoff to the next track description failed: {reason}"),
    ));
}
