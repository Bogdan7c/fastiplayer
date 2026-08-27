//! Private behavior trace и Accurate-preroll telemetry; само состояние остаётся у parent-модуля.

use super::*;

/// Запоминает elapsed только для первого события стадии.
fn record_first_elapsed(slot: &mut Option<Duration>, elapsed: Duration) {
    if slot.is_none() {
        *slot = Some(elapsed);
    }
}

/// Увеличивает счётчик без риска wrap-around.
fn increment_counter(counter: &mut u64) {
    *counter = counter.saturating_add(1);
}

impl SeekTraceState {
    /// Начинает новый trace и забывает одноразовые markers предыдущего seek-а.
    pub(crate) fn begin(&mut self, generation: u64) {
        *self = Self {
            active_generation: Some(generation),
            ..Self::default()
        };
    }

    /// Завершает trace без изменения runtime state seek transaction-а.
    pub(crate) fn clear(&mut self) {
        *self = Self::default();
    }

    /// Учитывает demux packet и возвращает решение о compact logging.
    pub(crate) fn record_post_seek_packet(
        &mut self,
        packet_kind: TrackKind,
    ) -> Option<PostSeekPacketTraceDecision> {
        self.active_generation?;

        self.observed_post_seek_packets = self.observed_post_seek_packets.saturating_add(1);
        let first_video_packet = packet_kind == TrackKind::Video && !self.first_video_packet_seen;
        if first_video_packet {
            self.first_video_packet_seen = true;
        }

        let within_packet_trace_limit =
            self.logged_post_seek_packets < POST_SEEK_PACKET_TRACE_LIMIT;
        if !within_packet_trace_limit && !first_video_packet {
            return None;
        }

        self.logged_post_seek_packets = self.logged_post_seek_packets.saturating_add(1);
        Some(PostSeekPacketTraceDecision {
            packet_index: self.observed_post_seek_packets,
            first_video_packet,
        })
    }

    /// Учитывает demux packet только для Accurate preroll diagnostics.
    pub(crate) fn record_accurate_preroll_demux_packet(
        &mut self,
        packet_kind: TrackKind,
        target_or_after_selected_video: bool,
        elapsed: Duration,
    ) {
        if self.active_generation.is_none() {
            return;
        }

        record_first_elapsed(
            &mut self.accurate_preroll_stages.first_post_seek_packet_elapsed,
            elapsed,
        );

        match packet_kind {
            TrackKind::Audio => {
                increment_counter(&mut self.accurate_preroll_counters.demux_events.audio_packets);
            }
            TrackKind::Video => {
                increment_counter(&mut self.accurate_preroll_counters.demux_events.video_packets);
            }
        }

        if target_or_after_selected_video {
            record_first_elapsed(
                &mut self
                    .accurate_preroll_stages
                    .first_target_or_after_video_packet_elapsed,
                elapsed,
            );
        }
    }

    /// Учитывает EOF/TracksChanged/error demux marker для Accurate preroll diagnostics.
    pub(crate) fn record_accurate_preroll_demux_event(
        &mut self,
        event_kind: AccuratePrerollDemuxEventKind,
    ) {
        if self.active_generation.is_none() {
            return;
        }

        let counters: &mut SeekPrerollDemuxEventCountersSnapshot =
            &mut self.accurate_preroll_counters.demux_events;
        match event_kind {
            AccuratePrerollDemuxEventKind::EndOfStream => {
                increment_counter(&mut counters.end_of_stream);
            }
            AccuratePrerollDemuxEventKind::TracksChanged => {
                increment_counter(&mut counters.tracks_changed);
            }
            AccuratePrerollDemuxEventKind::Error => {
                increment_counter(&mut counters.errors);
            }
        }
    }

    /// Возвращает `true` только для первого decoded frame текущего seek trace-а.
    pub(crate) fn record_first_decoded_frame(&mut self) -> bool {
        if self.active_generation.is_none() || self.first_decoded_frame_logged {
            return false;
        }

        self.first_decoded_frame_logged = true;
        true
    }

    /// Учитывает target-or-after decoded frame для Accurate preroll diagnostics.
    pub(crate) fn record_accurate_preroll_decoded_frame(
        &mut self,
        target_or_after_frame: bool,
        elapsed: Duration,
    ) {
        if self.active_generation.is_none() || !target_or_after_frame {
            return;
        }

        record_first_elapsed(
            &mut self
                .accurate_preroll_stages
                .first_decoded_target_frame_elapsed,
            elapsed,
        );
    }

    /// Возвращает `true` только для первого queued frame текущего seek trace-а.
    pub(crate) fn record_first_queued_frame(&mut self) -> bool {
        if self.active_generation.is_none() || self.first_queued_frame_logged {
            return false;
        }

        self.first_queued_frame_logged = true;
        true
    }

    /// Учитывает target-or-after queued frame для Accurate preroll diagnostics.
    pub(crate) fn record_accurate_preroll_queued_frame(
        &mut self,
        target_or_after_frame: bool,
        elapsed: Duration,
    ) {
        if self.active_generation.is_none() || !target_or_after_frame {
            return;
        }

        record_first_elapsed(
            &mut self
                .accurate_preroll_stages
                .first_queued_target_frame_elapsed,
            elapsed,
        );
    }

    /// Возвращает `true` только для первого presented frame текущего seek trace-а.
    pub(crate) fn record_first_presented_frame(&mut self, frame_pts: Duration) -> bool {
        if self.active_generation.is_none() || self.first_presented_frame_logged {
            return false;
        }

        self.first_presented_frame_logged = true;
        self.first_presented_frame_position = Some(frame_pts);
        true
    }

    /// Учитывает target-or-after presented frame для Accurate preroll diagnostics.
    pub(crate) fn record_accurate_preroll_presented_frame(
        &mut self,
        target_or_after_frame: bool,
        elapsed: Duration,
    ) {
        if self.active_generation.is_none() || !target_or_after_frame {
            return;
        }

        record_first_elapsed(
            &mut self
                .accurate_preroll_stages
                .first_presented_target_frame_elapsed,
            elapsed,
        );
    }

    /// Возвращает PTS первого presented frame только для текущего generation-а.
    #[cfg(test)]
    #[must_use]
    pub(crate) fn first_presented_frame_position_for_generation(
        &self,
        generation: u64,
    ) -> Option<Duration> {
        (self.active_generation == Some(generation))
            .then_some(self.first_presented_frame_position)
            .flatten()
    }

    /// Возвращает `true` только для первого TracksChanged marker-а текущего seek trace-а.
    pub(crate) fn record_first_track_list_update(&mut self) -> bool {
        if self.active_generation.is_none() || self.first_track_list_update_logged {
            return false;
        }

        self.first_track_list_update_logged = true;
        true
    }

    /// Учитывает aggregate skipped audio preroll packets.
    pub(crate) fn record_skipped_audio_preroll_packet(&mut self) {
        if self.active_generation.is_none() {
            return;
        }

        increment_counter(&mut self.accurate_preroll_counters.skipped_audio_preroll_packets);
    }

    /// Учитывает video packet, отправленный decoder-у как pre-target preroll.
    pub(crate) fn record_video_preroll_packet_sent(&mut self) {
        if self.active_generation.is_none() {
            return;
        }

        increment_counter(&mut self.accurate_preroll_counters.seek_video_packets_sent);
        increment_counter(&mut self.accurate_preroll_counters.video_preroll_packets_sent);
    }

    /// Учитывает target-or-after video packet, отправленный decoder-у до landing frame.
    pub(crate) fn record_target_or_after_video_packet_sent(&mut self) {
        if self.active_generation.is_none() {
            return;
        }

        increment_counter(&mut self.accurate_preroll_counters.seek_video_packets_sent);
        increment_counter(
            &mut self
                .accurate_preroll_counters
                .target_or_after_video_packets_sent,
        );
    }

    /// Учитывает decoded pre-target frame, который не дошёл в обычный output path.
    pub(crate) fn record_decoded_pre_target_frame_dropped(&mut self) {
        if self.active_generation.is_none() {
            return;
        }

        increment_counter(
            &mut self
                .accurate_preroll_counters
                .decoded_pre_target_frames_dropped,
        );
    }

    /// Учитывает decoder/video admission backpressure во время Accurate preroll-а.
    pub(crate) fn record_decoder_backpressure_pause(&mut self) {
        if self.active_generation.is_none() {
            return;
        }

        increment_counter(&mut self.accurate_preroll_counters.decoder_backpressure_pauses);
    }

    /// Возвращает read-only snapshot Accurate preroll diagnostics.
    #[must_use]
    pub(crate) fn accurate_preroll_snapshot(
        &self,
        active: bool,
    ) -> AccurateSeekPrerollDiagnosticsSnapshot {
        if !active || self.active_generation.is_none() {
            return AccurateSeekPrerollDiagnosticsSnapshot::default();
        }

        AccurateSeekPrerollDiagnosticsSnapshot {
            active: true,
            stages: self.accurate_preroll_stages,
            counters: self.accurate_preroll_counters,
        }
    }
}
