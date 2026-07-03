use std::num::NonZeroUsize;
use std::sync::{Arc, Mutex, MutexGuard};

use codec_core::VideoColorMetadata;
use frame_server_core::{
    FrameServerConfig as RuntimeFrameServerConfig, LiveScrubDecodeMode,
    TimelineHoverPrepareAdmissionOutcome, TimelineHoverPrepareAdmissionRequest,
    TimelineHoverPrepareCapacityReconfigureOutcome, TimelineHoverPrepareFrameKey,
    TimelineHoverPrepareFrameLookupRequest, TimelineHoverPrepareInsertOutcome,
    TimelineHoverPrepareLookupMissReason, TimelineHoverPrepareLookupOutcome,
    TimelineHoverPrepareNoOpReason, TimelineHoverPrepareSessionEndReleaseOutcome,
    TimelineHoverPrepareSessionEndReleaseReason, TimelineHoverPrepareTimingRejection,
    TimelineHoverPrepareWorkingSet, TimelineHoverPreparedFrameEntry,
    TimelineHoverPreparedFrameTiming, TimelineHoverRecentSupersededBudget,
    TimelineHoverRecentSupersededReconfigureOutcome, ValidatedFrameServerConfig,
};
use media_core::TrackTimestamp;
use rustiplayer_config::{
    FrameServerConfig as PersistedFrameServerConfig, FrameServerLiveScrubDecodeModeConfig,
};
use tracing::warn;
use video_core::VideoStreamDecodeConfig;
use video_present_core::VideoFrameLease;

use super::prepared_seek::PreparedSeekBranchToken;

/// Уже принятый playback-ом stream decode context, который hover executor может переиспользовать.
///
/// Player-core публикует его после успешного `configure_stream` playback decoder-а,
/// чтобы app-side hover decode не дублировал capability selection/packetization logic.
#[derive(Debug, Clone, PartialEq)]
pub struct PlayerHoverStreamDecodeContext {
    /// Validated stream config, который playback decoder уже принял для активного track-а.
    pub stream_config: VideoStreamDecodeConfig,

    /// Resolved color metadata активного requirement-а для decode packets.
    pub resolved_color: Option<VideoColorMetadata>,
}

/// Итог production insert-а hover-decoded кадра через handoff boundary.
///
/// На `NoOp` lease уже dropped: release выполняет сам lease через release-on-drop,
/// caller не получает entry назад и не может сделать двойной release.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PlayerTimelineHoverPrepareInsertOutcome {
    /// Entry вставлена в primary hover storage.
    Inserted {
        /// Сколько старых primary byproducts было вытеснено при вставке.
        evicted_primary_byproducts: usize,
    },

    /// Admission отклонил вставку; lease released через drop.
    NoOp {
        /// Typed причина no-op из neutral working set-а.
        reason: TimelineHoverPrepareNoOpReason,
    },
}

/// Итог insert-а, где caller сохраняет lease при admission NoOp.
///
/// Этот API нужен incremental hover decode: decoded target-frame уже дорогой,
/// поэтому при временном pressure caller кладёт lease в pending_insert и
/// повторяет handoff без повторного decode. Старый `insert_hover_prepared_frame`
/// сохраняет release-on-NoOp семантику для мест, где retry не нужен.
pub enum PlayerTimelineHoverPrepareRetainedInsertOutcome {
    /// Entry вставлена в primary hover storage.
    Inserted {
        /// Сколько старых primary byproducts было вытеснено при вставке.
        evicted_primary_byproducts: usize,
    },

    /// Admission отклонил вставку; lease остаётся у caller-а.
    NoOp {
        /// Typed причина no-op из neutral working set-а.
        reason: TimelineHoverPrepareNoOpReason,

        /// Lease decoded frame-а, который caller может повторно вставить позже.
        lease: VideoFrameLease,
    },
}

/// Shared handle на prepared working set, который создаёт app layer и читает S17.
///
/// Внутренний branch token остаётся player-owned: app может держать handle и
/// брать preview borrow, но не может валидировать или коммитить continuation.
#[derive(Clone)]
pub struct PlayerTimelineHoverPrepareHandoff {
    inner: Arc<Mutex<TimelineHoverPrepareWorkingSet<PreparedSeekBranchToken>>>,
    stream_decode_context: Arc<Mutex<Option<PlayerHoverStreamDecodeContext>>>,
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
            stream_decode_context: Arc::new(Mutex::new(None)),
        }
    }

    /// Публикует validated playback stream decode context для app hover decode executor-а.
    ///
    /// Вызывается player-core после успешного `configure_stream` playback decoder-а;
    /// повторная публикация перезаписывает предыдущий context (media/track switch).
    pub fn publish_hover_stream_decode_context(&self, context: PlayerHoverStreamDecodeContext) {
        *self.lock_stream_decode_context() = Some(context);
    }

    /// Сбрасывает published stream context при потере валидного playback stream-а.
    pub fn clear_hover_stream_decode_context(&self) {
        *self.lock_stream_decode_context() = None;
    }

    /// Возвращает clone последнего published playback stream decode context-а.
    #[must_use]
    pub fn hover_stream_decode_context(&self) -> Option<PlayerHoverStreamDecodeContext> {
        self.lock_stream_decode_context().clone()
    }

    /// Production вставка hover-decoded кадра как frame-only hover-owned entry.
    ///
    /// Branch token не назначается: continuation/promotion semantics остаются
    /// player-owned, а frame-only entry даёт SeekLanding visual-override input.
    /// При admission no-op lease dropped внутри (release-on-drop, ровно один раз).
    #[must_use]
    pub fn insert_hover_prepared_frame(
        &self,
        admission: TimelineHoverPrepareAdmissionRequest,
        lease: VideoFrameLease,
        actual_pts: TrackTimestamp,
    ) -> PlayerTimelineHoverPrepareInsertOutcome {
        let entry = TimelineHoverPreparedFrameEntry::new(
            lease,
            TimelineHoverPreparedFrameTiming::new(actual_pts),
        );

        self.with_locked_working_set(|working_set| {
            match working_set.try_insert_prepared_frame(admission, entry) {
                TimelineHoverPrepareInsertOutcome::Inserted {
                    evicted_primary_byproducts,
                    ..
                } => PlayerTimelineHoverPrepareInsertOutcome::Inserted {
                    evicted_primary_byproducts,
                },
                TimelineHoverPrepareInsertOutcome::NoOp { entry, reason } => {
                    drop(entry);
                    PlayerTimelineHoverPrepareInsertOutcome::NoOp { reason }
                }
            }
        })
    }

    /// Production вставка с возвратом lease-а при NoOp для retry continuation.
    ///
    /// Admission проверяется под тем же lock-ом, что и insert. Если storage уже
    /// не готов принять frame, lease даже не заворачивается в working-set entry
    /// и остаётся у caller-а для exactly-once повторной вставки.
    #[must_use]
    pub fn insert_hover_prepared_frame_retaining_no_op(
        &self,
        admission: TimelineHoverPrepareAdmissionRequest,
        lease: VideoFrameLease,
        actual_pts: TrackTimestamp,
    ) -> PlayerTimelineHoverPrepareRetainedInsertOutcome {
        self.with_locked_working_set(|working_set| {
            match working_set.evaluate_prepare_admission(admission) {
                TimelineHoverPrepareAdmissionOutcome::NoOp { reason } => {
                    return PlayerTimelineHoverPrepareRetainedInsertOutcome::NoOp {
                        reason,
                        lease,
                    };
                }
                TimelineHoverPrepareAdmissionOutcome::Admitted { .. } => {}
            }

            let entry = TimelineHoverPreparedFrameEntry::new(
                lease,
                TimelineHoverPreparedFrameTiming::new(actual_pts),
            );
            match working_set.try_insert_prepared_frame(admission, entry) {
                TimelineHoverPrepareInsertOutcome::Inserted {
                    evicted_primary_byproducts,
                    ..
                } => PlayerTimelineHoverPrepareRetainedInsertOutcome::Inserted {
                    evicted_primary_byproducts,
                },
                TimelineHoverPrepareInsertOutcome::NoOp { .. } => {
                    unreachable!(
                        "working-set admission changed while PlayerTimelineHoverPrepareHandoff held the lock"
                    )
                }
            }
        })
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
        self.reconfigure_hover_prepare_primary_capacity_protecting(
            new_capacity,
            Some(protected_key),
        )
    }

    /// Меняет primary hover capacity, когда текущей protected цели может не быть.
    pub fn reconfigure_hover_prepare_primary_capacity_protecting(
        &self,
        new_capacity: NonZeroUsize,
        protected_key: Option<TimelineHoverPrepareFrameKey>,
    ) -> TimelineHoverPrepareCapacityReconfigureOutcome {
        self.with_locked_working_set(|working_set| {
            working_set.reconfigure_primary_capacity_protecting(new_capacity, protected_key)
        })
    }

    /// Меняет click-back retention budget через owner working-set boundary.
    pub fn reconfigure_recent_superseded_budget(
        &self,
        new_budget: TimelineHoverRecentSupersededBudget,
    ) -> TimelineHoverRecentSupersededReconfigureOutcome {
        self.with_locked_working_set(|working_set| {
            working_set.reconfigure_recent_superseded_budget(new_budget)
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

    fn lock_stream_decode_context(&self) -> MutexGuard<'_, Option<PlayerHoverStreamDecodeContext>> {
        match self.stream_decode_context.lock() {
            Ok(guard) => guard,
            Err(poisoned) => {
                warn!("hover stream decode context mutex was poisoned; recovering ownership");
                poisoned.into_inner()
            }
        }
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

#[cfg(test)]
mod handoff_tests {
    use std::sync::{Arc, Mutex};
    use std::time::Duration;

    use codec_core::{VideoColorMetadata, VideoDisplayOrientation};
    use frame_server_core::{
        BackendRevision, FrameExactnessPolicy, PlaybackGeneration, ScrubGeneration,
        ScrubGenerationToken, ScrubTrackSelection, SourceRevision, TimelineHoverFrameBucket,
        TimelineHoverPrepareAdmissionMode, TimelineHoverPrepareNoOpReason,
        TimelineHoverPrepareProviderBudget,
    };
    use media_core::{TimeBase, TrackId};
    use video_core::{DecodedFrame, FrameResourceHandle, VideoFrameDiagnostics};
    use video_frame_contract::VideoFrameContract;
    use video_present_core::{
        VideoFrameLease, VideoFrameLeaseConfig, VideoFrameRelease, VideoFrameReleaseOutcome,
        VideoFrameReleaseSink,
    };

    use super::*;

    #[derive(Default)]
    struct CountingReleaseSink {
        releases: Mutex<Vec<u64>>,
    }

    impl VideoFrameReleaseSink for CountingReleaseSink {
        fn release_frame(&self, release: VideoFrameRelease) -> VideoFrameReleaseOutcome {
            self.releases
                .lock()
                .expect("release sink lock")
                .push(release.resource_handle().0);
            VideoFrameReleaseOutcome::Accepted
        }
    }

    fn test_timestamp(millis: i64) -> TrackTimestamp {
        TrackTimestamp::new(
            TrackId::new(1),
            millis,
            TimeBase::new(1, 1_000).expect("valid millisecond timebase"),
        )
    }

    fn test_key(target_millis: i64) -> TimelineHoverPrepareFrameKey {
        TimelineHoverPrepareFrameKey::new(
            SourceRevision::new(1),
            ScrubTrackSelection::video_only(TrackId::new(1)),
            BackendRevision::new(2),
            ScrubGenerationToken::new(PlaybackGeneration::new(3), ScrubGeneration::new(4)),
            FrameExactnessPolicy::TargetOrAfter,
            TimelineHoverFrameBucket::new(target_millis),
        )
    }

    fn test_lease(
        pts_millis: u64,
        resource_handle: u64,
        sink: Arc<CountingReleaseSink>,
    ) -> VideoFrameLease {
        let frame = DecodedFrame {
            generation: 1,
            pts: Duration::from_millis(pts_millis),
            frame_contract: VideoFrameContract::host_yuv420_planar8(),
            width: 640,
            height: 360,
            render_width: 640,
            render_height: 360,
            display_orientation: VideoDisplayOrientation::Identity,
            color: VideoColorMetadata::sdr_bt709_limited(),
            resource_handle: FrameResourceHandle(resource_handle),
            diagnostics: VideoFrameDiagnostics::default(),
        };
        VideoFrameLease::new(VideoFrameLeaseConfig::new(1, frame, sink))
    }

    fn admission(key: TimelineHoverPrepareFrameKey) -> TimelineHoverPrepareAdmissionRequest {
        TimelineHoverPrepareAdmissionRequest::new(
            key,
            key,
            TimelineHoverPrepareAdmissionMode::NormalHover,
            TimelineHoverPrepareProviderBudget::SpareSlotAvailable,
        )
    }

    #[test]
    fn published_hover_stream_decode_context_round_trips_and_clears() {
        let handoff = PlayerTimelineHoverPrepareHandoff::default();
        assert!(handoff.hover_stream_decode_context().is_none());

        let context = PlayerHoverStreamDecodeContext {
            stream_config: video_core::VideoStreamDecodeConfig {
                track_id: TrackId::new(1),
                codec: codec_core::VideoCodec::H264,
                profile: None,
                bit_depth: None,
                chroma: None,
                coded_width: Some(640),
                coded_height: Some(360),
                display_orientation: VideoDisplayOrientation::Identity,
                frame_contract: VideoFrameContract::host_yuv420_planar8(),
                codec_private: None,
                packetization: None,
            },
            resolved_color: None,
        };
        handoff.publish_hover_stream_decode_context(context.clone());
        assert_eq!(handoff.hover_stream_decode_context(), Some(context));

        handoff.clear_hover_stream_decode_context();
        assert!(handoff.hover_stream_decode_context().is_none());
    }

    #[test]
    fn insert_hover_prepared_frame_is_borrowable_and_releases_once_on_session_end() {
        let handoff = PlayerTimelineHoverPrepareHandoff::default();
        let sink = Arc::new(CountingReleaseSink::default());
        let key = test_key(10_000);
        let actual_pts = test_timestamp(10_020);

        let outcome = handoff.insert_hover_prepared_frame(
            admission(key),
            test_lease(10_020, 42, Arc::clone(&sink)),
            actual_pts,
        );

        assert!(matches!(
            outcome,
            PlayerTimelineHoverPrepareInsertOutcome::Inserted {
                evicted_primary_byproducts: 0,
            }
        ));
        match handoff.borrow_prepared_frame(TimelineHoverPrepareFrameLookupRequest::new(
            key,
            test_timestamp(10_000),
        )) {
            PlayerTimelineHoverPrepareBorrowOutcome::Borrowed(borrowed) => {
                assert_eq!(borrowed.timing().actual_pts(), actual_pts);
            }
            _ => panic!("inserted hover frame must be borrowable"),
        }

        let release_outcome = handoff.release_hover_owned_entries_for_session_end(
            TimelineHoverPrepareSessionEndReleaseReason::SourceOrBackendSwitched,
        );
        assert_eq!(release_outcome.primary_entries_released(), 1);
        assert_eq!(*sink.releases.lock().expect("release sink lock"), vec![42]);
    }

    #[test]
    fn insert_hover_prepared_frame_admission_no_op_releases_lease_once() {
        let handoff = PlayerTimelineHoverPrepareHandoff::default();
        let sink = Arc::new(CountingReleaseSink::default());
        let key = test_key(10_000);
        let pressured_admission = TimelineHoverPrepareAdmissionRequest::new(
            key,
            key,
            TimelineHoverPrepareAdmissionMode::NormalHover,
            TimelineHoverPrepareProviderBudget::ExhaustedAfterActivePins,
        );

        let outcome = handoff.insert_hover_prepared_frame(
            pressured_admission,
            test_lease(10_020, 7, Arc::clone(&sink)),
            test_timestamp(10_020),
        );

        assert!(matches!(
            outcome,
            PlayerTimelineHoverPrepareInsertOutcome::NoOp {
                reason: TimelineHoverPrepareNoOpReason::ProviderResourcePressure,
            }
        ));
        // Lease released ровно один раз через drop, entry не осталась в storage.
        assert_eq!(*sink.releases.lock().expect("release sink lock"), vec![7]);
        assert!(matches!(
            handoff.borrow_prepared_frame(TimelineHoverPrepareFrameLookupRequest::new(
                key,
                test_timestamp(10_000),
            )),
            PlayerTimelineHoverPrepareBorrowOutcome::Miss(_)
        ));
    }

    #[test]
    fn retained_insert_admission_no_op_returns_lease_without_release() {
        let handoff = PlayerTimelineHoverPrepareHandoff::default();
        let sink = Arc::new(CountingReleaseSink::default());
        let key = test_key(10_000);
        let pressured_admission = TimelineHoverPrepareAdmissionRequest::new(
            key,
            key,
            TimelineHoverPrepareAdmissionMode::NormalHover,
            TimelineHoverPrepareProviderBudget::ExhaustedAfterActivePins,
        );

        let outcome = handoff.insert_hover_prepared_frame_retaining_no_op(
            pressured_admission,
            test_lease(10_020, 7, Arc::clone(&sink)),
            test_timestamp(10_020),
        );

        let returned_lease = match outcome {
            PlayerTimelineHoverPrepareRetainedInsertOutcome::NoOp { reason, lease } => {
                assert_eq!(
                    reason,
                    TimelineHoverPrepareNoOpReason::ProviderResourcePressure
                );
                lease
            }
            PlayerTimelineHoverPrepareRetainedInsertOutcome::Inserted { .. } => {
                panic!("pressured admission must not insert retained hover frame")
            }
        };
        assert_eq!(
            *sink.releases.lock().expect("release sink lock"),
            Vec::<u64>::new(),
            "retained NoOp must keep caller-owned lease alive"
        );

        drop(returned_lease);

        assert_eq!(*sink.releases.lock().expect("release sink lock"), vec![7]);
    }
}
