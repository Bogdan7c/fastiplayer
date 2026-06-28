use std::time::{Duration, Instant};

use media_core::{DemuxSeekRequest, MediaTime, TrackKind};

use crate::diagnostics::{
    AccurateSeekPrerollDiagnosticsSnapshot, SeekPrerollCountersSnapshot,
    SeekPrerollDemuxEventCountersSnapshot, SeekPrerollStageDiagnosticsSnapshot,
};
use crate::{PlaybackState, SeekMode, SeekRequest};

/// Намерение возобновления playback после обычного final seek transaction.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PlaybackResumeIntent {
    /// Вернуться к paused-состоянию.
    Pause,

    /// Вернуться к playing-состоянию.
    Play,
}

impl PlaybackResumeIntent {
    /// Строит resume intent из состояния session на момент начала seek-а.
    #[must_use]
    pub const fn from_playback_state(playback_state: PlaybackState) -> Self {
        match playback_state {
            PlaybackState::Playing
            | PlaybackState::Buffering
            | PlaybackState::Seeking
            | PlaybackState::Draining => Self::Play,
            PlaybackState::Idle
            | PlaybackState::Opening
            | PlaybackState::Paused
            | PlaybackState::Scrubbing
            | PlaybackState::Ended
            | PlaybackState::Stopped
            | PlaybackState::Failed => Self::Pause,
        }
    }
}

/// Runtime state одного commit seek-а внутри playback pipeline.
#[derive(Debug, Clone, Copy)]
pub(crate) struct SeekCommitState {
    /// Поколение packets/frames, валидное для этой операции.
    pub generation: u64,

    /// Пользовательская политика seek-а, выбранная до container-level mapping-а.
    pub seek_mode: SeekMode,

    /// Цель commit-а на нормализованной media timeline.
    pub target_position: MediaTime,

    /// Фактическая позиция, на которую container переставил demuxer.
    pub actual_position: MediaTime,

    /// Момент старта операции для timeout policy.
    pub started_at: Instant,

    /// Playback-состояние, которое нужно применить после прохождения gates.
    pub resume_intent: PlaybackResumeIntent,
}

impl SeekCommitState {
    /// Возвращает `true`, если runtime должен скрыть decode preroll до user target.
    #[must_use]
    pub(crate) const fn drops_decode_preroll_before_target(self) -> bool {
        matches!(self.seek_mode, SeekMode::Accurate)
    }

    /// Возвращает clock base, от которого session должна вести playback после accepted seek-а.
    #[must_use]
    pub(crate) fn runtime_clock_base(self) -> Duration {
        if self.drops_decode_preroll_before_target() {
            return self.target_position.as_duration();
        }

        self.actual_position.as_duration()
    }

    /// Возвращает минимальный PTS кадра, который может открыть video gate.
    #[must_use]
    pub(crate) fn landing_frame_min_position(self) -> Duration {
        if self.drops_decode_preroll_before_target() {
            return self.target_position.as_duration();
        }

        self.actual_position.as_duration()
    }

    /// Перепривязывает active seek к новому packet generation после container reset.
    ///
    /// `TracksChanged` не является новым пользовательским seek-ом: target, actual,
    /// scrub intent, timeout и resume policy остаются частью той же transaction.
    #[must_use]
    pub(crate) const fn rebased_to_generation(self, generation: u64) -> Self {
        Self { generation, ..self }
    }
}

/// Session-level marker, что decoder подтвердил output-floor для Accurate seek.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) struct SeekDecoderOutputFloorState {
    /// Seek generation, на которую decoder применил floor.
    pub generation: u64,

    /// Минимальный PTS, который decoder должен публиковать наружу.
    pub floor_pts: Duration,
}

/// Лёгкое состояние compatibility scrub API без live-preview transaction-а.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub(crate) struct SimpleScrubState {
    /// Активен ли scrub gesture на уровне command contract-а.
    active: bool,

    /// Последний request, который нужно превратить в ordinary final seek при release.
    latest_request: Option<SeekRequest>,

    /// Последнее подтверждённое playback state до входа в public `Scrubbing`.
    confirmed_playback_state: Option<PlaybackState>,
}

/// Закрытый lightweight scrub вместе с state, к которому надо вернуться до command route-а.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) struct FinishedSimpleScrub {
    /// Последний request, который `EndScrub` имеет право закоммитить.
    latest_request: Option<SeekRequest>,

    /// Playback state до scrub; от него считаются cancel-first команды и seek resume intent.
    confirmed_playback_state: PlaybackState,
}

impl FinishedSimpleScrub {
    /// Возвращает latest target для `EndScrub`.
    #[must_use]
    pub(crate) const fn latest_request(self) -> Option<SeekRequest> {
        self.latest_request
    }

    /// Возвращает подтверждённый state до входа в public `Scrubbing`.
    #[must_use]
    pub(crate) const fn confirmed_playback_state(self) -> PlaybackState {
        self.confirmed_playback_state
    }
}

impl SimpleScrubState {
    /// Начинает scrub gesture и сбрасывает старую release-цель.
    pub(crate) fn begin(&mut self, confirmed_playback_state: PlaybackState) {
        if !self.active {
            self.confirmed_playback_state = Some(confirmed_playback_state);
        }
        self.active = true;
        self.latest_request = None;
    }

    /// Запоминает latest target по latest-wins policy без запуска demux seek-а.
    pub(crate) fn store_request(
        &mut self,
        request: SeekRequest,
        confirmed_playback_state: PlaybackState,
    ) {
        if !self.active {
            self.confirmed_playback_state = Some(confirmed_playback_state);
        }
        self.active = true;
        self.latest_request = Some(request);
    }

    /// Закрывает scrub state и возвращает release target, если gesture был активен.
    pub(crate) fn finish_active(&mut self) -> Option<FinishedSimpleScrub> {
        if !self.active {
            self.clear();
            return None;
        }

        let finished_scrub = FinishedSimpleScrub {
            latest_request: self.latest_request.take(),
            confirmed_playback_state: self
                .confirmed_playback_state
                .unwrap_or(PlaybackState::Paused),
        };
        self.clear();
        Some(finished_scrub)
    }

    /// Сбрасывает только lightweight scrub state, не трогая active seek transaction.
    pub(crate) fn clear(&mut self) {
        self.active = false;
        self.latest_request = None;
        self.confirmed_playback_state = None;
    }

    /// Проверяет, открыт ли scrub gesture.
    #[must_use]
    pub(crate) const fn active(&self) -> bool {
        self.active
    }

    /// Возвращает сохранённый latest request для diagnostics/tests.
    #[must_use]
    #[cfg(test)]
    pub(crate) const fn latest_request(&self) -> Option<SeekRequest> {
        self.latest_request
    }
}

/// Сколько первых demux packets после accepted seek попадает в debug trace.
pub(crate) const POST_SEEK_PACKET_TRACE_LIMIT: usize = 8;

/// Решение helper-а: нужно ли писать compact log для очередного post-seek packet-а.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) struct PostSeekPacketTraceDecision {
    /// Номер packet-а в post-seek последовательности, начиная с 1.
    pub(crate) packet_index: usize,

    /// Это первый video packet, увиденный после accepted seek.
    pub(crate) first_video_packet: bool,
}

/// Минимальное диагностическое состояние одного seek trace-а.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub(crate) struct SeekTraceState {
    /// Generation accepted seek transaction-а, для которого собирается trace.
    active_generation: Option<u64>,

    /// Сколько demux packets увидели после accepted seek.
    observed_post_seek_packets: usize,

    /// Сколько packet logs уже записали для текущего trace-а.
    logged_post_seek_packets: usize,

    /// Был ли уже зафиксирован первый post-seek video packet.
    first_video_packet_seen: bool,

    /// Был ли уже зафиксирован первый decoded frame после seek.
    first_decoded_frame_logged: bool,

    /// Был ли уже зафиксирован первый queued frame после seek.
    first_queued_frame_logged: bool,

    /// Был ли уже зафиксирован первый presented frame после seek.
    first_presented_frame_logged: bool,

    /// PTS первого presented frame текущего seek trace-а.
    first_presented_frame_position: Option<Duration>,

    /// Был ли уже зафиксирован первый TracksChanged marker после seek.
    first_track_list_update_logged: bool,

    /// Elapsed timings Accurate seek preroll-а.
    accurate_preroll_stages: SeekPrerollStageDiagnosticsSnapshot,

    /// Aggregate counters Accurate seek preroll-а.
    accurate_preroll_counters: SeekPrerollCountersSnapshot,
}

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

/// Non-packet demux event kind для Accurate preroll counters.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum AccuratePrerollDemuxEventKind {
    /// Demuxer дошёл до EOF до закрытия active seek-а.
    EndOfStream,

    /// Demuxer потребовал обновить track list.
    TracksChanged,

    /// Demuxer вернул fatal read error.
    Error,
}

/// Session-owned runtime state seek domain-а без decoder/demux ownership.
#[derive(Debug, Clone, Copy, Default)]
pub(crate) struct SeekRuntimeState {
    /// Активная операция final seek commit-а, если player ждёт pre-roll/gates.
    commit: Option<SeekCommitState>,

    /// Decoder-side output-floor, подтверждённый backend-ом для Accurate preroll.
    decoder_output_floor: Option<SeekDecoderOutputFloorState>,

    /// Одноразовые trace markers accepted seek-а.
    trace: SeekTraceState,

    /// Lightweight scrub gesture для старого command contract-а.
    simple_scrub: SimpleScrubState,

    /// PTS свежего near-EOF fallback frame-а, который уже был представлен.
    eof_fallback_video_position: Option<MediaTime>,
}

impl SeekRuntimeState {
    /// Проверяет, открыт ли seek commit.
    #[must_use]
    pub(crate) const fn has_active_commit(&self) -> bool {
        self.commit.is_some()
    }

    /// Возвращает active commit by value для gate/scheduler решений.
    #[must_use]
    pub(crate) const fn active_commit(&self) -> Option<SeekCommitState> {
        self.commit
    }

    /// Возвращает mutable active commit, когда command меняет только resume intent.
    pub(crate) fn active_commit_mut(&mut self) -> Option<&mut SeekCommitState> {
        self.commit.as_mut()
    }

    /// Открывает commit state после accepted demux seek.
    pub(crate) fn set_active_commit(&mut self, seek_commit: SeekCommitState) {
        self.commit = Some(seek_commit);
    }

    /// Закрывает commit state без изменения trace/scrub/fallback markers.
    pub(crate) fn clear_active_commit(&mut self) {
        self.commit = None;
    }

    /// Запоминает, что decoder подтвердил Accurate output-floor для указанного generation.
    pub(crate) fn mark_decoder_output_floor_applied(
        &mut self,
        generation: u64,
        floor_pts: Duration,
    ) {
        self.decoder_output_floor = Some(SeekDecoderOutputFloorState {
            generation,
            floor_pts,
        });
    }

    /// Возвращает активный decoder-side floor marker без доступа к decoder internals.
    #[must_use]
    pub(crate) const fn decoder_output_floor(&self) -> Option<SeekDecoderOutputFloorState> {
        self.decoder_output_floor
    }

    /// Проверяет, подтверждён ли decoder-side floor для generation конкретного packet-а.
    #[must_use]
    pub(crate) const fn decoder_output_floor_applied_for_generation(
        &self,
        generation: u64,
    ) -> bool {
        matches!(
            self.decoder_output_floor,
            Some(floor) if floor.generation == generation
        )
    }

    /// Сбрасывает только marker decoder-side floor; decoder command вызывается отдельно.
    pub(crate) fn clear_decoder_output_floor(&mut self) {
        self.decoder_output_floor = None;
    }

    /// Перепривязывает active commit к новому packet generation после demux reset-а.
    pub(crate) fn rebase_active_commit_to_generation(
        &mut self,
        generation: u64,
    ) -> Option<SeekCommitState> {
        let active_commit = self.commit?;
        let rebased_commit = active_commit.rebased_to_generation(generation);
        self.commit = Some(rebased_commit);
        Some(rebased_commit)
    }

    /// Начинает новый trace для accepted seek generation-а.
    pub(crate) fn begin_trace(&mut self, generation: u64) {
        self.trace.begin(generation);
    }

    /// Очищает trace markers без изменения commit state.
    pub(crate) fn clear_trace(&mut self) {
        self.trace.clear();
    }

    /// Учитывает post-seek demux packet для compact trace logging.
    pub(crate) fn record_post_seek_packet(
        &mut self,
        packet_kind: TrackKind,
    ) -> Option<PostSeekPacketTraceDecision> {
        self.trace.record_post_seek_packet(packet_kind)
    }

    /// Учитывает packet-level demux diagnostics active Accurate preroll-а.
    pub(crate) fn record_accurate_preroll_demux_packet(
        &mut self,
        packet_kind: TrackKind,
        target_or_after_selected_video: bool,
        elapsed: Duration,
    ) {
        self.trace.record_accurate_preroll_demux_packet(
            packet_kind,
            target_or_after_selected_video,
            elapsed,
        );
    }

    /// Учитывает lifecycle/error demux diagnostics active Accurate preroll-а.
    pub(crate) fn record_accurate_preroll_demux_event(
        &mut self,
        event_kind: AccuratePrerollDemuxEventKind,
    ) {
        self.trace.record_accurate_preroll_demux_event(event_kind);
    }

    /// Возвращает `true` только для первого decoded frame текущего trace-а.
    pub(crate) fn record_first_decoded_frame(&mut self) -> bool {
        self.trace.record_first_decoded_frame()
    }

    /// Учитывает target decoded frame для active Accurate preroll diagnostics.
    pub(crate) fn record_accurate_preroll_decoded_frame(
        &mut self,
        target_or_after_frame: bool,
        elapsed: Duration,
    ) {
        self.trace
            .record_accurate_preroll_decoded_frame(target_or_after_frame, elapsed);
    }

    /// Возвращает `true` только для первого queued frame текущего trace-а.
    pub(crate) fn record_first_queued_frame(&mut self) -> bool {
        self.trace.record_first_queued_frame()
    }

    /// Учитывает target queued frame для active Accurate preroll diagnostics.
    pub(crate) fn record_accurate_preroll_queued_frame(
        &mut self,
        target_or_after_frame: bool,
        elapsed: Duration,
    ) {
        self.trace
            .record_accurate_preroll_queued_frame(target_or_after_frame, elapsed);
    }

    /// Возвращает `true` только для первого presented frame текущего trace-а.
    pub(crate) fn record_first_presented_frame(&mut self, frame_pts: Duration) -> bool {
        self.trace.record_first_presented_frame(frame_pts)
    }

    /// Учитывает target presented frame для active Accurate preroll diagnostics.
    pub(crate) fn record_accurate_preroll_presented_frame(
        &mut self,
        target_or_after_frame: bool,
        elapsed: Duration,
    ) {
        self.trace
            .record_accurate_preroll_presented_frame(target_or_after_frame, elapsed);
    }

    /// Возвращает `true` только для первого TracksChanged marker-а текущего trace-а.
    pub(crate) fn record_first_track_list_update(&mut self) -> bool {
        self.trace.record_first_track_list_update()
    }

    /// Учитывает dropped audio preroll packet active Accurate seek-а.
    pub(crate) fn record_skipped_audio_preroll_packet(&mut self) {
        self.trace.record_skipped_audio_preroll_packet();
    }

    /// Учитывает pre-target video packet, отправленный decoder-у.
    pub(crate) fn record_video_preroll_packet_sent(&mut self) {
        self.trace.record_video_preroll_packet_sent();
    }

    /// Учитывает target-or-after video packet, отправленный decoder-у.
    pub(crate) fn record_target_or_after_video_packet_sent(&mut self) {
        self.trace.record_target_or_after_video_packet_sent();
    }

    /// Учитывает decoded pre-target frame, не попавший в обычный output path.
    pub(crate) fn record_decoded_pre_target_frame_dropped(&mut self) {
        self.trace.record_decoded_pre_target_frame_dropped();
    }

    /// Учитывает decoder/video admission backpressure active Accurate preroll-а.
    pub(crate) fn record_decoder_backpressure_pause(&mut self) {
        self.trace.record_decoder_backpressure_pause();
    }

    /// Возвращает read-only snapshot Accurate preroll diagnostics.
    #[must_use]
    pub(crate) fn accurate_preroll_snapshot(
        &self,
        active: bool,
    ) -> AccurateSeekPrerollDiagnosticsSnapshot {
        self.trace.accurate_preroll_snapshot(active)
    }

    /// Начинает lightweight scrub gesture.
    pub(crate) fn begin_simple_scrub(&mut self, confirmed_playback_state: PlaybackState) {
        self.simple_scrub.begin(confirmed_playback_state);
    }

    /// Запоминает latest scrub request по latest-wins policy.
    pub(crate) fn store_simple_scrub_request(
        &mut self,
        request: SeekRequest,
        confirmed_playback_state: PlaybackState,
    ) {
        self.simple_scrub
            .store_request(request, confirmed_playback_state);
    }

    /// Закрывает active scrub gesture и возвращает state для final/cancel route-а.
    pub(crate) fn finish_active_simple_scrub(&mut self) -> Option<FinishedSimpleScrub> {
        self.simple_scrub.finish_active()
    }

    /// Сбрасывает lightweight scrub state без изменения active commit-а.
    pub(crate) fn clear_simple_scrub(&mut self) {
        self.simple_scrub.clear();
    }

    /// Проверяет active scrub state для command/tests.
    #[must_use]
    pub(crate) const fn simple_scrub_active(&self) -> bool {
        self.simple_scrub.active()
    }

    /// Возвращает latest scrub request для diagnostics/tests.
    #[must_use]
    #[cfg(test)]
    pub(crate) const fn simple_scrub_latest_request(&self) -> Option<SeekRequest> {
        self.simple_scrub.latest_request()
    }

    /// Устанавливает lightweight scrub state напрямую только для focused boundary tests.
    #[cfg(test)]
    pub(crate) fn set_simple_scrub_state_for_tests(
        &mut self,
        active: bool,
        latest_request: Option<SeekRequest>,
    ) {
        self.simple_scrub.active = active;
        self.simple_scrub.latest_request = latest_request;
        self.simple_scrub.confirmed_playback_state = active.then_some(PlaybackState::Paused);
    }

    /// Проверяет, есть ли pending near-EOF fallback marker.
    #[must_use]
    pub(crate) const fn has_eof_fallback_video_position(&self) -> bool {
        self.eof_fallback_video_position.is_some()
    }

    /// Возвращает PTS presented EOF fallback frame-а.
    #[must_use]
    pub(crate) const fn eof_fallback_video_position(&self) -> Option<MediaTime> {
        self.eof_fallback_video_position
    }

    /// Запоминает PTS presented EOF fallback frame-а.
    pub(crate) fn set_eof_fallback_video_position(&mut self, position: MediaTime) {
        self.eof_fallback_video_position = Some(position);
    }

    /// Очищает EOF fallback marker без release pipeline-owned frame-а.
    pub(crate) fn clear_eof_fallback_video_position(&mut self) {
        self.eof_fallback_video_position = None;
    }
}

/// Политика выбора playback position при закрытии final seek-а.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum FinalSeekCommitPosition {
    /// Requested target становится clock base.
    Target { position: Duration },

    /// Explicit keyframe-before seek коммитит реально показанный frame текущего generation-а.
    PresentedFrame { position: Duration },

    /// Near-EOF fallback коммитит PTS реально показанного fallback frame-а.
    EofFallbackFrame { position: Duration },
}

impl FinalSeekCommitPosition {
    /// Возвращает позицию, которую нужно опубликовать как committed playback position.
    #[must_use]
    pub(crate) const fn position(self) -> Duration {
        match self {
            Self::Target { position }
            | Self::PresentedFrame { position }
            | Self::EofFallbackFrame { position } => position,
        }
    }

    /// Stable label для structured logs и focused regression tests.
    #[must_use]
    pub(crate) const fn policy_name(self) -> &'static str {
        match self {
            Self::Target { .. } => "target",
            Self::PresentedFrame { .. } => "presented-frame",
            Self::EofFallbackFrame { .. } => "eof-fallback-frame",
        }
    }
}

/// Ошибка выбора container-level seek request-а до изменения runtime pipeline.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum SeekDemuxRequestError {
    /// Запрошенный public `SeekMode` пока не имеет честной реализации в demux contract.
    UnsupportedSeekMode {
        /// Режим из пользовательской команды, который нельзя молча заменить другим.
        mode: SeekMode,
    },
}

/// Выбирает final demux seek request для текущего seek transaction-а.
///
/// Video accurate seek остаётся decode-safe: demuxer начинает до target,
/// а session декодирует и отбрасывает preroll до пользовательской цели.
/// Audio-only accurate seek сохраняет container-accurate contract без video preroll.
pub(crate) fn demux_seek_request_for_transaction(
    has_video_track: bool,
    target_duration: Duration,
    seek_mode: SeekMode,
) -> Result<DemuxSeekRequest, SeekDemuxRequestError> {
    if seek_mode == SeekMode::KeyframeAfter {
        return Err(SeekDemuxRequestError::UnsupportedSeekMode { mode: seek_mode });
    }

    Ok(final_demux_seek_request(
        has_video_track,
        target_duration,
        seek_mode,
    ))
}

/// Строит финальный demux request без потери public `SeekMode`.
fn final_demux_seek_request(
    has_video_track: bool,
    target_duration: Duration,
    seek_mode: SeekMode,
) -> DemuxSeekRequest {
    match seek_mode {
        SeekMode::Accurate if has_video_track => {
            DemuxSeekRequest::decode_point_before(target_duration)
        }
        SeekMode::Accurate => DemuxSeekRequest::accurate(target_duration),
        SeekMode::KeyframeBefore => DemuxSeekRequest::decode_point_before(target_duration),
        SeekMode::KeyframeAfter => unreachable!("KeyframeAfter rejected before final mapping"),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use media_core::DemuxSeekMode;

    fn request_mode(
        has_video_track: bool,
        seek_mode: SeekMode,
    ) -> Result<DemuxSeekMode, SeekDemuxRequestError> {
        demux_seek_request_for_transaction(has_video_track, Duration::from_millis(1_500), seek_mode)
            .map(|request| request.mode)
    }

    #[test]
    fn accurate_audio_only_final_seek_stays_container_accurate() {
        let mode = request_mode(false, SeekMode::Accurate)
            .expect("audio-only accurate seek должен поддерживаться");

        assert_eq!(mode, DemuxSeekMode::Accurate);
    }

    #[test]
    fn accurate_video_final_seek_uses_decode_safe_preroll_request() {
        let mode = request_mode(true, SeekMode::Accurate)
            .expect("video accurate seek должен поддерживаться через preroll");

        assert_eq!(mode, DemuxSeekMode::DecodePointBefore);
    }

    #[test]
    fn keyframe_before_final_seek_maps_to_decode_point_before() {
        let mode = request_mode(true, SeekMode::KeyframeBefore)
            .expect("keyframe-before seek должен поддерживаться");

        assert_eq!(mode, DemuxSeekMode::DecodePointBefore);
    }

    #[test]
    fn keyframe_after_is_explicitly_unsupported() {
        let error = request_mode(true, SeekMode::KeyframeAfter)
            .expect_err("keyframe-after пока должен отклоняться явно");

        assert_eq!(
            error,
            SeekDemuxRequestError::UnsupportedSeekMode {
                mode: SeekMode::KeyframeAfter,
            }
        );
    }
}
