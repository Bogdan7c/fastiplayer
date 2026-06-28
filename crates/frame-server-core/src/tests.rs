use std::time::Duration;

use codec_core::{VideoColorMetadata, VideoDisplayOrientation};
use media_core::{MediaTime, TimeBase, TrackId, TrackTimestamp};
use video_core::{DecodedFrame, FrameResourceHandle, VideoFrameDiagnostics};
use video_frame_contract::{DmaBufImageLayout, VideoFrameContract};
use video_present_core::VideoPresentFrameResourceDescriptor;

use crate::*;

fn generation_token(playback: u64, scrub: u64) -> ScrubGenerationToken {
    ScrubGenerationToken::new(
        PlaybackGeneration::new(playback),
        ScrubGeneration::new(scrub),
    )
}

fn target_for_tests(track_id: TrackId, millis: u64) -> ScrubTarget {
    let time_base = TimeBase::new(1, 1_000).expect("валидная test timebase");
    ScrubTarget::new(
        MediaTime::from_millis(millis),
        TrackTimestamp::new(track_id, millis as i64, time_base),
    )
}

fn context_for_tests(request_kind: ScrubRequestKind) -> ScrubTargetContext {
    let video_track = TrackId::new(7);
    ScrubTargetContext::new(
        SourceRevision::new(10),
        BackendRevision::new(20),
        ScrubTrackSelection::with_audio(video_track, TrackId::new(8)),
        target_for_tests(video_track, 1_250),
        ScrubExactnessPolicy::TargetOrAfter,
        request_kind,
        generation_token(30, 40),
    )
}

fn descriptor_for_tests(
    resource_handle: FrameResourceHandle,
) -> VideoPresentFrameResourceDescriptor {
    let decoded_frame = DecodedFrame {
        generation: 30,
        pts: Duration::from_millis(1_250),
        frame_contract: VideoFrameContract::dma_buf_nv12(DmaBufImageLayout::SeparateLayers),
        width: 640,
        height: 360,
        render_width: 640,
        render_height: 360,
        display_orientation: VideoDisplayOrientation::Identity,
        color: VideoColorMetadata::sdr_bt709_limited(),
        resource_handle,
        diagnostics: VideoFrameDiagnostics::default(),
    };

    VideoPresentFrameResourceDescriptor::from_decoded_frame(2, &decoded_frame)
}

fn preview_frame_for_tests(generation: ScrubGenerationToken) -> ScrubPreviewFrame {
    let video_track = TrackId::new(7);
    ScrubPreviewFrame {
        generation,
        actual_time: MediaTime::from_millis(1_250),
        actual_pts: target_for_tests(video_track, 1_250).target_pts,
        resource: descriptor_for_tests(FrameResourceHandle(42)),
    }
}

#[test]
fn priority_ordering_keeps_user_commit_above_live_and_hover_work() {
    assert!(ScrubPriority::UserCommit > ScrubPriority::LiveScrub);
    assert!(ScrubPriority::LiveScrub > ScrubPriority::HoverPreview);
    assert!(ScrubPriority::HoverPreview > ScrubPriority::BackgroundPrepare);
    assert_eq!(
        ScrubPriority::for_request_kind(ScrubRequestKind::SeekLanding),
        ScrubPriority::UserCommit
    );
}

#[test]
fn context_source_revision_mismatch_marks_intent_stale() {
    let context = context_for_tests(ScrubRequestKind::SeekLanding);
    let intent = ScrubIntent::PrepareTarget(PrepareTargetIntent { context });
    let current = ScrubCurrentGuards::new(
        SourceRevision::new(11),
        BackendRevision::new(20),
        generation_token(30, 40),
    );

    assert_eq!(
        intent.stale_reason_against(current),
        Some(ScrubStaleReason::SourceRevisionMismatch {
            context_revision: SourceRevision::new(10),
            current_revision: SourceRevision::new(11),
        })
    );
}

#[test]
fn context_scrub_generation_mismatch_marks_intent_stale() {
    let context = context_for_tests(ScrubRequestKind::LiveScrub);
    let current = ScrubCurrentGuards::new(
        SourceRevision::new(10),
        BackendRevision::new(20),
        generation_token(30, 41),
    );

    assert_eq!(
        context.stale_reason_against(current),
        Some(ScrubStaleReason::ScrubGenerationMismatch {
            context_generation: ScrubGeneration::new(40),
            current_generation: ScrubGeneration::new(41),
        })
    );
}

#[test]
fn generation_token_mismatch_on_either_field_marks_outcome_frame_and_readiness_stale() {
    let context = context_for_tests(ScrubRequestKind::HoverPreview);
    let frame = preview_frame_for_tests(context.generation());
    let outcome = ScrubDriverOutcome::ExactFrameReady(ExactFrameReadyOutcome { context, frame });

    let playback_mismatch = ScrubCurrentGuards::new(
        SourceRevision::new(10),
        BackendRevision::new(20),
        generation_token(31, 40),
    );
    assert_eq!(
        outcome.stale_reason_against(playback_mismatch),
        Some(ScrubStaleReason::PlaybackGenerationMismatch {
            context_generation: PlaybackGeneration::new(30),
            current_generation: PlaybackGeneration::new(31),
        })
    );
    assert_eq!(
        frame.stale_reason_against_generation(generation_token(31, 40)),
        Some(ScrubStaleReason::PlaybackGenerationMismatch {
            context_generation: PlaybackGeneration::new(30),
            current_generation: PlaybackGeneration::new(31),
        })
    );

    let readiness =
        ScrubFrameReadiness::ready(frame).mark_stale_for_generation(generation_token(30, 41));
    assert_eq!(
        readiness.state,
        ScrubFrameReadinessState::Stale {
            reason: ScrubStaleReason::ScrubGenerationMismatch {
                context_generation: ScrubGeneration::new(40),
                current_generation: ScrubGeneration::new(41),
            },
        }
    );
}

#[test]
fn scrub_intent_uses_only_the_coarse_accepted_set() {
    assert_eq!(
        ScrubIntent::accepted_kinds(),
        &[
            ScrubIntentKind::PrepareTarget,
            ScrubIntentKind::SeekDecodePointBefore,
            ScrubIntentKind::FeedAndDrain,
            ScrubIntentKind::Finish,
            ScrubIntentKind::Cancel,
        ]
    );
}

#[test]
fn every_scrub_intent_carries_required_guards_through_context() {
    let context = context_for_tests(ScrubRequestKind::SeekLanding);
    let intents = [
        ScrubIntent::PrepareTarget(PrepareTargetIntent { context }),
        ScrubIntent::SeekDecodePointBefore(SeekDecodePointBeforeIntent { context }),
        ScrubIntent::FeedAndDrain(FeedAndDrainIntent {
            context,
            stop_condition: FeedAndDrainStopCondition::PreviewFrameReady,
        }),
        ScrubIntent::Finish(FinishScrubIntent {
            context,
            policy: FinishScrubPolicy::CommitVisiblePreview,
        }),
        ScrubIntent::Cancel(CancelScrubIntent {
            context,
            reason: CancelScrubReason::UserCancelled,
        }),
    ];

    for intent in intents {
        assert_eq!(intent.context().source_revision(), SourceRevision::new(10));
        assert_eq!(
            intent.context().backend_revision(),
            BackendRevision::new(20)
        );
        assert_eq!(
            intent.context().track_selection().video_track,
            TrackId::new(7)
        );
        assert_eq!(
            intent.context().target().media_time,
            MediaTime::from_millis(1_250)
        );
        assert_eq!(
            intent.context().exactness_policy(),
            ScrubExactnessPolicy::TargetOrAfter
        );
        assert_eq!(intent.context().generation(), generation_token(30, 40));
    }

    assert_eq!(
        context.priority(),
        ScrubPriority::for_request_kind(context.request_kind())
    );
}

#[test]
fn seek_decode_point_before_intent_carries_target_revision_and_generation_only() {
    let context = context_for_tests(ScrubRequestKind::SeekLanding);
    let intent = ScrubIntent::SeekDecodePointBefore(SeekDecodePointBeforeIntent { context });

    assert_eq!(
        intent.context().target().media_time,
        MediaTime::from_millis(1_250)
    );
    assert_eq!(intent.context().source_revision(), SourceRevision::new(10));
    assert_eq!(
        intent.context().backend_revision(),
        BackendRevision::new(20)
    );
    assert_eq!(intent.context().generation(), generation_token(30, 40));
}

#[test]
fn outcome_to_event_mapping_preserves_reason_categories_in_public_failure() {
    let context = context_for_tests(ScrubRequestKind::HoverPreview);
    let outcome = ScrubDriverOutcome::DemuxUnsupported(DemuxUnsupportedOutcome {
        context,
        reason: DemuxUnsupportedReason::DecodePointBeforeUnsupported,
    });

    let event = ScrubEvent::from_driver_outcome(outcome);
    assert_eq!(
        event,
        ScrubEvent::Failed(ScrubFailedEvent {
            context,
            reason: ScrubFailureReason::DemuxUnsupported,
            diagnostics: ScrubEventDiagnostics::with_driver_reason(
                ScrubDriverOutcomeKind::DemuxUnsupported,
                ScrubDriverDiagnosticReason::DemuxUnsupported(
                    DemuxUnsupportedReason::DecodePointBeforeUnsupported,
                ),
            ),
        })
    );
}

#[test]
fn public_events_are_normalized_and_driver_details_stay_in_diagnostics() {
    let context = context_for_tests(ScrubRequestKind::LiveScrub);
    let frame = preview_frame_for_tests(context.generation());

    let events = [
        ScrubEvent::from_driver_outcome(ScrubDriverOutcome::Prepared(PreparedOutcome { context })),
        ScrubEvent::from_driver_outcome(ScrubDriverOutcome::Progressed(ProgressedOutcome {
            context,
            progress: ScrubProgress {
                packets_fed: 3,
                frames_drained: 1,
                target_status: ScrubTargetReachStatus::BeforeTarget,
            },
        })),
        ScrubEvent::from_driver_outcome(ScrubDriverOutcome::PreTargetReleased(
            PreTargetReleasedOutcome {
                context,
                released_frame: frame,
                progress: ScrubProgress {
                    packets_fed: 4,
                    frames_drained: 2,
                    target_status: ScrubTargetReachStatus::BeforeTarget,
                },
            },
        )),
        ScrubEvent::from_driver_outcome(ScrubDriverOutcome::ExactFrameReady(
            ExactFrameReadyOutcome { context, frame },
        )),
        ScrubEvent::from_driver_outcome(ScrubDriverOutcome::AudioResumePending(
            AudioResumePendingOutcome {
                context,
                budget: AudioResumeBudgetMetadata::supplied_by_driver(
                    Duration::from_millis(50),
                    Duration::from_millis(10),
                ),
            },
        )),
        ScrubEvent::from_driver_outcome(ScrubDriverOutcome::Finished(FinishedOutcome {
            context,
            committed_time: MediaTime::from_millis(1_250),
        })),
        ScrubEvent::from_driver_outcome(ScrubDriverOutcome::MatchedPlayback(
            MatchedPlaybackOutcome {
                context,
                matched_time: MediaTime::from_millis(1_250),
            },
        )),
        ScrubEvent::from_driver_outcome(ScrubDriverOutcome::Cancelled(CancelledOutcome {
            context,
            reason: CancelScrubReason::SupersededByNewTarget,
        })),
        ScrubEvent::from_driver_outcome(ScrubDriverOutcome::DecoderBackpressure(
            DecoderBackpressureOutcome {
                context,
                reason: DecoderBackpressureReason::PacketQueueFull,
            },
        )),
    ];

    assert!(matches!(events[0], ScrubEvent::Started(_)));
    assert!(matches!(events[1], ScrubEvent::Progress(_)));
    assert!(matches!(events[2], ScrubEvent::Progress(_)));
    assert!(matches!(events[3], ScrubEvent::PreviewFrameReady(_)));
    assert!(matches!(events[4], ScrubEvent::ResumePending(_)));
    assert!(matches!(events[5], ScrubEvent::Committed(_)));
    assert!(matches!(events[6], ScrubEvent::MatchedPlayback(_)));
    assert!(matches!(events[7], ScrubEvent::Cancelled(_)));
    assert!(matches!(events[8], ScrubEvent::Failed(_)));

    match events[2] {
        ScrubEvent::Progress(payload) => {
            assert_eq!(
                payload.diagnostics.driver_outcome,
                ScrubDriverOutcomeKind::PreTargetReleased
            );
        }
        other => panic!("ожидали normalized Progress event, получили {other:?}"),
    }

    match events[3] {
        ScrubEvent::PreviewFrameReady(payload) => {
            assert_eq!(
                payload.diagnostics.driver_outcome,
                ScrubDriverOutcomeKind::ExactFrameReady
            );
        }
        other => panic!("ожидали normalized PreviewFrameReady event, получили {other:?}"),
    }

    match events[8] {
        ScrubEvent::Failed(payload) => {
            assert_eq!(
                payload.diagnostics.driver_outcome,
                ScrubDriverOutcomeKind::DecoderBackpressure
            );
            assert_eq!(payload.reason, ScrubFailureReason::DecoderBackpressure);
            assert_eq!(
                payload.diagnostics.driver_reason,
                Some(ScrubDriverDiagnosticReason::DecoderBackpressure(
                    DecoderBackpressureReason::PacketQueueFull
                ))
            );
        }
        other => panic!("ожидали normalized Failed event, получили {other:?}"),
    }
}

#[test]
fn driver_outcome_variants_use_named_payloads_and_typed_subreasons() {
    let context = context_for_tests(ScrubRequestKind::HoverPreview);
    let outcomes = [
        ScrubDriverOutcome::AudioResumePending(AudioResumePendingOutcome {
            context,
            budget: AudioResumeBudgetMetadata::supplied_by_driver(
                Duration::from_millis(80),
                Duration::from_millis(12),
            ),
        }),
        ScrubDriverOutcome::AudioResumeTimedOut(AudioResumeTimedOutOutcome {
            context,
            budget: AudioResumeBudgetMetadata::timing_unknown_fallback(
                Duration::from_millis(500),
                Duration::from_millis(500),
            ),
        }),
        ScrubDriverOutcome::AudioResumeFailed(AudioResumeFailedOutcome {
            context,
            reason: AudioResumeErrorReason::OutputClosed,
            budget: None,
        }),
        ScrubDriverOutcome::PreTargetReleased(PreTargetReleasedOutcome {
            context,
            released_frame: preview_frame_for_tests(context.generation()),
            progress: ScrubProgress {
                packets_fed: 2,
                frames_drained: 1,
                target_status: ScrubTargetReachStatus::BeforeTarget,
            },
        }),
        ScrubDriverOutcome::ExactFrameReady(ExactFrameReadyOutcome {
            context,
            frame: preview_frame_for_tests(context.generation()),
        }),
        ScrubDriverOutcome::DemuxUnsupported(DemuxUnsupportedOutcome {
            context,
            reason: DemuxUnsupportedReason::NonSeekableSource,
        }),
        ScrubDriverOutcome::DemuxUnavailable(DemuxUnavailableOutcome {
            context,
            reason: DemuxUnavailableReason::SourceGone,
        }),
        ScrubDriverOutcome::DecoderBackpressure(DecoderBackpressureOutcome {
            context,
            reason: DecoderBackpressureReason::DecoderControlChannelFull,
        }),
        ScrubDriverOutcome::HostUploadBackpressure(HostUploadBackpressureOutcome {
            context,
            reason: HostUploadBackpressureReason::UploadSlotsExhausted,
        }),
        ScrubDriverOutcome::ResourceBusy(ResourceBusyOutcome {
            context,
            reason: ResourceBusyReason::BackendResourcePressure,
        }),
        ScrubDriverOutcome::StaleGeneration(StaleGenerationOutcome {
            context,
            reason: ScrubStaleReason::ScrubGenerationMismatch {
                context_generation: ScrubGeneration::new(40),
                current_generation: ScrubGeneration::new(41),
            },
        }),
        ScrubDriverOutcome::Cancelled(CancelledOutcome {
            context,
            reason: CancelScrubReason::UserCancelled,
        }),
        ScrubDriverOutcome::TimedOut(ScrubTimedOutOutcome {
            context,
            reason: ScrubTimeoutReason::FrameReadinessDeadline,
            elapsed: Duration::from_millis(120),
        }),
        ScrubDriverOutcome::Fatal(FatalOutcome {
            context,
            reason: ScrubFatalReason::BackendContractViolated,
        }),
    ];

    let kinds: Vec<ScrubDriverOutcomeKind> =
        outcomes.iter().map(ScrubDriverOutcome::kind).collect();
    assert_eq!(
        kinds,
        vec![
            ScrubDriverOutcomeKind::AudioResumePending,
            ScrubDriverOutcomeKind::AudioResumeTimedOut,
            ScrubDriverOutcomeKind::AudioResumeFailed,
            ScrubDriverOutcomeKind::PreTargetReleased,
            ScrubDriverOutcomeKind::ExactFrameReady,
            ScrubDriverOutcomeKind::DemuxUnsupported,
            ScrubDriverOutcomeKind::DemuxUnavailable,
            ScrubDriverOutcomeKind::DecoderBackpressure,
            ScrubDriverOutcomeKind::HostUploadBackpressure,
            ScrubDriverOutcomeKind::ResourceBusy,
            ScrubDriverOutcomeKind::StaleGeneration,
            ScrubDriverOutcomeKind::Cancelled,
            ScrubDriverOutcomeKind::TimedOut,
            ScrubDriverOutcomeKind::Fatal,
        ]
    );
}

#[test]
fn preview_scrub_is_the_only_main_video_real_preview_entrypoint() {
    assert_eq!(
        MainVideoRealPreviewEntrypoint::accepted_entrypoints(),
        &[MainVideoRealPreviewEntrypoint::PreviewScrub]
    );
    assert_eq!(
        MainVideoRealPreviewEntrypoint::PreviewScrub.command_name(),
        "PreviewScrub"
    );
}

#[test]
fn audio_resume_budget_is_neutral_driver_supplied_metadata() {
    let metadata = AudioResumeBudgetMetadata::supplied_by_driver(
        Duration::from_millis(75),
        Duration::from_millis(20),
    );

    assert_eq!(metadata.budget, Duration::from_millis(75));
    assert_eq!(metadata.elapsed, Duration::from_millis(20));
    assert_eq!(
        metadata.source,
        AudioResumeBudgetSource::SuppliedByExternalDriver
    );

    let manifest = include_str!("../Cargo.toml");
    assert!(!manifest.contains("audio-core"));
    assert!(!manifest.contains("audio ="));
}

#[test]
fn config_validation_rejects_impossible_values() {
    assert_eq!(
        FrameServerConfig {
            max_feed_and_drain_driver_steps: 0,
            ..FrameServerConfig::default()
        }
        .validate(),
        Err(FrameServerConfigError::ZeroMaxFeedAndDrainDriverSteps)
    );
    assert_eq!(
        FrameServerConfig {
            stale_outcome_cancel_threshold: 0,
            ..FrameServerConfig::default()
        }
        .validate(),
        Err(FrameServerConfigError::ZeroStaleOutcomeCancelThreshold)
    );
    assert_eq!(
        FrameServerConfig {
            resume_pending_event_interval: Duration::ZERO,
            ..FrameServerConfig::default()
        }
        .validate(),
        Err(FrameServerConfigError::ZeroResumePendingEventInterval)
    );
    assert_eq!(
        FrameServerConfig {
            live_scrub_max_hz: 0,
            ..FrameServerConfig::default()
        }
        .validate(),
        Err(FrameServerConfigError::ZeroLiveScrubMaxHz)
    );
    assert_eq!(
        FrameServerConfig {
            live_scrub_max_hz: MAX_LIVE_SCRUB_MAX_HZ + 1,
            ..FrameServerConfig::default()
        }
        .validate(),
        Err(FrameServerConfigError::LiveScrubMaxHzTooHigh {
            max_allowed: MAX_LIVE_SCRUB_MAX_HZ,
            actual: MAX_LIVE_SCRUB_MAX_HZ + 1,
        })
    );
    assert_eq!(
        FrameServerConfig {
            hover_prepare_window_slots: 0,
            ..FrameServerConfig::default()
        }
        .validate(),
        Err(FrameServerConfigError::ZeroHoverPrepareWindowSlots)
    );
    assert_eq!(
        FrameServerConfig {
            hover_prepare_window_slots: MAX_HOVER_PREPARE_WINDOW_SLOTS + 1,
            ..FrameServerConfig::default()
        }
        .validate(),
        Err(FrameServerConfigError::HoverPrepareWindowSlotsTooHigh {
            max_allowed: MAX_HOVER_PREPARE_WINDOW_SLOTS,
            actual: MAX_HOVER_PREPARE_WINDOW_SLOTS + 1,
        })
    );
    assert_eq!(
        FrameServerConfig {
            software_hover_prepare_window_slots: 0,
            ..FrameServerConfig::default()
        }
        .validate(),
        Err(FrameServerConfigError::ZeroSoftwareHoverPrepareWindowSlots)
    );
    assert_eq!(
        FrameServerConfig {
            software_hover_prepare_window_slots: MAX_SOFTWARE_HOVER_PREPARE_WINDOW_SLOTS + 1,
            ..FrameServerConfig::default()
        }
        .validate(),
        Err(
            FrameServerConfigError::SoftwareHoverPrepareWindowSlotsTooHigh {
                max_allowed: MAX_SOFTWARE_HOVER_PREPARE_WINDOW_SLOTS,
                actual: MAX_SOFTWARE_HOVER_PREPARE_WINDOW_SLOTS + 1,
            }
        )
    );
    assert_eq!(
        FrameServerConfig {
            recent_superseded_prepare_slots: MAX_RECENT_SUPERSEDED_PREPARE_SLOTS + 1,
            ..FrameServerConfig::default()
        }
        .validate(),
        Err(
            FrameServerConfigError::RecentSupersededPrepareSlotsTooHigh {
                max_allowed: MAX_RECENT_SUPERSEDED_PREPARE_SLOTS,
                actual: MAX_RECENT_SUPERSEDED_PREPARE_SLOTS + 1,
            }
        )
    );
    assert_eq!(
        FrameServerConfig {
            software_recent_superseded_prepare_slots: MAX_SOFTWARE_RECENT_SUPERSEDED_PREPARE_SLOTS
                + 1,
            ..FrameServerConfig::default()
        }
        .validate(),
        Err(
            FrameServerConfigError::SoftwareRecentSupersededPrepareSlotsTooHigh {
                max_allowed: MAX_SOFTWARE_RECENT_SUPERSEDED_PREPARE_SLOTS,
                actual: MAX_SOFTWARE_RECENT_SUPERSEDED_PREPARE_SLOTS + 1,
            }
        )
    );
    FrameServerConfig {
        recent_superseded_prepare_slots: 0,
        software_recent_superseded_prepare_slots: 0,
        ..FrameServerConfig::default()
    }
    .validate()
    .expect("zero recent retention disables only click-back retention");
    let default_config = FrameServerConfig::default()
        .validate()
        .expect("default frame-server config stays valid");
    assert_eq!(
        default_config.live_scrub_max_hz(),
        DEFAULT_LIVE_SCRUB_MAX_HZ
    );
    assert_eq!(
        default_config.live_scrub_decode_mode(),
        LiveScrubDecodeMode::ThrottledLatest
    );
    assert_eq!(
        default_config.hover_prepare_window_slots(),
        DEFAULT_HOVER_PREPARE_WINDOW_SLOTS
    );
    assert_eq!(
        default_config.software_hover_prepare_window_slots(),
        DEFAULT_SOFTWARE_HOVER_PREPARE_WINDOW_SLOTS
    );
    assert_eq!(
        default_config.recent_superseded_prepare_slots(),
        DEFAULT_RECENT_SUPERSEDED_PREPARE_SLOTS
    );
    assert_eq!(
        default_config.software_recent_superseded_prepare_slots(),
        DEFAULT_SOFTWARE_RECENT_SUPERSEDED_PREPARE_SLOTS
    );
    let default_recent_budget =
        TimelineHoverRecentSupersededBudget::from_validated_config(default_config);
    assert_eq!(
        default_recent_budget.general_slots(),
        usize::from(DEFAULT_RECENT_SUPERSEDED_PREPARE_SLOTS)
    );
    assert_eq!(
        default_recent_budget.software_slots(),
        usize::from(DEFAULT_SOFTWARE_RECENT_SUPERSEDED_PREPARE_SLOTS)
    );
    assert!(FrameServerConfig::default().validate().is_ok());
}
