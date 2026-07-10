//! Accelerated-video overload policy вокруг pipeline-owned recovery state machine.

use tracing::{debug, trace, warn};

use super::{
    PlayerTickConfig,
    demux_admission::{
        DemuxPacketRouteOutcome, audio_catchup_pending_video_limit,
        audio_priority_pending_video_limit_allows_demux,
    },
};
use crate::{
    pipeline::{VideoBacklogRecoveryScanLimits, VideoBacklogRecoveryScanStart},
    session::PlayerSession,
};

/// Запускает bounded scan только для ускоренного playback и полного catch-up FIFO.
pub(super) fn try_begin_video_backlog_recovery_scan(
    session: &mut PlayerSession,
    tick_config: &PlayerTickConfig,
    prioritize_audio_catchup: bool,
) {
    if !prioritize_audio_catchup
        || !session.snapshot().playback_rate.is_faster_than_normal()
        || audio_priority_pending_video_limit_allows_demux(session, tick_config)
    {
        return;
    }

    let scan_limits = video_backlog_recovery_scan_limits(tick_config);
    match session
        .pipeline
        .begin_video_backlog_recovery_scan(scan_limits)
    {
        VideoBacklogRecoveryScanStart::Started => {
            debug!(
                retained_pending_video_packets = session.pipeline.pending_video_packet_len(),
                max_staged_video_packets = scan_limits.max_staged_packets,
                max_staged_video_bytes = scan_limits.max_staged_bytes,
                catchup_video_packet_limit = audio_catchup_pending_video_limit(tick_config),
                audio_buffer_ms = session.audio_buffer_level_ms().unwrap_or(0.0),
                playback_rate = %session.snapshot().playback_rate,
                "Video backlog recovery scan начат без очистки текущей очереди"
            );
        }
        VideoBacklogRecoveryScanStart::AlreadyScanning => {}
        VideoBacklogRecoveryScanStart::BackoffUntilBacklogDrains => {
            trace!("Video backlog recovery ждёт разгрузки pending FIFO после rollback");
        }
        VideoBacklogRecoveryScanStart::NoSelectedVideo => {
            trace!("Video backlog recovery не запущен: video track не выбран");
        }
        VideoBacklogRecoveryScanStart::NoActiveVideoRequirement => {
            trace!("Video backlog recovery не запущен: codec requirement отсутствует");
        }
        VideoBacklogRecoveryScanStart::CodecWithoutNoFlushRecoveryProof { codec } => {
            trace!(
                ?codec,
                "Video backlog recovery не запущен: no-flush cut не доказан для codec"
            );
        }
        VideoBacklogRecoveryScanStart::NoProvenKeyframeObserved => {
            trace!("Video backlog recovery не запущен: container ещё не доказал keyframe");
        }
        VideoBacklogRecoveryScanStart::DecoderAwaitingKeyframe => {
            trace!("Video backlog recovery не запущен: decoder уже ожидает bootstrap keyframe");
        }
    }
}

/// Публикует один итоговый warning вместо start+finish WARN spam-а.
pub(super) fn log_video_backlog_recovery_outcome(
    session: &PlayerSession,
    route_outcome: DemuxPacketRouteOutcome,
) {
    match route_outcome {
        DemuxPacketRouteOutcome::SwitchedVideoBacklogAtKeyframe {
            discarded_pending_packets,
            discarded_staged_packets,
            recovery_keyframe_pts,
        } => {
            warn!(
                discarded_pending_packets,
                discarded_staged_packets,
                recovery_keyframe_pts_ms = recovery_keyframe_pts.as_secs_f64() * 1000.0,
                audio_buffer_ms = session.audio_buffer_level_ms().unwrap_or(0.0),
                playback_rate = %session.snapshot().playback_rate,
                "Video backlog атомарно переключён на proven keyframe"
            );
        }
        DemuxPacketRouteOutcome::VideoBacklogRecoveryScanLimitReached {
            limit,
            restored_staged_packets,
            restored_staged_bytes,
            pending_packets_after_restore,
        } => {
            warn!(
                ?limit,
                restored_staged_packets,
                restored_staged_bytes,
                pending_packets_after_restore,
                playback_rate = %session.snapshot().playback_rate,
                "Video backlog recovery достиг bounded scan limit и восстановил continuation"
            );
        }
        DemuxPacketRouteOutcome::Queued
        | DemuxPacketRouteOutcome::DroppedSeekAudioPreroll
        | DemuxPacketRouteOutcome::StagedVideoBacklogRecoveryPacket => {}
    }
}

/// Строит именованные allocation/rearm limits из существующего tick config.
fn video_backlog_recovery_scan_limits(
    tick_config: &PlayerTickConfig,
) -> VideoBacklogRecoveryScanLimits {
    VideoBacklogRecoveryScanLimits {
        // Именованный maximum остаётся строгой allocation-границей даже если
        // caller сознательно задаёт его ниже decoder-facing catch-up FIFO.
        max_staged_packets: tick_config.max_video_backlog_recovery_scan_packets,
        max_staged_bytes: tick_config.max_video_backlog_recovery_scan_bytes,
        rearm_pending_packets: tick_config.max_pending_video_packets,
    }
}
