//! Наблюдения и trace-счётчики активной seek-транзакции.
use super::PlayerSession;
use super::seek_commit_gates::SeekProgressGateSnapshot;
use super::seek_transaction::playback_resume_intent_name;
use crate::seek_state::AccuratePrerollDemuxEventKind;
use crate::{ActiveSeekDiagnosticsSnapshot, PlayerTickConfig};
use std::time::{Duration, Instant};
use tracing::{debug, info, trace};
impl PlayerSession {
    /// Собирает подробный snapshot активного seek-а для throttled stall logs.
    #[must_use]
    pub(crate) fn active_seek_diagnostics(
        &self,
        now: Instant,
        tick_config: &PlayerTickConfig,
    ) -> Option<ActiveSeekDiagnosticsSnapshot> {
        let seek_commit = self.seek_runtime.active_commit()?;
        let target_position = seek_commit.target_position.as_duration();
        let queues = self.diagnostic_queue_depths();
        let required_video_frames = self.required_seek_resume_video_ready_frames(
            seek_commit,
            tick_config.effective_seek_resume_video_min_ready_frames(),
        );
        let ready_video_frames = self.seek_ready_video_frame_count(seek_commit);
        let target_frame_presented = self.seek_presented_frame_ready(seek_commit);
        let video_gate_ready = self.seek_video_gate_ready(seek_commit, required_video_frames);
        let audio_gate_status =
            self.seek_audio_gate_status(seek_commit, tick_config.seek_resume_audio_min_buffer_ms);
        let audio_gate_ready = audio_gate_status.is_ready();
        let diagnostics_snapshot = self.diagnostics_snapshot_with_queues(queues);
        let gate_snapshot = SeekProgressGateSnapshot {
            target_frame_presented,
            video_gate_ready,
            audio_gate_status,
            ready_video_frames,
            required_video_frames,
        };
        let blocker = self.seek_progress_blocker(
            tick_config,
            queues,
            gate_snapshot,
            diagnostics_snapshot.seek_bootstrap,
        );

        Some(ActiveSeekDiagnosticsSnapshot {
            kind: "seek",
            generation: seek_commit.generation,
            pipeline_generation: self.pipeline.seek_generation(),
            selected_video_track_id: self.pipeline.selected_video_track_id(),
            selected_audio_track_id: self.pipeline.selected_audio_track_id(),
            age: now.saturating_duration_since(seek_commit.started_at),
            target: target_position,
            actual: seek_commit.actual_position.as_duration(),
            resume_intent: playback_resume_intent_name(seek_commit.resume_intent),
            seek_mode: seek_commit.seek_mode,
            blocker,
            video_gate_ready,
            audio_gate_ready,
            target_frame_presented,
            ready_video_frames,
            required_video_frames,
            present_frame_pts: self.pipeline.present_video_frame_pts(),
            front_queued_frame_pts: self
                .pipeline
                .front_queued_video_frame()
                .map(|frame| frame.pts),
            demuxing_active: self.is_demuxing_active(),
            draining_after_eof: self.is_eof_draining(),
            stale_frame: self.snapshot.timeline.stale_frame,
            stale_generation_discards: diagnostics_snapshot.drops.stale_generation,
            seek_bootstrap: diagnostics_snapshot.seek_bootstrap,
            last_pause_reason: diagnostics_snapshot.pauses.last.map(|pause| pause.reason),
            accurate_preroll: self
                .seek_runtime
                .accurate_preroll_snapshot(seek_commit.drops_decode_preroll_before_target()),
            queues,
        })
    }

    /// Пишет compact marker для первых demux packets активного seek-а.
    pub(crate) fn note_demux_packet_for_seek_trace(
        &mut self,
        packet: &media_core::Packet,
        packet_generation: u64,
    ) {
        let Some(seek_commit) = self.seek_runtime.active_commit() else {
            return;
        };

        let elapsed = seek_commit.started_at.elapsed();
        let target_position = seek_commit.target_position.as_duration();
        let selected_video_packet = packet.kind == media_core::TrackKind::Video
            && self.pipeline.selected_video_track_id() == Some(packet.track_id);
        if seek_commit.drops_decode_preroll_before_target() {
            self.seek_runtime.record_accurate_preroll_demux_packet(
                packet.kind,
                selected_video_packet && packet.pts >= target_position,
                elapsed,
            );
        }

        let Some(trace_decision) = self.seek_runtime.record_post_seek_packet(packet.kind) else {
            return;
        };

        if trace_decision.first_video_packet {
            debug!(
                kind = "seek",
                target_ms = seek_commit.target_position.as_duration().as_millis(),
                actual_ms = seek_commit.actual_position.as_duration().as_millis(),
                active_seek_generation = seek_commit.generation,
                packet_generation,
                pipeline_generation = self.pipeline.seek_generation(),
                selected_video_track_id = ?self.pipeline.selected_video_track_id(),
                selected_audio_track_id = ?self.pipeline.selected_audio_track_id(),
                packet_index = trace_decision.packet_index,
                packet_track_id = %packet.track_id,
                packet_pts_ms = packet.pts.as_millis(),
                packet_dts_ms = ?packet.dts.map(|dts| dts.as_millis()),
                packet_duration_ms = ?packet.duration.map(|duration| duration.as_millis()),
                packet_keyframe = ?packet.keyframe,
                elapsed_ms = elapsed.as_millis(),
                "First post-seek video packet observed"
            );
            return;
        }

        debug!(
            kind = "seek",
            target_ms = seek_commit.target_position.as_duration().as_millis(),
            actual_ms = seek_commit.actual_position.as_duration().as_millis(),
            active_seek_generation = seek_commit.generation,
            packet_generation,
            pipeline_generation = self.pipeline.seek_generation(),
            selected_video_track_id = ?self.pipeline.selected_video_track_id(),
            selected_audio_track_id = ?self.pipeline.selected_audio_track_id(),
            packet_index = trace_decision.packet_index,
            packet_track_id = %packet.track_id,
            packet_kind = ?packet.kind,
            packet_pts_ms = packet.pts.as_millis(),
            packet_dts_ms = ?packet.dts.map(|dts| dts.as_millis()),
            packet_duration_ms = ?packet.duration.map(|duration| duration.as_millis()),
            packet_keyframe = ?packet.keyframe,
            elapsed_ms = elapsed.as_millis(),
            "Post-seek demux packet observed"
        );
    }

    /// Учитывает EOF marker demuxer-а для active Accurate seek diagnostics.
    pub(crate) fn note_demux_eof_for_seek_preroll_diagnostics(&mut self) {
        self.note_demux_event_for_seek_preroll_diagnostics(
            AccuratePrerollDemuxEventKind::EndOfStream,
        );
    }

    /// Учитывает TracksChanged marker demuxer-а для active Accurate seek diagnostics.
    pub(crate) fn note_demux_tracks_changed_for_seek_preroll_diagnostics(&mut self) {
        self.note_demux_event_for_seek_preroll_diagnostics(
            AccuratePrerollDemuxEventKind::TracksChanged,
        );
    }

    /// Учитывает fatal demux read error для active Accurate seek diagnostics.
    pub(crate) fn note_demux_error_for_seek_preroll_diagnostics(&mut self) {
        self.note_demux_event_for_seek_preroll_diagnostics(AccuratePrerollDemuxEventKind::Error);
    }

    /// Записывает demux lifecycle/error event только для Accurate skip semantics.
    fn note_demux_event_for_seek_preroll_diagnostics(
        &mut self,
        event_kind: AccuratePrerollDemuxEventKind,
    ) {
        let Some(seek_commit) = self.seek_runtime.active_commit() else {
            return;
        };

        if !seek_commit.drops_decode_preroll_before_target() {
            return;
        }

        self.seek_runtime
            .record_accurate_preroll_demux_event(event_kind);
    }

    /// Пишет marker первого decoded frame-а после accepted seek.
    pub(crate) fn note_decoded_video_frame_for_seek_trace(
        &mut self,
        frame_pts: Duration,
        frame_generation: u64,
    ) {
        let Some(seek_commit) = self.seek_runtime.active_commit() else {
            return;
        };

        let elapsed = seek_commit.started_at.elapsed();
        if seek_commit.drops_decode_preroll_before_target() {
            self.seek_runtime.record_accurate_preroll_decoded_frame(
                frame_pts >= seek_commit.landing_frame_min_position(),
                elapsed,
            );
        }

        if !self.seek_runtime.record_first_decoded_frame() {
            return;
        }

        info!(
            kind = "seek",
            target_ms = seek_commit.target_position.as_duration().as_millis(),
            actual_ms = seek_commit.actual_position.as_duration().as_millis(),
            active_seek_generation = seek_commit.generation,
            pipeline_generation = self.pipeline.seek_generation(),
            frame_pts_ms = frame_pts.as_millis(),
            frame_generation,
            elapsed_ms = elapsed.as_millis(),
            "First post-seek decoded frame observed"
        );
    }

    /// Пишет marker первого decoded frame-а, который дошёл до presentation queue.
    pub(crate) fn note_queued_video_frame_for_seek_trace(
        &mut self,
        frame_pts: Duration,
        frame_generation: u64,
    ) {
        let Some(seek_commit) = self.seek_runtime.active_commit() else {
            return;
        };

        let elapsed = seek_commit.started_at.elapsed();
        if seek_commit.drops_decode_preroll_before_target() {
            self.seek_runtime.record_accurate_preroll_queued_frame(
                frame_pts >= seek_commit.landing_frame_min_position(),
                elapsed,
            );
        }

        if !self.seek_runtime.record_first_queued_frame() {
            return;
        }

        debug!(
            kind = "seek",
            target_ms = seek_commit.target_position.as_duration().as_millis(),
            actual_ms = seek_commit.actual_position.as_duration().as_millis(),
            active_seek_generation = seek_commit.generation,
            pipeline_generation = self.pipeline.seek_generation(),
            frame_pts_ms = frame_pts.as_millis(),
            frame_generation,
            present_queue_depth = self.pipeline.video_present_queue_len(),
            elapsed_ms = elapsed.as_millis(),
            "First post-seek queued frame observed"
        );
    }

    /// Учитывает demuxed audio packet, отброшенный как Accurate preroll.
    pub(crate) fn note_skipped_audio_preroll_packet_for_seek_diagnostics(&mut self) {
        let Some(seek_commit) = self.seek_runtime.active_commit() else {
            return;
        };

        if seek_commit.drops_decode_preroll_before_target() {
            self.seek_runtime.record_skipped_audio_preroll_packet();
        }
    }

    /// Учитывает pre-target video packet, отправленный decoder-у во время Accurate seek-а.
    pub(crate) fn note_video_preroll_packet_sent_for_seek_diagnostics(&mut self) {
        let Some(seek_commit) = self.seek_runtime.active_commit() else {
            return;
        };

        if seek_commit.drops_decode_preroll_before_target() {
            self.seek_runtime.record_video_preroll_packet_sent();
        }
    }

    /// Учитывает target-or-after video packet, отправленный до первого landing frame.
    pub(crate) fn note_target_or_after_video_packet_sent_for_seek_diagnostics(&mut self) {
        let Some(seek_commit) = self.seek_runtime.active_commit() else {
            return;
        };

        if seek_commit.drops_decode_preroll_before_target()
            && !self.seek_presented_frame_ready(seek_commit)
        {
            self.seek_runtime.record_target_or_after_video_packet_sent();
        }
    }

    /// Учитывает decoded pre-target frame, который не дошёл до обычного scheduler-а.
    pub(crate) fn note_decoded_pre_target_frame_dropped_for_seek_diagnostics(&mut self) {
        let Some(seek_commit) = self.seek_runtime.active_commit() else {
            return;
        };

        if seek_commit.drops_decode_preroll_before_target() {
            self.seek_runtime.record_decoded_pre_target_frame_dropped();
        }
    }

    /// Учитывает frame, который backend подавил ниже decoder-side Accurate output-floor.
    pub(crate) fn note_suppressed_preroll_frame_for_seek_diagnostics(
        &mut self,
        pts: Duration,
        generation: u64,
        floor_pts: Duration,
    ) -> bool {
        let Some(seek_commit) = self.seek_runtime.active_commit() else {
            return false;
        };

        let matches_active_accurate_seek = seek_commit.generation == generation
            && seek_commit.drops_decode_preroll_before_target()
            && pts < seek_commit.landing_frame_min_position();
        if !matches_active_accurate_seek {
            return false;
        }

        self.seek_runtime.record_decoded_pre_target_frame_dropped();
        trace!(
            pts_ms = pts.as_millis(),
            generation,
            floor_ms = floor_pts.as_millis(),
            target_ms = seek_commit.target_position.as_duration().as_millis(),
            "Accurate seek preroll frame suppressed by decoder output floor"
        );
        true
    }

    /// Учитывает decoder/video admission backpressure во время Accurate fast-preroll-а.
    pub(crate) fn note_decoder_backpressure_for_seek_preroll_diagnostics(&mut self) {
        let Some(seek_commit) = self.seek_runtime.active_commit() else {
            return;
        };

        if seek_commit.drops_decode_preroll_before_target()
            && !self.seek_presented_frame_ready(seek_commit)
        {
            self.seek_runtime.record_decoder_backpressure_pause();
        }
    }
}
