use std::time::Duration;

use media_core::{MediaTime, TimeBase, TrackId, TrackTimestamp};

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

fn stale_generation_reason_for_tests() -> ScrubStaleReason {
    ScrubStaleReason::ScrubGenerationMismatch {
        context_generation: ScrubGeneration::new(40),
        current_generation: ScrubGeneration::new(41),
    }
}

fn live_scrub_settings_for_tests(
    decode_mode: LiveScrubDecodeMode,
    max_hz: u16,
) -> LiveScrubSettingsSnapshot {
    LiveScrubSettingsSnapshot {
        decode_mode,
        max_hz,
    }
}

fn working_set_key_for_tests() -> TimelineHoverPrepareFrameKey {
    let video_track = TrackId::new(7);
    TimelineHoverPrepareFrameKey::new(
        SourceRevision::new(10),
        ScrubTrackSelection::video_only(video_track),
        BackendRevision::new(20),
        generation_token(30, 40),
        FrameExactnessPolicy::TargetOrAfter,
        TimelineHoverFrameBucket::new(125),
    )
}

#[test]
fn request_lifecycle_counters_are_split_by_request_kind() {
    let mut diagnostics = ScrubDiagnosticsRecorder::new();

    diagnostics.record_request_accepted(ScrubRequestKind::SeekLanding);
    diagnostics.record_request_accepted(ScrubRequestKind::SeekLanding);
    diagnostics.record_request_accepted(ScrubRequestKind::LiveScrub);
    diagnostics.record_request_cancelled(ScrubRequestKind::LiveScrub);
    diagnostics.record_request_completed(ScrubRequestKind::HoverPreview);
    diagnostics.record_request_completed(ScrubRequestKind::TimelineHoverPrepareWindow);

    let snapshot = diagnostics.snapshot();

    assert_eq!(
        snapshot
            .requests
            .accepted
            .get(ScrubRequestKind::SeekLanding),
        2
    );
    assert_eq!(
        snapshot.requests.accepted.get(ScrubRequestKind::LiveScrub),
        1
    );
    assert_eq!(
        snapshot.requests.cancelled.get(ScrubRequestKind::LiveScrub),
        1
    );
    assert_eq!(
        snapshot
            .requests
            .completed
            .get(ScrubRequestKind::HoverPreview),
        1
    );
    assert_eq!(
        snapshot
            .requests
            .completed
            .get(ScrubRequestKind::TimelineHoverPrepareWindow),
        1
    );
    assert_eq!(
        snapshot
            .requests
            .cancelled
            .get(ScrubRequestKind::SeekLanding),
        0
    );
}

#[test]
fn cancelled_and_stale_generation_outcomes_stay_separate() {
    let context = context_for_tests(ScrubRequestKind::LiveScrub);
    let mut diagnostics = ScrubDiagnosticsRecorder::new();

    diagnostics.record_driver_outcome(&ScrubDriverOutcome::Cancelled(CancelledOutcome {
        context,
        reason: CancelScrubReason::UserCancelled,
    }));
    diagnostics.record_driver_outcome(&ScrubDriverOutcome::StaleGeneration(
        StaleGenerationOutcome {
            context,
            reason: stale_generation_reason_for_tests(),
        },
    ));

    let snapshot = diagnostics.snapshot();

    assert_eq!(snapshot.outcomes.get(ScrubDriverOutcomeKind::Cancelled), 1);
    assert_eq!(
        snapshot
            .outcomes
            .get(ScrubDriverOutcomeKind::StaleGeneration),
        1
    );
    assert_eq!(snapshot.driver_reasons.stale_generation, 1);
    assert_eq!(snapshot.driver_reasons.fatal, 0);
}

#[test]
fn cold_decode_progress_is_counted_without_renaming_driver_outcome() {
    let context = context_for_tests(ScrubRequestKind::LiveScrub);
    let mut diagnostics = ScrubDiagnosticsRecorder::new();

    diagnostics.record_driver_outcome(&ScrubDriverOutcome::Progressed(ProgressedOutcome {
        context,
        progress: ScrubProgress {
            packets_fed: 8,
            frames_drained: 2,
            target_status: ScrubTargetReachStatus::BeforeTarget,
        },
    }));
    diagnostics.record_driver_outcome(&ScrubDriverOutcome::Progressed(ProgressedOutcome {
        context,
        progress: ScrubProgress {
            packets_fed: 1,
            frames_drained: 1,
            target_status: ScrubTargetReachStatus::TargetOrAfter,
        },
    }));

    let snapshot = diagnostics.snapshot();

    assert_eq!(snapshot.outcomes.progressed, 2);
    assert_eq!(snapshot.outcomes.cold_decode_in_progress, 1);
}

#[test]
fn prepared_hit_diagnostics_keep_resume_ready_runway_pending_and_cold_split() {
    let mut diagnostics = ScrubDiagnosticsRecorder::new();

    diagnostics.record_prepared_frame_hit(ScrubPreparedFrameHitOutcome::ResumePending {
        reason: ScrubPreparedFrameResumePendingReason::RunwayPending(
            ScrubResumeRunwayState::PostTargetPacketAccepted,
        ),
    });
    diagnostics.record_prepared_frame_hit(ScrubPreparedFrameHitOutcome::ResumePending {
        reason: ScrubPreparedFrameResumePendingReason::AudioGatePending {
            video_runway: ScrubResumeRunwayState::DisplayableFrameQueued,
        },
    });
    diagnostics.record_prepared_frame_hit(ScrubPreparedFrameHitOutcome::ResumeReady {
        video_runway: ScrubResumeRunwayState::NextFrameAlmostReady,
    });
    diagnostics.record_cold_exact_decode_pending();

    let snapshot = diagnostics.snapshot();
    let prepared = snapshot.prepared_frames;

    assert_eq!(prepared.prepared_frame_hits, 3);
    assert_eq!(prepared.resume_ready_prepared_hits, 1);
    assert_eq!(prepared.prepared_frame_resume_runway_pending, 1);
    assert_eq!(prepared.prepared_frame_audio_gate_pending, 1);
    assert_eq!(prepared.cold_exact_decode_pending, 1);
    assert_eq!(prepared.resume_pending_reasons.runway_pending, 1);
    assert_eq!(prepared.resume_pending_reasons.audio_gate_pending, 1);
    assert_eq!(prepared.video_runway.post_target_packet_accepted, 1);
    assert_eq!(prepared.video_runway.displayable_frame_queued, 1);
    assert_eq!(prepared.video_runway.next_frame_almost_ready, 1);
    assert_eq!(prepared.video_runway.progress_only, 1);
    assert_eq!(prepared.video_runway.commit_ready, 2);
}

#[test]
fn prepared_ownership_diagnostics_keep_promotion_demote_and_release_paths() {
    let mut diagnostics = ScrubDiagnosticsRecorder::new();

    diagnostics.record_prepared_frame_ownership_event(
        ScrubPreparedFrameOwnershipEvent::PromotedResumeReadyBranch,
    );
    diagnostics.record_prepared_frame_ownership_event(
        ScrubPreparedFrameOwnershipEvent::PromotedVisualOverrideResumePending,
    );
    diagnostics.record_prepared_frame_ownership_event(
        ScrubPreparedFrameOwnershipEvent::DemotedToRecentSuperseded,
    );
    diagnostics.record_prepared_frame_ownership_event(
        ScrubPreparedFrameOwnershipEvent::DemoteRejected(
            ScrubPreparedFrameDemoteRejectionKind::PromotedKeyNotCurrent,
        ),
    );
    diagnostics.record_prepared_frame_ownership_event(
        ScrubPreparedFrameOwnershipEvent::ReleasedWithoutDemote,
    );
    diagnostics.record_prepared_frame_ownership_event(
        ScrubPreparedFrameOwnershipEvent::NoPromotedFrameOnRelease,
    );

    let ownership = diagnostics.snapshot().prepared_frames.ownership;

    assert_eq!(ownership.promoted_to_seek_ownership, 2);
    assert_eq!(ownership.promoted_resume_ready_branch, 1);
    assert_eq!(ownership.promoted_visual_override_resume_pending, 1);
    assert_eq!(ownership.demoted_to_recent_superseded, 1);
    assert_eq!(ownership.demote_rejected, 1);
    assert_eq!(
        ownership.demote_rejection_reasons.promoted_key_not_current,
        1
    );
    assert_eq!(ownership.released_without_demote, 1);
    assert_eq!(ownership.no_promoted_frame_on_release, 1);
}

#[test]
fn typed_driver_pressure_and_failure_counts_do_not_collapse() {
    let context = context_for_tests(ScrubRequestKind::SeekLanding);
    let mut diagnostics = ScrubDiagnosticsRecorder::new();

    let outcomes = [
        ScrubDriverOutcome::DemuxUnavailable(DemuxUnavailableOutcome {
            context,
            reason: DemuxUnavailableReason::DemuxerClosed,
        }),
        ScrubDriverOutcome::DemuxUnsupported(DemuxUnsupportedOutcome {
            context,
            reason: DemuxUnsupportedReason::DecodePointBeforeUnsupported,
        }),
        ScrubDriverOutcome::DecoderBackpressure(DecoderBackpressureOutcome {
            context,
            reason: DecoderBackpressureReason::PacketQueueFull,
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
            reason: stale_generation_reason_for_tests(),
        }),
        ScrubDriverOutcome::TimedOut(ScrubTimedOutOutcome {
            context,
            reason: ScrubTimeoutReason::DriverStepBudgetExceeded,
            elapsed: Duration::from_millis(250),
        }),
        ScrubDriverOutcome::Fatal(FatalOutcome {
            context,
            reason: ScrubFatalReason::BackendContractViolated,
        }),
    ];

    for outcome in outcomes {
        diagnostics.record_driver_outcome(&outcome);
    }

    let snapshot = diagnostics.snapshot();

    assert_eq!(snapshot.outcomes.demux_unavailable, 1);
    assert_eq!(snapshot.outcomes.demux_unsupported, 1);
    assert_eq!(snapshot.outcomes.decoder_backpressure, 1);
    assert_eq!(snapshot.outcomes.host_upload_backpressure, 1);
    assert_eq!(snapshot.outcomes.resource_busy, 1);
    assert_eq!(snapshot.outcomes.stale_generation, 1);
    assert_eq!(snapshot.outcomes.timed_out, 1);
    assert_eq!(snapshot.outcomes.fatal, 1);

    assert_eq!(snapshot.driver_reasons.demux_unavailable, 1);
    assert_eq!(snapshot.driver_reasons.demux_unsupported, 1);
    assert_eq!(snapshot.driver_reasons.decoder_backpressure, 1);
    assert_eq!(snapshot.driver_reasons.host_upload_backpressure, 1);
    assert_eq!(snapshot.driver_reasons.resource_busy, 1);
    assert_eq!(snapshot.driver_reasons.stale_generation, 1);
    assert_eq!(snapshot.driver_reasons.timeout, 1);
    assert_eq!(snapshot.driver_reasons.fatal, 1);

    assert_eq!(snapshot.resource_pressure.decoder_backpressure, 1);
    assert_eq!(
        snapshot
            .resource_pressure
            .decoder_backpressure_reasons
            .packet_queue_full,
        1
    );
    assert_eq!(snapshot.resource_pressure.host_upload_backpressure, 1);
    assert_eq!(
        snapshot
            .resource_pressure
            .host_upload_backpressure_reasons
            .upload_slots_exhausted,
        1
    );
    assert_eq!(snapshot.resource_pressure.resource_busy, 1);
    assert_eq!(
        snapshot
            .resource_pressure
            .resource_busy_reasons
            .backend_resource_pressure,
        1
    );
}

#[test]
fn live_scrub_event_diagnostics_keep_snapshot_deferred_change_and_throttle_skip() {
    let mut diagnostics = ScrubDiagnosticsRecorder::new();
    let pointer_down = live_scrub_settings_for_tests(LiveScrubDecodeMode::ThrottledLatest, 60);
    let changed_once = live_scrub_settings_for_tests(LiveScrubDecodeMode::EveryDragEvent, 60);
    let changed_latest = live_scrub_settings_for_tests(LiveScrubDecodeMode::EveryDragEvent, 120);
    let mut live_scrub = LiveScrubDiagnostics::from_settings_snapshot(pointer_down);

    live_scrub.record_throttled_latest_skip();
    live_scrub.record_deferred_settings_change(DeferredLiveScrubSettingsChange {
        old_snapshot: pointer_down,
        new_snapshot: changed_once,
    });
    live_scrub.record_deferred_settings_change(DeferredLiveScrubSettingsChange {
        old_snapshot: changed_once,
        new_snapshot: changed_latest,
    });

    diagnostics.record_event_diagnostics(
        ScrubEventDiagnostics::with_driver_reason(
            ScrubDriverOutcomeKind::DecoderBackpressure,
            ScrubDriverDiagnosticReason::DecoderBackpressure(
                DecoderBackpressureReason::PacketQueueFull,
            ),
        )
        .with_live_scrub(live_scrub),
    );

    let snapshot = diagnostics.snapshot();
    let latest_live_scrub = snapshot
        .latest_live_scrub
        .expect("live scrub diagnostics must be retained without an event history");

    assert_eq!(latest_live_scrub.settings_snapshot, pointer_down);
    assert_eq!(latest_live_scrub.throttled_latest_skip_count, 1);
    assert_eq!(
        latest_live_scrub.deferred_live_scrub_settings_change_count,
        2
    );
    assert_eq!(
        latest_live_scrub.latest_deferred_live_scrub_settings_change,
        Some(DeferredLiveScrubSettingsChange {
            old_snapshot: changed_once,
            new_snapshot: changed_latest,
        })
    );
    assert_eq!(snapshot.scheduler.live_scrub_throttled, 0);
    assert_eq!(snapshot.resource_pressure.decoder_backpressure, 1);
}

#[test]
fn latency_count_and_working_set_summaries_keep_pressure_evidence() {
    let mut diagnostics = ScrubDiagnosticsRecorder::new();

    diagnostics.record_queue_age(Duration::from_millis(4));
    diagnostics.record_queue_age(Duration::from_millis(9));
    diagnostics.record_decode_latency(Duration::from_millis(17));
    diagnostics.record_demux_seek_latency(Duration::from_millis(23));
    diagnostics.record_packets_from_decode_point_to_target(6);
    diagnostics.record_packets_from_decode_point_to_target(10);
    diagnostics.record_pre_target_frame_drops(2);
    diagnostics.record_pre_target_frame_drops(5);

    diagnostics.record_working_set_hit();
    let lookup_miss = TimelineHoverPrepareLookupOutcome::<()>::Miss(
        TimelineHoverPrepareLookupMissReason::NoEntryForKey,
    );
    diagnostics.record_working_set_lookup_outcome(&lookup_miss);
    let insert_outcome = TimelineHoverPrepareInsertOutcome::<()>::Inserted {
        slot_plan: TimelineHoverPrepareSlotPlan::EvictOldestPrimaryByproduct,
        evicted_primary_byproducts: 2,
    };
    diagnostics.record_working_set_insert_outcome(&insert_outcome);
    diagnostics.record_working_set_pressure_release_outcome(
        TimelineHoverPreparePressureReleaseOutcome::ReleasedRecentSuperseded {
            released_key: working_set_key_for_tests(),
        },
    );
    diagnostics.record_working_set_pressure_release_outcome(
        TimelineHoverPreparePressureReleaseOutcome::NothingReleased {
            reason: TimelineHoverPreparePressureReleaseMissReason::NoHoverOwnedEntries,
        },
    );

    let snapshot = diagnostics.snapshot();

    assert_eq!(snapshot.queue_age.samples, 2);
    assert_eq!(snapshot.queue_age.total, Duration::from_millis(13));
    assert_eq!(snapshot.queue_age.min, Some(Duration::from_millis(4)));
    assert_eq!(snapshot.queue_age.max, Some(Duration::from_millis(9)));
    assert_eq!(snapshot.decode_latency.samples, 1);
    assert_eq!(snapshot.demux_seek_latency.samples, 1);

    assert_eq!(snapshot.packets_from_decode_point_to_target.samples, 2);
    assert_eq!(snapshot.packets_from_decode_point_to_target.total, 16);
    assert_eq!(snapshot.pre_target_frame_drops.samples, 2);
    assert_eq!(snapshot.pre_target_frame_drops.max, Some(5));

    assert_eq!(snapshot.working_set.hits, 1);
    assert_eq!(snapshot.working_set.misses, 1);
    assert_eq!(snapshot.working_set.evictions, 3);
    assert_eq!(snapshot.working_set.released_recent_superseded, 1);
    assert_eq!(snapshot.working_set.pressure_release_misses, 1);
}

#[test]
fn hover_prepare_span_and_network_diagnostics_are_typed_and_bounded() {
    let mut diagnostics = ScrubDiagnosticsRecorder::new();

    diagnostics.record_hover_prepare_provider_budget(
        TimelineHoverPrepareProviderBudget::SpareSlotAvailable,
    );
    diagnostics.record_hover_prepare_admission_outcome(
        &TimelineHoverPrepareAdmissionOutcome::Admitted {
            slot_plan: TimelineHoverPrepareSlotPlan::UseSparePrimarySlot,
        },
    );
    diagnostics.record_hover_prepare_admission_outcome(
        &TimelineHoverPrepareAdmissionOutcome::NoOp {
            reason: TimelineHoverPrepareNoOpReason::NoSpareHoverSlot {
                capacity: 1,
                used_slots: 1,
                protected_key: working_set_key_for_tests(),
            },
        },
    );
    diagnostics.record_hover_dependency_span_outcome(ScrubHoverDependencySpanOutcome::Resolved);
    diagnostics.record_hover_dependency_span_outcome(ScrubHoverDependencySpanOutcome::Incomplete(
        ScrubHoverDependencySpanIncompleteReason::DecodeExecutionNotWired,
    ));
    diagnostics.record_hover_dependency_span_progress(ScrubHoverDependencySpanProgress {
        packets_decoded_to_target: 11,
        frames_decoded_to_target: 4,
        post_target_reorder_drain_frames: 2,
        prepared_targets_produced: 3,
    });
    diagnostics.record_hover_network_state(ScrubHoverNetworkState::Opening);
    diagnostics.record_hover_network_state(ScrubHoverNetworkState::Throttled);
    diagnostics.record_hover_network_zero_throttle_no_delay();
    diagnostics.record_hover_network_latest_only_replaced_in_flight();
    diagnostics.record_hover_network_stale_late_result_ignored();
    diagnostics.record_hover_network_throttle_delay(Duration::from_millis(75));

    let snapshot = diagnostics.snapshot();
    let hover = snapshot.hover_prepare;

    assert_eq!(hover.admission.provider_spare_slot_available, 1);
    assert_eq!(hover.admission.admitted, 1);
    assert_eq!(hover.admission.use_spare_primary_slot, 1);
    assert_eq!(hover.admission.no_op, 1);
    assert_eq!(hover.admission.no_spare_hover_slot, 1);

    assert_eq!(hover.dependency_span.resolved, 1);
    assert_eq!(hover.dependency_span.incomplete, 1);
    assert_eq!(
        hover
            .dependency_span
            .incomplete_reasons
            .decode_execution_not_wired,
        1
    );
    assert_eq!(hover.dependency_span.packets_decoded_to_target.total, 11);
    assert_eq!(
        hover.dependency_span.latest_progress,
        Some(ScrubHoverDependencySpanProgress {
            packets_decoded_to_target: 11,
            frames_decoded_to_target: 4,
            post_target_reorder_drain_frames: 2,
            prepared_targets_produced: 3,
        })
    );

    assert_eq!(hover.network.opening, 1);
    assert_eq!(hover.network.throttled, 1);
    assert_eq!(
        hover.network.latest_state,
        Some(ScrubHoverNetworkState::Throttled)
    );
    assert_eq!(hover.network.zero_throttle_no_delay, 1);
    assert_eq!(hover.network.latest_only_replaced_in_flight, 1);
    assert_eq!(hover.network.stale_late_result_ignored, 1);
    assert_eq!(
        hover.network.throttle_delay.max,
        Some(Duration::from_millis(75))
    );
}
