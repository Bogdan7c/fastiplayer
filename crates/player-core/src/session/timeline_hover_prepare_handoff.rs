use std::num::NonZeroUsize;
use std::sync::{Arc, Mutex, MutexGuard};

#[cfg(test)]
use frame_server_core::TimelineHoverPreparedFrameEntry;
use frame_server_core::{
    FrameServerConfig as RuntimeFrameServerConfig, LiveScrubDecodeMode,
    TimelineHoverPrepareCapacityReconfigureOutcome, TimelineHoverPrepareFrameKey,
    TimelineHoverPrepareFrameLookupRequest, TimelineHoverPrepareLookupMissReason,
    TimelineHoverPrepareLookupOutcome, TimelineHoverPrepareSessionEndReleaseOutcome,
    TimelineHoverPrepareSessionEndReleaseReason, TimelineHoverPrepareTimingRejection,
    TimelineHoverPrepareWorkingSet, TimelineHoverPreparedFrameTiming,
    TimelineHoverRecentSupersededBudget, ValidatedFrameServerConfig,
};
#[cfg(test)]
use media_core::TrackTimestamp;
use rustiplayer_config::{
    FrameServerConfig as PersistedFrameServerConfig, FrameServerLiveScrubDecodeModeConfig,
};
use tracing::warn;
use video_present_core::VideoFrameLease;

use super::prepared_seek::PreparedSeekBranchToken;

/// Shared handle на prepared working set, который создаёт app layer и читает S17.
///
/// Внутренний branch token остаётся player-owned: app может держать handle и
/// брать preview borrow, но не может валидировать или коммитить continuation.
#[derive(Clone)]
pub struct PlayerTimelineHoverPrepareHandoff {
    inner: Arc<Mutex<TimelineHoverPrepareWorkingSet<PreparedSeekBranchToken>>>,
}

/// Borrow для будущего `HoverPreview`: lease клонируется, entry остаётся в hover storage.
pub struct PlayerTimelineHoverPreparedFrameBorrow {
    lease: VideoFrameLease,
    timing: TimelineHoverPreparedFrameTiming,
}

/// Типизированный результат preview lookup-а без удаления entry из working set.
pub enum PlayerTimelineHoverPrepareBorrowOutcome {
    Borrowed(PlayerTimelineHoverPreparedFrameBorrow),
    Miss(TimelineHoverPrepareLookupMissReason),
    TimingRejected(TimelineHoverPrepareTimingRejection),
}

impl PlayerTimelineHoverPrepareHandoff {
    /// Создаёт handoff из persisted app config, сохраняя split config crates.
    #[must_use]
    pub fn from_app_config(config: &rustiplayer_config::AppConfig) -> Self {
        let runtime_config = runtime_frame_server_config_from_persisted(&config.frame_server);
        let validated_config = runtime_config
            .validate()
            .expect("validated app frame_server config must map to frame-server-core config");
        Self::from_validated_frame_server_config(validated_config)
    }

    /// Создаёт handoff из уже validated neutral frame-server config.
    #[must_use]
    pub fn from_validated_frame_server_config(config: ValidatedFrameServerConfig) -> Self {
        let primary_capacity = NonZeroUsize::new(config.hover_prepare_window_slots() as usize)
            .expect("validated hover prepare slots must be non-zero");
        let recent_budget = TimelineHoverRecentSupersededBudget::from_validated_config(config);
        let working_set = TimelineHoverPrepareWorkingSet::with_capacity_and_recent_superseded(
            primary_capacity,
            recent_budget,
        );

        Self {
            inner: Arc::new(Mutex::new(working_set)),
        }
    }

    /// Возвращает clone lease-а для preview/materialization без promotion ownership transfer.
    #[must_use]
    pub fn borrow_prepared_frame(
        &self,
        request: TimelineHoverPrepareFrameLookupRequest,
    ) -> PlayerTimelineHoverPrepareBorrowOutcome {
        self.with_locked_working_set(|working_set| {
            match working_set.lookup_prepared_frame(request) {
                TimelineHoverPrepareLookupOutcome::Hit(prepared_frame) => {
                    PlayerTimelineHoverPrepareBorrowOutcome::Borrowed(
                        PlayerTimelineHoverPreparedFrameBorrow {
                            lease: prepared_frame.lease().clone(),
                            timing: prepared_frame.timing(),
                        },
                    )
                }
                TimelineHoverPrepareLookupOutcome::Miss(reason) => {
                    PlayerTimelineHoverPrepareBorrowOutcome::Miss(reason)
                }
                TimelineHoverPrepareLookupOutcome::TimingRejected(rejection) => {
                    PlayerTimelineHoverPrepareBorrowOutcome::TimingRejected(rejection)
                }
            }
        })
    }

    /// Освобождает hover-owned prepared entries при завершении hover session.
    ///
    /// App layer передаёт только причину session-end cleanup-а; storage,
    /// индексы и release-on-drop semantics остаются у neutral working set-а.
    #[must_use]
    pub fn release_hover_owned_entries_for_session_end(
        &self,
        reason: TimelineHoverPrepareSessionEndReleaseReason,
    ) -> TimelineHoverPrepareSessionEndReleaseOutcome {
        self.with_locked_working_set(|working_set| {
            working_set.release_hover_owned_entries_for_session_end(reason)
        })
    }

    /// Меняет primary hover capacity через owner working-set boundary.
    ///
    /// Caller передаёт уже validated capacity; full runtime Settings routing
    /// остаётся задачей S30C и не выполняется на этом уровне.
    pub fn reconfigure_hover_prepare_primary_capacity(
        &self,
        new_capacity: NonZeroUsize,
        protected_key: TimelineHoverPrepareFrameKey,
    ) -> TimelineHoverPrepareCapacityReconfigureOutcome {
        self.with_locked_working_set(|working_set| {
            working_set.reconfigure_primary_capacity(new_capacity, protected_key)
        })
    }

    pub(super) fn with_locked_working_set<ReturnValue>(
        &self,
        operation: impl FnOnce(
            &mut TimelineHoverPrepareWorkingSet<PreparedSeekBranchToken>,
        ) -> ReturnValue,
    ) -> ReturnValue {
        let mut working_set = self.lock_working_set();
        operation(&mut working_set)
    }

    #[cfg(test)]
    pub(crate) fn insert_prepared_frame_for_tests(
        &self,
        key: TimelineHoverPrepareFrameKey,
        lease: VideoFrameLease,
        actual_pts: TrackTimestamp,
        branch_token: Option<PreparedSeekBranchToken>,
    ) {
        let entry = TimelineHoverPreparedFrameEntry::new(
            lease,
            TimelineHoverPreparedFrameTiming::new(actual_pts),
        );
        let entry = match branch_token {
            Some(token) => entry.with_branch_token(token),
            None => entry,
        };

        self.with_locked_working_set(|working_set| working_set.insert_prepared_frame(key, entry));
    }

    fn lock_working_set(
        &self,
    ) -> MutexGuard<'_, TimelineHoverPrepareWorkingSet<PreparedSeekBranchToken>> {
        match self.inner.lock() {
            Ok(guard) => guard,
            Err(poisoned) => {
                warn!(
                    "timeline hover prepare working set mutex was poisoned; recovering ownership"
                );
                poisoned.into_inner()
            }
        }
    }
}

impl Default for PlayerTimelineHoverPrepareHandoff {
    fn default() -> Self {
        let config = RuntimeFrameServerConfig::default()
            .validate()
            .expect("default frame-server config must validate");
        Self::from_validated_frame_server_config(config)
    }
}

impl PlayerTimelineHoverPreparedFrameBorrow {
    /// Возвращает cloned lease для preview surface; caller не получает ownership entry.
    #[must_use]
    pub fn lease(&self) -> &VideoFrameLease {
        &self.lease
    }

    /// Возвращает actual timing, который уже прошёл working-set validation.
    #[must_use]
    pub const fn timing(&self) -> TimelineHoverPreparedFrameTiming {
        self.timing
    }
}

fn runtime_frame_server_config_from_persisted(
    config: &PersistedFrameServerConfig,
) -> RuntimeFrameServerConfig {
    RuntimeFrameServerConfig {
        live_scrub_max_hz: config.live_scrub_max_hz,
        live_scrub_decode_mode: runtime_live_scrub_decode_mode(config.live_scrub_decode_mode),
        hover_prepare_window_slots: config.hover_prepare_window_slots,
        software_hover_prepare_window_slots: config.software_hover_prepare_window_slots,
        recent_superseded_prepare_slots: config.recent_superseded_prepare_slots,
        software_recent_superseded_prepare_slots: config.software_recent_superseded_prepare_slots,
        ..RuntimeFrameServerConfig::default()
    }
}

fn runtime_live_scrub_decode_mode(
    mode: FrameServerLiveScrubDecodeModeConfig,
) -> LiveScrubDecodeMode {
    match mode {
        FrameServerLiveScrubDecodeModeConfig::ThrottledLatest => {
            LiveScrubDecodeMode::ThrottledLatest
        }
        FrameServerLiveScrubDecodeModeConfig::EveryDragEvent => LiveScrubDecodeMode::EveryDragEvent,
    }
}
