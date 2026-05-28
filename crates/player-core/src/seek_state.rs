use std::time::{Duration, Instant};

use media_core::{DemuxSeekRequest, MediaTime, TrackKind};

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
    /// Перепривязывает active seek к новому packet generation после container reset.
    ///
    /// `TracksChanged` не является новым пользовательским seek-ом: target, actual,
    /// scrub intent, timeout и resume policy остаются частью той же transaction.
    #[must_use]
    pub(crate) const fn rebased_to_generation(self, generation: u64) -> Self {
        Self { generation, ..self }
    }
}

/// Лёгкое состояние compatibility scrub API без live-preview transaction-а.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub(crate) struct SimpleScrubState {
    /// Активен ли scrub gesture на уровне command contract-а.
    active: bool,

    /// Последний request, который нужно превратить в ordinary final seek при release.
    latest_request: Option<SeekRequest>,
}

impl SimpleScrubState {
    /// Начинает scrub gesture и сбрасывает старую release-цель.
    pub(crate) fn begin(&mut self) {
        self.active = true;
        self.latest_request = None;
    }

    /// Запоминает latest target по latest-wins policy без запуска demux seek-а.
    pub(crate) fn store_request(&mut self, request: SeekRequest) {
        self.active = true;
        self.latest_request = Some(request);
    }

    /// Закрывает scrub state и возвращает release target, если gesture был активен.
    pub(crate) fn finish_and_take_latest_request(&mut self) -> Option<SeekRequest> {
        let latest_request = self.active.then(|| self.latest_request.take()).flatten();
        self.clear();
        latest_request
    }

    /// Сбрасывает только lightweight scrub state, не трогая active seek transaction.
    pub(crate) fn clear(&mut self) {
        self.active = false;
        self.latest_request = None;
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

    /// Возвращает `true` только для первого decoded frame текущего seek trace-а.
    pub(crate) fn record_first_decoded_frame(&mut self) -> bool {
        if self.active_generation.is_none() || self.first_decoded_frame_logged {
            return false;
        }

        self.first_decoded_frame_logged = true;
        true
    }

    /// Возвращает `true` только для первого queued frame текущего seek trace-а.
    pub(crate) fn record_first_queued_frame(&mut self) -> bool {
        if self.active_generation.is_none() || self.first_queued_frame_logged {
            return false;
        }

        self.first_queued_frame_logged = true;
        true
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

    /// Возвращает PTS первого presented frame только для текущего generation-а.
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
}

/// Session-owned runtime state seek domain-а без decoder/demux ownership.
#[derive(Debug, Clone, Copy, Default)]
pub(crate) struct SeekRuntimeState {
    /// Активная операция final seek commit-а, если player ждёт pre-roll/gates.
    commit: Option<SeekCommitState>,

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

    /// Возвращает `true` только для первого decoded frame текущего trace-а.
    pub(crate) fn record_first_decoded_frame(&mut self) -> bool {
        self.trace.record_first_decoded_frame()
    }

    /// Возвращает `true` только для первого queued frame текущего trace-а.
    pub(crate) fn record_first_queued_frame(&mut self) -> bool {
        self.trace.record_first_queued_frame()
    }

    /// Возвращает `true` только для первого presented frame текущего trace-а.
    pub(crate) fn record_first_presented_frame(&mut self, frame_pts: Duration) -> bool {
        self.trace.record_first_presented_frame(frame_pts)
    }

    /// Возвращает PTS первого presented frame для текущего seek generation-а.
    #[must_use]
    pub(crate) fn first_presented_frame_position_for_generation(
        &self,
        generation: u64,
    ) -> Option<Duration> {
        self.trace
            .first_presented_frame_position_for_generation(generation)
    }

    /// Возвращает `true` только для первого TracksChanged marker-а текущего trace-а.
    pub(crate) fn record_first_track_list_update(&mut self) -> bool {
        self.trace.record_first_track_list_update()
    }

    /// Начинает lightweight scrub gesture.
    pub(crate) fn begin_simple_scrub(&mut self) {
        self.simple_scrub.begin();
    }

    /// Запоминает latest scrub request по latest-wins policy.
    pub(crate) fn store_simple_scrub_request(&mut self, request: SeekRequest) {
        self.simple_scrub.store_request(request);
    }

    /// Закрывает active scrub gesture и возвращает latest target для final seek-а.
    pub(crate) fn finish_simple_scrub_and_take_latest_request(&mut self) -> Option<SeekRequest> {
        self.simple_scrub.finish_and_take_latest_request()
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

    /// Обычный video seek коммитит первый свежий frame текущего seek generation-а.
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
/// а session закрывает transition по первому свежему frame-у текущего generation-а.
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
