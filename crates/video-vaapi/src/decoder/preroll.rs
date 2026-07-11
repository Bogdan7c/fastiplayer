use std::time::Duration;

use video_core::{
    VideoPrerollOutputFloor, VideoPrerollOutputFloorClear, VideoPrerollOutputFloorResult,
};

/// Active accurate-seek output floor, которым владеет VAAPI decoder backend.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) struct ActivePrerollOutputFloor {
    /// Seek generation, для которой backend подавляет pre-floor output.
    pub(super) generation: u64,

    /// Минимальный PTS, который можно публиковать наружу.
    pub(super) floor_pts: Duration,

    /// Нужно ли сохранить последний кадр перед floor как EOF fallback.
    pub(super) retain_latest_before_floor: bool,

    /// Был ли уже успешно опубликован кадр с `pts >= floor_pts`.
    pub(super) target_or_after_published: bool,
}

impl ActivePrerollOutputFloor {
    /// Создаёт backend-local state из нейтрального video-core policy.
    pub(super) fn from_policy(policy: VideoPrerollOutputFloor) -> Self {
        Self {
            generation: policy.generation,
            floor_pts: policy.floor_pts,
            retain_latest_before_floor: policy.retain_latest_before_floor,
            target_or_after_published: false,
        }
    }

    /// Проверяет, совпадает ли active floor с новым запросом без сброса флагов.
    pub(super) fn matches_policy(self, policy: VideoPrerollOutputFloor) -> bool {
        self.generation == policy.generation
            && self.floor_pts == policy.floor_pts
            && self.retain_latest_before_floor == policy.retain_latest_before_floor
    }
}

/// Накопительные diagnostics counters для decoder-side preroll output floor.
#[derive(Debug, Default, Clone, Copy, PartialEq, Eq)]
pub(super) struct PrerollOutputFloorCounters {
    /// Сколько pre-floor кадров подавлено без DMA-BUF export/publish.
    pub(super) suppressed_frame_count: u64,

    /// Сколько раз более поздний pre-floor candidate заменил старый.
    pub(super) candidate_replaced_count: u64,

    /// Сколько fallback candidates было опубликовано через EOF promotion.
    pub(super) candidate_promoted_count: u64,

    /// Сколько раз backend впервые дошёл до publish кадра `pts >= floor`.
    pub(super) target_published_after_floor_count: u64,
}

/// Pure state accurate-seek output floor без concrete VA handle payload-а.
#[derive(Debug, Default, Clone, Copy, PartialEq, Eq)]
pub(super) struct PrerollOutputFloorState {
    /// Active floor отсутствует вне accurate-seek preroll window.
    pub(super) active: Option<ActivePrerollOutputFloor>,

    /// Counters остаются накопительными, чтобы debug logs были полезны вручную.
    pub(super) counters: PrerollOutputFloorCounters,
}

impl PrerollOutputFloorState {
    /// Устанавливает новый floor или возвращает `Unchanged` для того же policy.
    pub(super) fn set_floor(
        &mut self,
        policy: VideoPrerollOutputFloor,
    ) -> VideoPrerollOutputFloorResult {
        if self
            .active
            .is_some_and(|active_floor| active_floor.matches_policy(policy))
        {
            return VideoPrerollOutputFloorResult::Unchanged;
        }

        self.active = Some(ActivePrerollOutputFloor::from_policy(policy));
        VideoPrerollOutputFloorResult::Applied
    }

    /// Очищает active floor, сохраняя distinct `Unchanged` для generation mismatch.
    pub(super) fn clear_floor(
        &mut self,
        clear: VideoPrerollOutputFloorClear,
    ) -> VideoPrerollOutputFloorResult {
        let Some(active_floor) = self.active else {
            return VideoPrerollOutputFloorResult::Unchanged;
        };

        let should_clear = match clear {
            VideoPrerollOutputFloorClear::MatchingGeneration(generation) => {
                active_floor.generation == generation
            }
            VideoPrerollOutputFloorClear::Any => true,
        };

        if !should_clear {
            return VideoPrerollOutputFloorResult::Unchanged;
        }

        self.active = None;
        VideoPrerollOutputFloorResult::Cleared
    }

    /// Возвращает active floor только если кадр принадлежит той же generation и ниже floor.
    pub(super) fn suppression_floor(
        self,
        frame_pts: Duration,
        generation: u64,
    ) -> Option<ActivePrerollOutputFloor> {
        let active_floor = self.active?;

        if active_floor.generation == generation && frame_pts < active_floor.floor_pts {
            Some(active_floor)
        } else {
            None
        }
    }

    /// Проверяет, что кадр закрывает retained candidate для той же active generation.
    pub(super) fn is_target_or_after_for_active_floor(
        self,
        frame_pts: Duration,
        generation: u64,
    ) -> bool {
        let Some(active_floor) = self.active else {
            return false;
        };

        active_floor.generation == generation && frame_pts >= active_floor.floor_pts
    }

    /// Проверяет, является ли кадр первым успешно опубликованным target-or-after output.
    pub(super) fn record_target_or_after_published(
        &mut self,
        frame_pts: Duration,
        generation: u64,
    ) -> bool {
        let Some(active_floor) = self.active.as_mut() else {
            return false;
        };

        if active_floor.generation != generation
            || frame_pts < active_floor.floor_pts
            || active_floor.target_or_after_published
        {
            return false;
        }

        active_floor.target_or_after_published = true;
        self.counters.target_published_after_floor_count = self
            .counters
            .target_published_after_floor_count
            .saturating_add(1);
        true
    }

    /// Учитывает suppression без связывания pure state с VA handle ownership.
    pub(super) fn record_suppressed_frame(&mut self) {
        self.counters.suppressed_frame_count =
            self.counters.suppressed_frame_count.saturating_add(1);
    }

    /// Учитывает замену pre-floor candidate-а более поздним кадром.
    pub(super) fn record_candidate_replaced(&mut self) {
        self.counters.candidate_replaced_count =
            self.counters.candidate_replaced_count.saturating_add(1);
    }

    /// Проверяет, нужно ли EOF drain публиковать fallback candidate.
    pub(super) fn should_promote_candidate(self, generation: u64) -> bool {
        let Some(active_floor) = self.active else {
            return false;
        };

        active_floor.generation == generation && !active_floor.target_or_after_published
    }

    /// Учитывает успешную EOF promotion и закрывает повторную promotion для generation.
    pub(super) fn record_candidate_promoted(&mut self, generation: u64) {
        self.counters.candidate_promoted_count =
            self.counters.candidate_promoted_count.saturating_add(1);

        if let Some(active_floor) = self.active.as_mut()
            && active_floor.generation == generation
        {
            active_floor.target_or_after_published = true;
        }
    }
}

/// Metadata fallback candidate-а без concrete VA handle.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) struct PrerollFallbackCandidateMetadata {
    /// PTS candidate-а, по которому выбирается самый поздний pre-floor frame.
    pub(super) pts: Duration,

    /// Seek generation candidate-а; promotion разрешена только для matching generation.
    pub(super) generation: u64,
}

/// Последний pre-floor frame, который можно promoted на EOF без target-or-after output.
pub(super) struct PrerollFallbackCandidate<T> {
    /// Concrete handle остаётся backend-local payload-ом.
    pub(super) handle: T,

    /// Metadata нужна для generation checks/logs без чтения handle после move.
    pub(super) metadata: PrerollFallbackCandidateMetadata,
}

impl<T> PrerollFallbackCandidate<T> {
    /// Собирает candidate из handle и metadata одной generation.
    pub(super) fn new(handle: T, pts: Duration, generation: u64) -> Self {
        Self {
            handle,
            metadata: PrerollFallbackCandidateMetadata { pts, generation },
        }
    }
}

/// Решение по сохранению нового pre-floor candidate-а.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) enum PrerollFallbackCandidateDecision {
    /// Candidate slot пуст, incoming становится baseline fallback-ом.
    StoreFirst,

    /// Incoming позже или равен старому PTS и заменяет текущий candidate.
    ReplaceExisting,

    /// Incoming старее текущего candidate-а и сразу отбрасывается.
    DropIncoming,
}

/// Выбирает самый поздний pre-floor candidate без знания concrete handle type.
pub(super) fn preroll_fallback_candidate_decision<T>(
    current_candidate: Option<&PrerollFallbackCandidate<T>>,
    incoming_metadata: PrerollFallbackCandidateMetadata,
) -> PrerollFallbackCandidateDecision {
    let Some(current_candidate) = current_candidate else {
        return PrerollFallbackCandidateDecision::StoreFirst;
    };

    if incoming_metadata.pts >= current_candidate.metadata.pts {
        PrerollFallbackCandidateDecision::ReplaceExisting
    } else {
        PrerollFallbackCandidateDecision::DropIncoming
    }
}
