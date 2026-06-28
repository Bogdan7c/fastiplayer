use std::path::Path;

use frame_server_core::{
    AudioResumeBudgetSource, CancelScrubReason, DecoderBackpressureReason, DemuxUnavailableReason,
    DemuxUnsupportedReason, FeedAndDrainStopCondition, FinishScrubPolicy, FrameServerConfig,
    HostUploadBackpressureReason, ResourceBusyReason, ScrubDriverOutcome, ScrubExactnessPolicy,
    ScrubGeneration, ScrubIntentKind, ScrubRequestKind, ScrubStaleReason, ScrubTarget,
    ScrubTargetUpdate, ScrubTimeoutReason, ScrubTrackSelection,
};

use super::super::scrub_driver::{
    AudioResumeTimingInput, PlayerAudioResumeBudget, PlayerScrubTransactionDriver,
    ScrubDecodePointBefore, ScrubDemuxSeekAccepted, ScrubFeedDrainResult, ScrubFinishResult,
    ScrubLifecycleError, ScrubLifecycleResult, ScrubTransactionLifecycle,
    default_scrub_execution_policy, derive_audio_resume_timeout_budget,
    scrub_update_guards_for_owner,
};
use super::test_support::*;
use super::*;

#[test]
fn scrub_driver_owns_prepare_seek_feed_lifecycle_order() {
    let config = FrameServerConfig::default()
        .validate()
        .expect("default frame-server config must validate");
    let mut driver = PlayerScrubTransactionDriver::new(config, ScrubGeneration::new(0));
    let mut lifecycle = RecordingScrubLifecycle::default();

    let run = driver.submit_target_update(&mut lifecycle, live_scrub_update(config));

    assert_eq!(
        lifecycle.steps,
        vec![
            LifecycleStep::ClearOldFloor,
            LifecycleStep::FlushDecoder,
            LifecycleStep::BeginNestedScrubGeneration,
            LifecycleStep::ClearPendingQueues,
            LifecycleStep::ComputeDecodePointBefore,
            LifecycleStep::DemuxSeekToDecodePoint,
            LifecycleStep::FeedAndDrain,
        ]
    );
    assert_eq!(
        run.intents,
        vec![
            ScrubIntentKind::PrepareTarget,
            ScrubIntentKind::SeekDecodePointBefore,
            ScrubIntentKind::FeedAndDrain,
        ]
    );
    assert!(matches!(
        run.outcomes.as_slice(),
        [
            ScrubDriverOutcome::Prepared(_),
            ScrubDriverOutcome::DecodePointSeeked(_),
            ScrubDriverOutcome::AudioResumePending(_),
        ]
    ));
}

#[test]
fn scrub_driver_reuses_existing_decoder_boundary_without_second_session() {
    let config = FrameServerConfig::default()
        .validate()
        .expect("default frame-server config must validate");
    let mut driver = PlayerScrubTransactionDriver::new(config, ScrubGeneration::new(0));
    let mut lifecycle = RecordingScrubLifecycle::default();

    let _run = driver.submit_target_update(&mut lifecycle, live_scrub_update(config));

    assert_eq!(lifecycle.existing_decoder_flush_count, 1);
    assert_eq!(lifecycle.created_decoder_count, 0);
    assert_eq!(lifecycle.created_session_count, 0);
}

#[test]
fn player_session_scrub_driver_adapter_uses_existing_pipeline_boundaries() {
    let config = FrameServerConfig::default()
        .validate()
        .expect("default frame-server config must validate");
    let mut session = PlayerSession::new();
    let seek_request_log = install_fake_media_with_seek_request_log(
        &mut session,
        vec![fake_track(7, TrackKind::Video)],
    );
    let decoder = SharedFakeVideoDecoderThread::new();
    session.pipeline.set_video_decoder_thread(decoder.clone());
    let initial_generation = session.pipeline.seek_generation();
    let mut driver = PlayerScrubTransactionDriver::new(config, ScrubGeneration::new(0));

    let run = driver.submit_target_update(&mut session, live_scrub_update(config));

    assert_eq!(decoder.flush_count(), 1);
    assert_eq!(session.pipeline.seek_generation(), initial_generation + 1);
    let requests = seek_request_log.lock().expect("seek request log lock");
    assert_eq!(requests.len(), 1);
    assert_eq!(requests[0].mode, DemuxSeekMode::DecodePointBefore);
    assert_eq!(requests[0].timestamp, Duration::from_millis(1_500));
    assert!(matches!(
        run.outcomes.as_slice(),
        [
            ScrubDriverOutcome::Prepared(_),
            ScrubDriverOutcome::DecodePointSeeked(_),
            ScrubDriverOutcome::AudioResumePending(_),
        ]
    ));
}

#[test]
fn scrub_lifecycle_errors_map_to_typed_driver_outcomes() {
    let context = scrub_context();

    let cases = [
        (
            ScrubLifecycleError::DemuxUnavailable(DemuxUnavailableReason::DemuxerClosed),
            ScrubDriverOutcomeKindForTest::DemuxUnavailable,
        ),
        (
            ScrubLifecycleError::DemuxUnsupported(
                DemuxUnsupportedReason::DecodePointBeforeUnsupported,
            ),
            ScrubDriverOutcomeKindForTest::DemuxUnsupported,
        ),
        (
            ScrubLifecycleError::DecoderBackpressure(
                DecoderBackpressureReason::DecoderControlChannelFull,
            ),
            ScrubDriverOutcomeKindForTest::DecoderBackpressure,
        ),
        (
            ScrubLifecycleError::HostUploadBackpressure(
                HostUploadBackpressureReason::UploadSlotsExhausted,
            ),
            ScrubDriverOutcomeKindForTest::HostUploadBackpressure,
        ),
        (
            ScrubLifecycleError::ResourceBusy(ResourceBusyReason::PlaybackOwnsDecoder),
            ScrubDriverOutcomeKindForTest::ResourceBusy,
        ),
        (
            ScrubLifecycleError::StaleGeneration(ScrubStaleReason::ScrubGenerationMismatch {
                context_generation: ScrubGeneration::new(1),
                current_generation: ScrubGeneration::new(2),
            }),
            ScrubDriverOutcomeKindForTest::StaleGeneration,
        ),
        (
            ScrubLifecycleError::Cancelled(CancelScrubReason::UserCancelled),
            ScrubDriverOutcomeKindForTest::Cancelled,
        ),
        (
            ScrubLifecycleError::TimedOut {
                reason: ScrubTimeoutReason::DriverStepBudgetExceeded,
                elapsed: Duration::from_millis(42),
            },
            ScrubDriverOutcomeKindForTest::TimedOut,
        ),
        (
            ScrubLifecycleError::Fatal(
                frame_server_core::ScrubFatalReason::BackendContractViolated,
            ),
            ScrubDriverOutcomeKindForTest::Fatal,
        ),
    ];

    for (error, expected_kind) in cases {
        assert_eq!(
            ScrubDriverOutcomeKindForTest::from(error.into_outcome(context)),
            expected_kind
        );
    }
}

#[test]
fn audio_resume_budget_uses_player_core_formula_and_records_inputs() {
    let budget = derive_audio_resume_timeout_budget(
        AudioResumeTimingInput::known(Duration::from_millis(120), Duration::from_millis(16)),
        Duration::from_millis(7),
    );

    assert_eq!(
        budget,
        PlayerAudioResumeBudget {
            metadata: frame_server_core::AudioResumeBudgetMetadata::supplied_by_driver(
                Duration::from_millis(161),
                Duration::from_millis(7)
            ),
            formula_inputs: super::super::scrub_driver::AudioResumeTimeoutFormulaInputs {
                current_output_buffer: Some(Duration::from_millis(120)),
                callback_or_device_period: Some(Duration::from_millis(16)),
                safety_margin: Duration::from_millis(25),
                max_budget: Duration::from_millis(500),
            },
        }
    );

    let capped_budget = derive_audio_resume_timeout_budget(
        AudioResumeTimingInput::known(Duration::from_millis(490), Duration::from_millis(20)),
        Duration::from_millis(3),
    );
    assert_eq!(capped_budget.metadata.budget, Duration::from_millis(500));
    assert_eq!(
        capped_budget.metadata.source,
        AudioResumeBudgetSource::SuppliedByExternalDriver
    );
}

#[test]
fn audio_resume_budget_falls_back_when_timing_is_unknown_or_invalid() {
    let unknown_budget = derive_audio_resume_timeout_budget(
        AudioResumeTimingInput::unknown(),
        Duration::from_millis(9),
    );
    assert_eq!(unknown_budget.metadata.budget, Duration::from_millis(500));
    assert_eq!(
        unknown_budget.metadata.source,
        AudioResumeBudgetSource::TimingUnknownFallback
    );

    let invalid_budget = derive_audio_resume_timeout_budget(
        AudioResumeTimingInput::known(Duration::from_millis(10), Duration::ZERO),
        Duration::from_millis(11),
    );
    assert_eq!(invalid_budget.metadata.budget, Duration::from_millis(500));
    assert_eq!(
        invalid_budget.metadata.source,
        AudioResumeBudgetSource::TimingUnknownFallback
    );
}

#[test]
fn frame_server_core_does_not_depend_on_player_core() {
    let player_core_manifest_dir = Path::new(env!("CARGO_MANIFEST_DIR"));
    let frame_server_manifest = player_core_manifest_dir
        .parent()
        .expect("player-core lives under crates/")
        .join("frame-server-core")
        .join("Cargo.toml");
    let manifest_text =
        std::fs::read_to_string(frame_server_manifest).expect("read frame-server-core manifest");

    assert!(
        !manifest_text.contains("player-core"),
        "frame-server-core must stay neutral and must not depend on player-core"
    );
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum LifecycleStep {
    ClearOldFloor,
    FlushDecoder,
    BeginNestedScrubGeneration,
    ClearPendingQueues,
    ComputeDecodePointBefore,
    DemuxSeekToDecodePoint,
    FeedAndDrain,
    Finish,
    Cancel,
}

#[derive(Debug)]
struct RecordingScrubLifecycle {
    steps: Vec<LifecycleStep>,
    existing_decoder_flush_count: usize,
    created_decoder_count: usize,
    created_session_count: usize,
    feed_and_drain_result: ScrubFeedDrainResult,
}

impl Default for RecordingScrubLifecycle {
    fn default() -> Self {
        Self {
            steps: Vec::new(),
            existing_decoder_flush_count: 0,
            created_decoder_count: 0,
            created_session_count: 0,
            feed_and_drain_result: ScrubFeedDrainResult::AudioResumePending(
                frame_server_core::AudioResumeBudgetMetadata::timing_unknown_fallback(
                    Duration::from_millis(500),
                    Duration::ZERO,
                ),
            ),
        }
    }
}

impl ScrubTransactionLifecycle for RecordingScrubLifecycle {
    fn clear_old_decode_floor(
        &mut self,
        _context: frame_server_core::ScrubTargetContext,
    ) -> ScrubLifecycleResult<()> {
        self.steps.push(LifecycleStep::ClearOldFloor);
        Ok(())
    }

    fn flush_decoder(
        &mut self,
        _context: frame_server_core::ScrubTargetContext,
    ) -> ScrubLifecycleResult<()> {
        self.steps.push(LifecycleStep::FlushDecoder);
        self.existing_decoder_flush_count += 1;
        Ok(())
    }

    fn begin_nested_scrub_generation(
        &mut self,
        _generation: frame_server_core::ScrubGenerationToken,
    ) -> ScrubLifecycleResult<()> {
        self.steps.push(LifecycleStep::BeginNestedScrubGeneration);
        Ok(())
    }

    fn clear_pending_queues(
        &mut self,
        _context: frame_server_core::ScrubTargetContext,
    ) -> ScrubLifecycleResult<()> {
        self.steps.push(LifecycleStep::ClearPendingQueues);
        Ok(())
    }

    fn compute_decode_point_before(
        &mut self,
        context: frame_server_core::ScrubTargetContext,
    ) -> ScrubLifecycleResult<ScrubDecodePointBefore> {
        self.steps.push(LifecycleStep::ComputeDecodePointBefore);
        Ok(ScrubDecodePointBefore {
            request: media_core::DemuxSeekRequest::decode_point_before(
                context.target().media_time.as_duration(),
            ),
        })
    }

    fn seek_demux_to_decode_point(
        &mut self,
        context: frame_server_core::ScrubTargetContext,
        _decode_point: ScrubDecodePointBefore,
    ) -> ScrubLifecycleResult<ScrubDemuxSeekAccepted> {
        self.steps.push(LifecycleStep::DemuxSeekToDecodePoint);
        Ok(ScrubDemuxSeekAccepted {
            actual_decode_time: context.target().media_time,
            actual_decode_pts: context.target().target_pts,
        })
    }

    fn feed_and_drain(
        &mut self,
        _context: frame_server_core::ScrubTargetContext,
        _stop_condition: FeedAndDrainStopCondition,
    ) -> ScrubLifecycleResult<ScrubFeedDrainResult> {
        self.steps.push(LifecycleStep::FeedAndDrain);
        Ok(self.feed_and_drain_result)
    }

    fn finish_scrub(
        &mut self,
        context: frame_server_core::ScrubTargetContext,
        _policy: FinishScrubPolicy,
    ) -> ScrubLifecycleResult<ScrubFinishResult> {
        self.steps.push(LifecycleStep::Finish);
        Ok(ScrubFinishResult::Committed {
            committed_time: context.target().media_time,
        })
    }

    fn cancel_scrub(
        &mut self,
        _context: frame_server_core::ScrubTargetContext,
        _reason: CancelScrubReason,
    ) -> ScrubLifecycleResult<()> {
        self.steps.push(LifecycleStep::Cancel);
        Ok(())
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum ScrubDriverOutcomeKindForTest {
    Cancelled,
    StaleGeneration,
    ResourceBusy,
    DemuxUnavailable,
    DemuxUnsupported,
    DecoderBackpressure,
    HostUploadBackpressure,
    TimedOut,
    Fatal,
}

impl From<ScrubDriverOutcome> for ScrubDriverOutcomeKindForTest {
    fn from(outcome: ScrubDriverOutcome) -> Self {
        match outcome {
            ScrubDriverOutcome::Cancelled(_) => Self::Cancelled,
            ScrubDriverOutcome::StaleGeneration(_) => Self::StaleGeneration,
            ScrubDriverOutcome::ResourceBusy(_) => Self::ResourceBusy,
            ScrubDriverOutcome::DemuxUnavailable(_) => Self::DemuxUnavailable,
            ScrubDriverOutcome::DemuxUnsupported(_) => Self::DemuxUnsupported,
            ScrubDriverOutcome::DecoderBackpressure(_) => Self::DecoderBackpressure,
            ScrubDriverOutcome::HostUploadBackpressure(_) => Self::HostUploadBackpressure,
            ScrubDriverOutcome::TimedOut(_) => Self::TimedOut,
            ScrubDriverOutcome::Fatal(_) => Self::Fatal,
            _ => panic!("unexpected outcome kind for mapping test: {outcome:?}"),
        }
    }
}

fn live_scrub_update(config: frame_server_core::ValidatedFrameServerConfig) -> ScrubTargetUpdate {
    ScrubTargetUpdate::new(
        scrub_update_guards_for_owner(1, 2, 3),
        ScrubTrackSelection::video_only(TrackId::new(7)),
        scrub_target(),
        ScrubExactnessPolicy::ExactFrame,
        ScrubRequestKind::LiveScrub,
        default_scrub_execution_policy(config, FinishScrubPolicy::CommitVisiblePreview),
    )
}

fn scrub_context() -> frame_server_core::ScrubTargetContext {
    frame_server_core::ScrubTargetContext::new(
        frame_server_core::SourceRevision::new(1),
        frame_server_core::BackendRevision::new(2),
        ScrubTrackSelection::video_only(TrackId::new(7)),
        scrub_target(),
        ScrubExactnessPolicy::ExactFrame,
        ScrubRequestKind::LiveScrub,
        frame_server_core::ScrubGenerationToken::new(
            frame_server_core::PlaybackGeneration::new(3),
            ScrubGeneration::new(4),
        ),
    )
}

fn scrub_target() -> ScrubTarget {
    ScrubTarget::new(
        MediaTime::from_millis(1_500),
        media_core::TrackTimestamp::from_unsigned_units(
            TrackId::new(7),
            1_500,
            media_core::TimeBase::new(1, 1_000).expect("valid timebase"),
        ),
    )
}
