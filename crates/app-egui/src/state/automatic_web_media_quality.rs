//! Hysteresis-контроллер автоматического web-media качества.

use std::time::{Duration, Instant};

use player_core::{MediaInstanceId, PlaybackState, PlayerSnapshot};
use playlist_core::PlaylistItemId;

/// Production tuning автоматического качества без скрытых числовых литералов в state loop.
#[derive(Debug, Clone, Copy)]
struct AutomaticWebMediaQualityPolicy {
    /// Сколько непрерывного playback с запасом в очередях нужно перед upshift.
    stable_playback_before_upshift: Duration,
    /// Короткий transient buffering не должен немедленно понижать качество.
    buffering_grace: Duration,
    /// Не повторяем высоту, которая только что привела к starvation.
    failed_height_retry_after: Duration,
    /// Минимальный audio runway для безопасного пробного upshift.
    minimum_audio_runway: Duration,
    /// Video-only эквивалент runway в encoded/decoded очередях.
    minimum_video_queue_units: usize,
    /// Защита от нескольких решений между началом и publication same-item switch-а.
    decision_hold: Duration,
}

impl AutomaticWebMediaQualityPolicy {
    /// Консервативная interactive policy: fast downshift, осторожный upshift.
    const fn interactive() -> Self {
        Self {
            stable_playback_before_upshift: Duration::from_secs(30),
            buffering_grace: Duration::from_millis(750),
            failed_height_retry_after: Duration::from_secs(120),
            minimum_audio_runway: Duration::from_millis(250),
            minimum_video_queue_units: 2,
            decision_hold: Duration::from_secs(5),
        }
    }
}

/// Наблюдение одного frame tick без доступа контроллера к catalog или mutable player state.
#[derive(Debug, Clone, Copy)]
pub(super) struct AutomaticWebMediaQualityObservation {
    /// Стабильная queue identity переживает same-item media reinstall.
    item_id: PlaylistItemId,
    /// Новый installed runtime сбрасывает только transient measurement window.
    media_instance_id: MediaInstanceId,
    /// Authoritative player state; UI buffering flag здесь не угадывается.
    playback_state: PlaybackState,
    /// Монотонный backend counter позволяет заметить короткий underrun между кадрами UI.
    audio_underruns: u64,
    /// Active rendition, которую нужно заблокировать при starvation.
    active_height: u32,
    /// Наличие реальной соседней ступени вниз.
    lower_available: bool,
    /// Высота реальной соседней ступени вверх, если catalog её имеет.
    higher_height: Option<u32>,
    /// Pipeline уже накопил достаточный запас для безопасного upshift trial.
    has_playback_runway: bool,
}

impl AutomaticWebMediaQualityObservation {
    /// Строит observation из read-only player snapshot и catalog-derived соседей.
    pub(super) fn from_snapshot(
        item_id: PlaylistItemId,
        media_instance_id: MediaInstanceId,
        snapshot: &PlayerSnapshot,
        active_height: u32,
        lower_available: bool,
        higher_height: Option<u32>,
    ) -> Self {
        let policy = AutomaticWebMediaQualityPolicy::interactive();
        let has_playback_runway = match snapshot.selected_tracks.audio_track {
            Some(_) => snapshot
                .audio_buffer
                .level
                .is_some_and(|level| level >= policy.minimum_audio_runway),
            None => {
                snapshot
                    .queues
                    .pending_video_packets
                    .saturating_add(snapshot.queues.decoded_video_frames)
                    >= policy.minimum_video_queue_units
            }
        };
        Self {
            item_id,
            media_instance_id,
            playback_state: snapshot.playback_state,
            audio_underruns: snapshot.audio_buffer.underruns,
            active_height,
            lower_available,
            higher_height,
            has_playback_runway,
        }
    }
}

/// Ровно одно намерение; exact target выбирает владелец catalog-а.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) enum AutomaticWebMediaQualityDecision {
    /// Starvation evidence требует одну ступень ниже.
    Lower,
    /// Устойчивый playback допускает пробу одной ступени выше.
    Higher,
}

/// Высота, которую временно нельзя повторно пробовать.
#[derive(Debug, Clone, Copy)]
struct BlockedHeight {
    /// Exact высота неуспешной rendition.
    height: u32,
    /// После этого момента сеть разрешено проверить повторно.
    retry_at: Instant,
}

/// App-owned controller не владеет catalog, transport или player mutation.
pub(super) struct AutomaticWebMediaQualityController {
    /// Tuning policy собрана в одном месте и подменяется только focused tests.
    policy: AutomaticWebMediaQualityPolicy,
    /// Текущий queue item; смена item полностью очищает network evidence.
    item_id: Option<PlaylistItemId>,
    /// Текущий installed runtime; смена generation сбрасывает measurement window.
    media_instance_id: Option<MediaInstanceId>,
    /// Начало непрерывного Playing с достаточным runway.
    stable_playback_since: Option<Instant>,
    /// Начало непрерывного Buffering.
    buffering_since: Option<Instant>,
    /// Последнее наблюдавшееся значение monotonic underrun counter.
    last_audio_underruns: u64,
    /// Временный запрет повторного trial заведомо тяжёлой ступени.
    blocked_height: Option<BlockedHeight>,
    /// Защита от дублирующих решений, пока same-item switch ещё не стартовал.
    hold_until: Option<Instant>,
}

impl Default for AutomaticWebMediaQualityController {
    fn default() -> Self {
        Self::new(AutomaticWebMediaQualityPolicy::interactive())
    }
}

impl AutomaticWebMediaQualityController {
    /// Создаёт пустой controller для новой app session.
    const fn new(policy: AutomaticWebMediaQualityPolicy) -> Self {
        Self {
            policy,
            item_id: None,
            media_instance_id: None,
            stable_playback_since: None,
            buffering_since: None,
            last_audio_underruns: 0,
            blocked_height: None,
            hold_until: None,
        }
    }

    /// Полностью забывает automatic evidence при manual preference или смене source scope.
    pub(super) fn reset(&mut self) {
        *self = Self::default();
    }

    /// Обновляет hysteresis и, максимум, выдаёт одно quality-намерение.
    pub(super) fn observe(
        &mut self,
        observation: AutomaticWebMediaQualityObservation,
        now: Instant,
    ) -> Option<AutomaticWebMediaQualityDecision> {
        self.bind_observation_lineage(observation, now);
        self.expire_blocked_height(now);

        let underrun_observed = observation.audio_underruns > self.last_audio_underruns;
        self.last_audio_underruns = observation.audio_underruns;
        if underrun_observed
            && observation.lower_available
            && self.decision_allowed(now)
            && matches!(
                observation.playback_state,
                PlaybackState::Playing | PlaybackState::Buffering
            )
        {
            return Some(self.commit_downshift(observation.active_height, now));
        }

        match observation.playback_state {
            PlaybackState::Buffering => self.observe_buffering(observation, now),
            PlaybackState::Playing => self.observe_playing(observation, now),
            PlaybackState::Idle
            | PlaybackState::Opening
            | PlaybackState::Paused
            | PlaybackState::Seeking
            | PlaybackState::Scrubbing
            | PlaybackState::Draining
            | PlaybackState::Ended
            | PlaybackState::Stopped
            | PlaybackState::Failed => {
                self.clear_measurement_windows();
                None
            }
        }
    }

    /// Привязывает measurement к item/runtime, сохраняя failed-height evidence across reinstall.
    fn bind_observation_lineage(
        &mut self,
        observation: AutomaticWebMediaQualityObservation,
        now: Instant,
    ) {
        if self.item_id != Some(observation.item_id) {
            let policy = self.policy;
            *self = Self::new(policy);
            self.item_id = Some(observation.item_id);
        }
        if self.media_instance_id != Some(observation.media_instance_id) {
            let replaces_existing_runtime = self.media_instance_id.is_some();
            self.media_instance_id = Some(observation.media_instance_id);
            self.clear_measurement_windows();
            self.last_audio_underruns = observation.audio_underruns;
            self.hold_until = replaces_existing_runtime.then(|| now + self.policy.decision_hold);
        }
    }

    /// Buffering становится starvation evidence только после grace interval.
    fn observe_buffering(
        &mut self,
        observation: AutomaticWebMediaQualityObservation,
        now: Instant,
    ) -> Option<AutomaticWebMediaQualityDecision> {
        self.stable_playback_since = None;
        let buffering_since = *self.buffering_since.get_or_insert(now);
        if observation.lower_available
            && now.saturating_duration_since(buffering_since) >= self.policy.buffering_grace
            && self.decision_allowed(now)
        {
            return Some(self.commit_downshift(observation.active_height, now));
        }
        None
    }

    /// Playing разрешает только осторожный adjacent upshift после полного stable window.
    fn observe_playing(
        &mut self,
        observation: AutomaticWebMediaQualityObservation,
        now: Instant,
    ) -> Option<AutomaticWebMediaQualityDecision> {
        self.buffering_since = None;
        if !observation.has_playback_runway {
            self.stable_playback_since = None;
            return None;
        }
        let stable_since = *self.stable_playback_since.get_or_insert(now);
        if now.saturating_duration_since(stable_since) < self.policy.stable_playback_before_upshift
            || !self.decision_allowed(now)
        {
            return None;
        }
        let higher_height = observation.higher_height?;
        if self
            .blocked_height
            .is_some_and(|blocked| blocked.height == higher_height && now < blocked.retry_at)
        {
            return None;
        }
        self.stable_playback_since = None;
        self.hold_until = Some(now + self.policy.decision_hold);
        Some(AutomaticWebMediaQualityDecision::Higher)
    }

    /// Фиксирует неуспешную active высоту и выдаёт одну lower-команду.
    fn commit_downshift(
        &mut self,
        active_height: u32,
        now: Instant,
    ) -> AutomaticWebMediaQualityDecision {
        self.blocked_height = Some(BlockedHeight {
            height: active_height,
            retry_at: now + self.policy.failed_height_retry_after,
        });
        self.clear_measurement_windows();
        self.hold_until = Some(now + self.policy.decision_hold);
        AutomaticWebMediaQualityDecision::Lower
    }

    /// Удаляет истёкший запрет, чтобы долгоживущий playback мог проверить улучшившуюся сеть.
    fn expire_blocked_height(&mut self, now: Instant) {
        if self
            .blocked_height
            .is_some_and(|blocked| now >= blocked.retry_at)
        {
            self.blocked_height = None;
        }
    }

    /// Проверяет cooldown без изменения состояния.
    fn decision_allowed(&self, now: Instant) -> bool {
        self.hold_until.is_none_or(|hold_until| now >= hold_until)
    }

    /// Сбрасывает только transient состояния текущего runtime-а.
    fn clear_measurement_windows(&mut self) {
        self.stable_playback_since = None;
        self.buffering_since = None;
    }
}

#[cfg(test)]
mod tests {
    use std::num::NonZeroU64;

    use super::*;

    /// Короткая policy делает временные переходы точными без sleep/flaky wall clock.
    const fn test_policy() -> AutomaticWebMediaQualityPolicy {
        AutomaticWebMediaQualityPolicy {
            stable_playback_before_upshift: Duration::from_secs(3),
            buffering_grace: Duration::from_secs(1),
            failed_height_retry_after: Duration::from_secs(10),
            minimum_audio_runway: Duration::ZERO,
            minimum_video_queue_units: 0,
            decision_hold: Duration::ZERO,
        }
    }

    /// Создаёт observation без mutable player fixture.
    fn observation(
        media_instance_value: u64,
        playback_state: PlaybackState,
        active_height: u32,
        lower_available: bool,
        higher_height: Option<u32>,
    ) -> AutomaticWebMediaQualityObservation {
        AutomaticWebMediaQualityObservation {
            item_id: PlaylistItemId::from_persistence_value(1).expect("playlist item"),
            media_instance_id: MediaInstanceId::from_non_zero(
                NonZeroU64::new(media_instance_value).expect("media instance"),
            ),
            playback_state,
            audio_underruns: 0,
            active_height,
            lower_available,
            higher_height,
            has_playback_runway: true,
        }
    }

    #[test]
    fn stable_playback_upshifts_only_after_complete_window() {
        let mut controller = AutomaticWebMediaQualityController::new(test_policy());
        let start = Instant::now();
        let playing = observation(1, PlaybackState::Playing, 720, true, Some(1080));

        assert_eq!(controller.observe(playing, start), None);
        assert_eq!(
            controller.observe(playing, start + Duration::from_millis(2_999)),
            None
        );
        assert_eq!(
            controller.observe(playing, start + Duration::from_secs(3)),
            Some(AutomaticWebMediaQualityDecision::Higher)
        );
    }

    #[test]
    fn buffering_downshift_blocks_failed_height_across_same_item_reinstall() {
        let mut controller = AutomaticWebMediaQualityController::new(test_policy());
        let start = Instant::now();
        let buffering = observation(1, PlaybackState::Buffering, 1080, true, None);

        assert_eq!(controller.observe(buffering, start), None);
        assert_eq!(
            controller.observe(buffering, start + Duration::from_secs(1)),
            Some(AutomaticWebMediaQualityDecision::Lower)
        );

        let lower_runtime = observation(2, PlaybackState::Playing, 720, true, Some(1080));
        assert_eq!(
            controller.observe(lower_runtime, start + Duration::from_secs(2)),
            None
        );
        assert_eq!(
            controller.observe(lower_runtime, start + Duration::from_secs(5)),
            None,
            "failed 1080p нельзя повторно пробовать до retry deadline"
        );
        assert_eq!(
            controller.observe(lower_runtime, start + Duration::from_secs(12)),
            Some(AutomaticWebMediaQualityDecision::Higher),
            "после retry deadline улучшившуюся сеть можно проверить снова"
        );
    }

    #[test]
    fn audio_underrun_downshifts_without_waiting_for_visible_buffering_state() {
        let mut controller = AutomaticWebMediaQualityController::new(test_policy());
        let start = Instant::now();
        let mut playing = observation(1, PlaybackState::Playing, 720, true, Some(1080));
        assert_eq!(controller.observe(playing, start), None);

        playing.audio_underruns = 1;
        assert_eq!(
            controller.observe(playing, start + Duration::from_millis(1)),
            Some(AutomaticWebMediaQualityDecision::Lower)
        );
    }

    #[test]
    fn initial_runtime_uses_buffering_grace_while_reinstalled_runtime_keeps_hold() {
        let mut policy = test_policy();
        policy.decision_hold = Duration::from_secs(5);
        let mut controller = AutomaticWebMediaQualityController::new(policy);
        let start = Instant::now();
        let buffering = observation(1, PlaybackState::Buffering, 1080, true, None);

        assert_eq!(controller.observe(buffering, start), None);
        assert_eq!(
            controller.observe(buffering, start + Duration::from_secs(1)),
            Some(AutomaticWebMediaQualityDecision::Lower),
            "первый runtime реагирует по buffering grace без искусственного startup hold"
        );

        let mut reinstalled = observation(2, PlaybackState::Playing, 720, true, None);
        assert_eq!(
            controller.observe(reinstalled, start + Duration::from_millis(1_100)),
            None
        );
        reinstalled.audio_underruns = 1;
        assert_eq!(
            controller.observe(reinstalled, start + Duration::from_secs(2)),
            None,
            "reinstalled runtime обязан удерживать ступень внутри anti-flap окна"
        );
    }
}
