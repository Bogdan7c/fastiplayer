use std::time::{Duration, Instant};

use frame_server_core::{
    LiveScrubDiagnostics, PlaybackGeneration, ScrubFrameTiming, ScrubGeneration,
    ScrubGenerationToken, ScrubRequestKind, ScrubStaleReason, ScrubTargetContext,
};
use media_core::{DemuxSeekRequest, MediaTime, TrackKind};
use video_present_core::VideoPresentFrameIdentity;

use crate::diagnostics::{
    AccurateSeekPrerollDiagnosticsSnapshot, SeekPrerollCountersSnapshot,
    SeekPrerollDemuxEventCountersSnapshot, SeekPrerollStageDiagnosticsSnapshot,
};
use crate::seek_acceptance_telemetry::{
    SeekAcceptanceTelemetry, SeekPositionProgressEvidence, SeekTargetPresentationEvidence,
};
use crate::{PlaybackState, PreparedDemuxSeekLandingPolicy, SeekMode, SeekRequest};

mod runtime;
mod trace;

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

/// Timeline range, который обязан удерживать target до завершения seek commit-а.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum SeekTargetRetention {
    /// Пользовательская exact цель должна оставаться в packet-proven public range.
    ExactPublicRange,
    /// Recovery к live edge остаётся валидным в authoritative manifest availability.
    LiveAvailability,
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

    /// Source-owned contract выбора presentation/audio floor и final playback position.
    pub landing_policy: PreparedDemuxSeekLandingPolicy,

    /// Момент старта операции для timeout policy.
    pub started_at: Instant,

    /// Момент принятия public final seek для честной seek-to-presentation метрики.
    pub public_accepted_at: Instant,

    /// Playback-состояние, которое нужно применить после прохождения gates.
    pub resume_intent: PlaybackResumeIntent,

    /// Range owner, который имеет право инвалидировать target во время refresh-а.
    pub target_retention: SeekTargetRetention,
}

/// Public seek, который уже вошёл в S17A SeekLanding route, но ещё не получил
/// scrub generation от `frame-server-core`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) struct PendingSeekLandingState {
    /// Planned scrub identity для route-а. Это не decoder packet generation.
    generation: ScrubGenerationToken,

    /// Пользовательский seek mode, который должен попасть в final commit gates.
    seek_mode: SeekMode,

    /// Состояние, в которое session должна вернуться после успешного landing.
    resume_intent: PlaybackResumeIntent,

    /// Кто владеет моментом final commit-а для этого route-а.
    route: SeekLandingRoute,
}

/// Active S17 SeekLanding transaction.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) struct ActiveSeekLandingState {
    /// Двойной guard state-machine: playback generation + nested scrub generation.
    generation: ScrubGenerationToken,

    /// Route исполнения: cold decode или prepared override без decoder loop.
    execution: SeekLandingExecution,

    /// Пользовательский seek mode, сохранённый до demux-level mapping-а.
    seek_mode: SeekMode,

    /// Resume policy, выбранная в момент входа в one-shot route.
    resume_intent: PlaybackResumeIntent,

    /// Кто владеет моментом final commit-а для этого route-а.
    route: SeekLandingRoute,

    /// Фактическая позиция, куда demuxer принял decode-point seek.
    actual_decode_position: Option<MediaTime>,

    /// Pipeline seek generation для cold decoder route-а.
    ///
    /// Prepared visual override не декодирует новый frame, поэтому не имеет
    /// отдельного decoder generation.
    decode_seek_generation: Option<u64>,
}

/// Последний live-scrub кадр, который player-owned scheduler сделал текущим presented frame.
///
/// Context, timing и stable identity хранятся вместе: release не должен собирать
/// позицию заново из UI cursor-а или принимать совпавший только по PTS stale frame.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) struct VisibleScrubPreview {
    /// Полный source/backend/track/generation context active live target-а.
    pub context: ScrubTargetContext,

    /// Media timing реально представленного кадра.
    pub timing: ScrubFrameTiming,

    /// Stable render/decoder/resource/PTS identity того же кадра.
    pub frame_identity: VideoPresentFrameIdentity,
}

/// Commit ownership active SeekLanding route-а.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum SeekLandingRoute {
    /// Обычный one-shot seek сам commit-ится, когда gates готовы.
    OneShot,

    /// Timeline live scrub декодит preview во время drag, но commit разрешает только release.
    LiveScrub {
        /// `EndScrub` уже запросил final commit этого target-а.
        commit_requested: bool,

        /// Bounded diagnostics текущего drag-а; не влияет на lifecycle.
        diagnostics: Option<LiveScrubDiagnostics>,
    },
}

impl SeekLandingRoute {
    /// Создаёт live route до release: preview можно декодить, commit ещё нельзя.
    #[must_use]
    pub(crate) const fn live_scrub_preview(diagnostics: Option<LiveScrubDiagnostics>) -> Self {
        Self::LiveScrub {
            commit_requested: false,
            diagnostics,
        }
    }

    /// Возвращает `true`, если final commit можно применять на готовых gates.
    #[must_use]
    pub(crate) const fn commit_allowed(self) -> bool {
        match self {
            Self::OneShot => true,
            Self::LiveScrub {
                commit_requested, ..
            } => commit_requested,
        }
    }

    /// Возвращает `true`, если route принадлежит live timeline drag.
    #[must_use]
    pub(crate) const fn is_live_scrub(self) -> bool {
        matches!(self, Self::LiveScrub { .. })
    }

    /// Возвращает live-scrub diagnostics, если route принадлежит active drag-а.
    #[must_use]
    pub(crate) const fn live_scrub_diagnostics(self) -> Option<LiveScrubDiagnostics> {
        match self {
            Self::OneShot => None,
            Self::LiveScrub { diagnostics, .. } => diagnostics,
        }
    }

    /// Возвращает neutral request kind для scrub events/diagnostics.
    #[must_use]
    pub(crate) const fn request_kind(self) -> ScrubRequestKind {
        match self {
            Self::OneShot => ScrubRequestKind::SeekLanding,
            Self::LiveScrub { .. } => ScrubRequestKind::LiveScrub,
        }
    }
}

/// Route исполнения active SeekLanding без неявного `bool` на callsite.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum SeekLandingExecution {
    /// S17A cold route: reused playback decoder должен demux/decode exact frame.
    ReusedDecoderColdDecode,
}

impl SeekLandingExecution {
    /// Разрешает ли route demux/decode loop во время public `Scrubbing`.
    #[must_use]
    pub(crate) const fn decode_active(self) -> bool {
        true
    }
}

/// Ошибка начала SeekLanding playback generation.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum SeekLandingGenerationStartError {
    /// Следующее playback generation переполнилось.
    GenerationOverflow,

    /// State-machine generation не совпала с owner-side guard-ами.
    Stale(ScrubStaleReason),
}

impl ActiveSeekLandingState {
    /// Возвращает полный guard token active landing-а без раскрытия полей state struct.
    #[must_use]
    pub(crate) const fn generation(self) -> ScrubGenerationToken {
        self.generation
    }

    /// Проверяет, относится ли driver intent/outcome к текущему active landing.
    #[must_use]
    pub(crate) fn matches_generation(self, generation: ScrubGenerationToken) -> bool {
        self.generation == generation
    }

    /// Возвращает исходный public seek mode для commit policy.
    #[must_use]
    pub(crate) const fn seek_mode(self) -> SeekMode {
        self.seek_mode
    }

    /// Возвращает resume intent, выбранный до входа в public `Scrubbing`.
    #[must_use]
    pub(crate) const fn resume_intent(self) -> PlaybackResumeIntent {
        self.resume_intent
    }

    /// Возвращает commit owner route без раскрытия layout-а state struct.
    #[must_use]
    pub(crate) const fn route(self) -> SeekLandingRoute {
        self.route
    }

    /// Проверяет, должен ли tick продолжать cold decode route.
    #[must_use]
    pub(crate) const fn decode_active(self) -> bool {
        self.execution.decode_active()
    }

    /// Возвращает accepted demux position, если demux seek уже состоялся.
    #[must_use]
    pub(crate) const fn actual_decode_position(self) -> Option<MediaTime> {
        self.actual_decode_position
    }

    /// Возвращает pipeline seek generation, если route действительно декодирует.
    #[must_use]
    pub(crate) const fn decode_seek_generation(self) -> Option<u64> {
        self.decode_seek_generation
    }

    /// Проверяет, может ли commit generation относиться к этому landing-у.
    #[must_use]
    pub(crate) fn matches_commit_generation(self, commit_generation: u64) -> bool {
        self.decode_seek_generation == Some(commit_generation)
            || self.generation.playback_generation == PlaybackGeneration::new(commit_generation)
    }
}

impl SeekCommitState {
    /// Сообщает, что source доказал post-target actual как новый playback authority.
    #[must_use]
    pub(crate) const fn presents_from_actual_position(self) -> bool {
        matches!(
            self.landing_policy,
            PreparedDemuxSeekLandingPolicy::AuthoritativePostTarget
        )
    }

    /// Возвращает `true`, если runtime должен скрыть decode preroll до user target.
    #[must_use]
    pub(crate) const fn drops_decode_preroll_before_target(self) -> bool {
        matches!(self.seek_mode, SeekMode::Accurate)
    }

    /// Возвращает clock base, от которого session должна вести playback после accepted seek-а.
    #[must_use]
    pub(crate) fn runtime_clock_base(self) -> Duration {
        if self.presents_from_actual_position() {
            return self.actual_position.as_duration();
        }
        if self.drops_decode_preroll_before_target() {
            return self.target_position.as_duration();
        }

        self.actual_position.as_duration()
    }

    /// Возвращает минимальный PTS кадра, который может открыть video gate.
    #[must_use]
    pub(crate) fn landing_frame_min_position(self) -> Duration {
        if self.presents_from_actual_position() {
            return self.actual_position.as_duration();
        }
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

    /// Monotonic origin принятого `BeginScrub`; wall clock сюда не попадает.
    began_at: Option<Instant>,

    /// Последний request, который release передаёт в единый SeekLanding route.
    latest_request: Option<SeekRequest>,

    /// Последнее подтверждённое playback state до входа в public `Scrubbing`.
    confirmed_playback_state: Option<PlaybackState>,

    /// Diagnostics live drag-а, если этот simple scrub запущен real live route-ом.
    live_scrub_diagnostics: Option<LiveScrubDiagnostics>,
}

/// Закрытый lightweight scrub вместе с state, к которому надо вернуться до command route-а.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) struct FinishedSimpleScrub {
    /// Последний request, который `EndScrub` имеет право закоммитить.
    latest_request: Option<SeekRequest>,

    /// Playback state до scrub; от него считаются cancel-first команды и seek resume intent.
    confirmed_playback_state: PlaybackState,

    /// Последний bounded live-scrub diagnostics state на момент release/cancel.
    live_scrub_diagnostics: Option<LiveScrubDiagnostics>,
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

    /// Возвращает live-scrub diagnostics, накопленные до release/cancel.
    #[must_use]
    pub(crate) const fn live_scrub_diagnostics(self) -> Option<LiveScrubDiagnostics> {
        self.live_scrub_diagnostics
    }
}

impl SimpleScrubState {
    /// Начинает scrub gesture и сбрасывает старую release-цель.
    pub(crate) fn begin(
        &mut self,
        confirmed_playback_state: PlaybackState,
        live_scrub_diagnostics: Option<LiveScrubDiagnostics>,
    ) {
        if !self.active {
            self.confirmed_playback_state = Some(confirmed_playback_state);
            self.began_at = Some(Instant::now());
        }
        self.active = true;
        self.latest_request = None;
        self.live_scrub_diagnostics = live_scrub_diagnostics;
    }

    /// Запоминает latest target по latest-wins policy без запуска demux seek-а.
    pub(crate) fn store_request(
        &mut self,
        request: SeekRequest,
        confirmed_playback_state: PlaybackState,
        live_scrub_diagnostics: Option<LiveScrubDiagnostics>,
    ) {
        if !self.active {
            self.confirmed_playback_state = Some(confirmed_playback_state);
            self.began_at = Some(Instant::now());
        }
        self.active = true;
        self.latest_request = Some(request);
        if let Some(live_scrub_diagnostics) = live_scrub_diagnostics {
            self.live_scrub_diagnostics = Some(live_scrub_diagnostics);
        }
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
            live_scrub_diagnostics: self.live_scrub_diagnostics,
        };
        self.clear();
        Some(finished_scrub)
    }

    /// Сбрасывает только lightweight scrub state, не трогая active seek transaction.
    pub(crate) fn clear(&mut self) {
        self.active = false;
        self.began_at = None;
        self.latest_request = None;
        self.confirmed_playback_state = None;
        self.live_scrub_diagnostics = None;
    }

    /// Проверяет, открыт ли scrub gesture.
    #[must_use]
    pub(crate) const fn active(&self) -> bool {
        self.active
    }

    /// Возвращает owner-monotonic возраст текущего scrub gesture-а.
    #[must_use]
    pub(crate) fn elapsed_since_begin(&self) -> Option<Duration> {
        self.began_at.map(|began_at| began_at.elapsed())
    }

    /// Возвращает state, подтверждённый до входа в scrub gesture.
    #[must_use]
    pub(crate) const fn confirmed_playback_state(&self) -> Option<PlaybackState> {
        self.confirmed_playback_state
    }

    /// Возвращает live-scrub diagnostics текущего gesture-а без изменения lifecycle.
    #[must_use]
    pub(crate) const fn live_scrub_diagnostics(&self) -> Option<LiveScrubDiagnostics> {
        self.live_scrub_diagnostics
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

    /// Bounded proof state presentation/commit/progress acceptance path-а.
    acceptance_telemetry: SeekAcceptanceTelemetry,

    /// Lightweight scrub gesture для старого command contract-а.
    simple_scrub: SimpleScrubState,

    /// One-shot SeekLanding request, ожидающий scrub generation от state-machine.
    pending_seek_landing: Option<PendingSeekLandingState>,

    /// Active one-shot SeekLanding transaction: cold decode или prepared override route.
    active_seek_landing: Option<ActiveSeekLandingState>,

    /// Последний действительно представленный и ещё проверяемый live-scrub preview.
    visible_scrub_preview: Option<VisibleScrubPreview>,

    /// PTS свежего near-EOF fallback frame-а, который уже был представлен.
    eof_fallback_video_position: Option<MediaTime>,
}

/// Политика выбора playback position при закрытии final seek-а.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum FinalSeekCommitPosition {
    /// Requested target становится clock base.
    Target { position: Duration },

    /// Audio-only post-target source коммитит доказанный actual без video frame-а.
    AuthoritativeActual { position: Duration },

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
            | Self::AuthoritativeActual { position }
            | Self::PresentedFrame { position }
            | Self::EofFallbackFrame { position } => position,
        }
    }

    /// Stable label для structured logs и focused regression tests.
    #[must_use]
    pub(crate) const fn policy_name(self) -> &'static str {
        match self {
            Self::Target { .. } => "target",
            Self::AuthoritativeActual { .. } => "authoritative-actual",
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
