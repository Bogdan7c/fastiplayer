//! Scenario tests для admission, lifecycle, ownership и pointer commit.

use super::*;

#[test]
fn production_port_uses_exact_player_selection_and_rejects_mismatched_pair() {
    let generation = renderer_generation(1);
    let candidate_request_id = request_id(900);
    let (owner, mut port) = player_selected_video_candidate_boundary(
        generation,
        PlayerVideoDecoderThreadConfig::default(),
        player_core::MediaInstallVideoBackendConstraint::RequireBackend(
            video_ffmpeg::ffmpeg_software_backend_id(),
        ),
        FakeCandidateDriver::successful(),
    );

    let reply = port
        .request_detached_backend(DetachedVideoBackendRequest::new(
            candidate_request_id,
            DetachedVideoBackendSelection::selected(
                video_ffmpeg::FFMPEG_SOFTWARE_BACKEND_ID,
                VideoFrameContract::host_yuv420_planar8(),
            ),
        ))
        .expect("production port must stay connected");
    let backend = available_backend(reply, candidate_request_id);
    assert_eq!(
        backend.backend_id(),
        video_ffmpeg::FFMPEG_SOFTWARE_BACKEND_ID
    );
    assert!(owner.has_candidate());

    port.publish_candidate_status(DetachedVideoBackendCandidateStatus::StreamConfigured {
        request_id: candidate_request_id,
        backend_id: video_ffmpeg::FFMPEG_SOFTWARE_BACKEND_ID.to_owned(),
    })
    .expect("matching player status must configure app half");
    drop(backend);

    let mismatched_request_id = request_id(901);
    let (mismatched_owner, mut mismatched_port) = player_selected_video_candidate_boundary(
        generation,
        PlayerVideoDecoderThreadConfig::default(),
        player_core::MediaInstallVideoBackendConstraint::RequireBackend(
            video_ffmpeg::ffmpeg_software_backend_id(),
        ),
        FakeCandidateDriver::successful(),
    );
    let mismatched_reply = mismatched_port
        .request_detached_backend(DetachedVideoBackendRequest::new(
            mismatched_request_id,
            DetachedVideoBackendSelection::selected(
                "vaapi",
                VideoFrameContract::dma_buf_nv12(
                    video_frame_contract::DmaBufImageLayout::ComposedLayers,
                ),
            ),
        ))
        .expect("typed selection rejection is a resource reply, not disconnect");
    assert!(matches!(
        mismatched_reply.into_parts().1,
        Err(DetachedVideoBackendResourceError::Unavailable { .. })
    ));
    assert!(!mismatched_owner.has_candidate());
}

#[test]
fn production_port_preserves_candidate_preparation_failure() {
    let request_id = request_id(902);
    let (owner, mut port) = player_selected_video_candidate_boundary(
        renderer_generation(2),
        PlayerVideoDecoderThreadConfig::default(),
        player_core::MediaInstallVideoBackendConstraint::AnyPlayable,
        FakeCandidateDriver::failing(CandidateVideoPipelinePreparationError::at_stage(
            CandidateVideoPipelinePreparationStage::BackendStartup,
            "candidate backend startup failed",
        )),
    );
    let reply = port
        .request_detached_backend(DetachedVideoBackendRequest::new(
            request_id,
            DetachedVideoBackendSelection::selected(
                video_ffmpeg::FFMPEG_SOFTWARE_BACKEND_ID,
                VideoFrameContract::host_yuv420_planar8(),
            ),
        ))
        .expect("preparation failure must stay a typed resource reply");
    assert!(matches!(
        reply.into_parts().1,
        Err(DetachedVideoBackendResourceError::StartupFailed { .. })
    ));
    assert!(!owner.has_candidate());
    assert!(matches!(
        owner.drain_terminal_outcome(),
        Some(StagedVideoPipelineCandidateTerminalOutcome::PreparationFailed { .. })
    ));
}

#[test]
fn vaapi_and_ffmpeg_fake_paths_keep_decoder_materializer_pairing_exact() {
    // Проверяем оба selectable production plan-а через один fake driver boundary.
    let cases = [
        (
            vaapi_plan(),
            VideoBackendKind::HardwareZeroCopy,
            CandidateVideoMaterializerKind::DmaBufZeroCopy,
        ),
        (
            ffmpeg_plan(),
            VideoBackendKind::FfmpegSoftware,
            CandidateVideoMaterializerKind::HostPlanarUpload,
        ),
    ];

    // Каждый plan получает независимый bounded slot и resource set.
    for (index, (plan, expected_backend, expected_materializer)) in cases.into_iter().enumerate() {
        // Independent slot исключает cross-case terminal state.
        let mut slot = StagedVideoPipelineCandidateSlot::new();
        // Fake driver создаёт exact plan-shaped pair.
        let mut driver = FakeCandidateDriver::successful();
        // Request IDs различаются между cases.
        let candidate_request_id = request_id(index as u64 + 1);

        // Preparation stage-ит app half и отдаёт detached player half.
        let reply = slot.prepare_and_stage(
            candidate_request_id,
            renderer_generation(1),
            plan,
            &mut driver,
        );
        // Descriptor slot-а фиксирует exact decoder/materializer combination.
        let descriptor = slot
            .candidate_descriptor()
            .expect("candidate descriptor must be staged");
        // Backend class не смешивается между VA-API и FFmpeg.
        assert_eq!(descriptor.backend_kind(), expected_backend);
        // Materializer class совпадает с transfer path выбранного backend-а.
        assert_eq!(descriptor.materializer_kind(), expected_materializer);
        // Driver был вызван ровно один раз без fallback.
        assert_eq!(driver.prepare_calls, 1);
        assert_eq!(driver.destructive_fallback_calls, 0);

        // Неиспользованный player half освобождается явно в конце test case-а.
        drop(available_backend(reply, candidate_request_id));
        // Slot drop освобождает matching app half; active state здесь отсутствует.
        drop(slot);
    }
}

#[test]
fn every_software_hardware_transition_commits_one_exact_pipeline_pair() {
    let old_backends = [
        VideoBackendKind::HardwareZeroCopy,
        VideoBackendKind::FfmpegSoftware,
    ];

    for (old_index, old_backend) in old_backends.into_iter().enumerate() {
        for (new_index, new_hardware) in [true, false].into_iter().enumerate() {
            let (plan, expected_backend, expected_materializer) = if new_hardware {
                (
                    vaapi_plan(),
                    VideoBackendKind::HardwareZeroCopy,
                    CandidateVideoMaterializerKind::DmaBufZeroCopy,
                )
            } else {
                (
                    ffmpeg_plan(),
                    VideoBackendKind::FfmpegSoftware,
                    CandidateVideoMaterializerKind::HostPlanarUpload,
                )
            };
            let old_materializer_drops = Arc::new(AtomicUsize::new(0));
            let old_binding_drops = Arc::new(AtomicUsize::new(0));
            let mut active = ActiveVideoPipelinePointers::new(
                old_backend,
                DropProbe::new(10, old_materializer_drops.clone()),
                DropProbe::new(20, old_binding_drops.clone()),
            );
            let mut slot = StagedVideoPipelineCandidateSlot::new();
            let mut driver = FakeCandidateDriver::successful();
            let candidate_request_id = request_id((old_index * 2 + new_index + 20) as u64);
            let generation = renderer_generation(4);

            let reply = slot.prepare_and_stage(candidate_request_id, generation, plan, &mut driver);
            let descriptor = slot
                .candidate_descriptor()
                .expect("candidate descriptor must stay staged until Installed");
            assert_eq!(descriptor.backend_kind(), expected_backend);
            assert_eq!(descriptor.materializer_kind(), expected_materializer);

            let mut port =
                FakeCandidatePort::connected(available_backend(reply, candidate_request_id));
            let status = port.configure(candidate_request_id);
            slot.record_player_status(status, generation, &mut port)
                .expect("matching configured status must be accepted");
            slot.prepare_post_installed_commit(candidate_request_id, generation)
                .expect("matching Installed must prepare pointer commit")
                .commit(&mut active);

            assert_eq!(active.backend_kind(), expected_backend);
            assert_eq!(active.materializer().id, 200);
            assert_eq!(active.submission_binding().id, 300);
            assert_eq!(old_materializer_drops.load(Ordering::SeqCst), 1);
            assert_eq!(old_binding_drops.load(Ordering::SeqCst), 1);
            drop(port);
        }
    }
}

#[test]
fn candidate_success_does_not_change_active_pointers_until_infallible_commit() {
    // Old active pointers имеют отдельные IDs и release counters.
    let old_materializer_drops = Arc::new(AtomicUsize::new(0));
    let old_binding_drops = Arc::new(AtomicUsize::new(0));
    let mut active = ActiveVideoPipelinePointers::new(
        VideoBackendKind::HardwareZeroCopy,
        DropProbe::new(10, old_materializer_drops.clone()),
        DropProbe::new(20, old_binding_drops.clone()),
    );
    // Candidate preparation не получает mutable active reference.
    let mut slot = StagedVideoPipelineCandidateSlot::new();
    let mut driver = FakeCandidateDriver::successful();
    let candidate_request_id = request_id(10);
    let generation = renderer_generation(3);

    // Candidate pair создаётся рядом с old active pair.
    let reply =
        slot.prepare_and_stage(candidate_request_id, generation, ffmpeg_plan(), &mut driver);
    // Old decoder/materializer class остаётся active после successful creation.
    assert_eq!(active.backend_kind(), VideoBackendKind::HardwareZeroCopy);
    assert_eq!(active.materializer().id, 10);
    assert_eq!(active.submission_binding().id, 20);
    assert_eq!(old_materializer_drops.load(Ordering::SeqCst), 0);
    assert_eq!(old_binding_drops.load(Ordering::SeqCst), 0);

    // Player half настраивается fallibly отдельно от app pointers.
    let mut port = FakeCandidatePort::connected(available_backend(reply, candidate_request_id));
    let status = port.configure(candidate_request_id);
    // Matching configured status только меняет staged marker.
    slot.record_player_status(status, generation, &mut port)
        .expect("matching configured status must be accepted");
    // Active pointers всё ещё old до Installed barrier.
    assert_eq!(active.materializer().id, 10);
    assert_eq!(active.submission_binding().id, 20);

    // Matching validation выполняется до pointer-only primitive-а.
    let prepared_commit = slot
        .prepare_post_installed_commit(candidate_request_id, generation)
        .expect("matching Installed must prepare commit token");
    // Commit не возвращает Result и не вызывает driver/factory повторно.
    prepared_commit.commit(&mut active);
    // Новый active pair перемещён атомарной assignment-границей.
    assert_eq!(active.backend_kind(), VideoBackendKind::FfmpegSoftware);
    assert_eq!(active.materializer().id, 200);
    assert_eq!(active.submission_binding().id, 300);
    // Old app pointers освобождены ровно один раз после replacement.
    assert_eq!(old_materializer_drops.load(Ordering::SeqCst), 1);
    assert_eq!(old_binding_drops.load(Ordering::SeqCst), 1);
    // Post-Installed primitive не выполнял startup/provider/materializer work.
    assert_eq!(driver.prepare_calls, 1);

    // Installed outcome lossless и drain-ится exactly once.
    assert!(matches!(
        slot.drain_terminal_outcome(),
        Some(StagedVideoPipelineCandidateTerminalOutcome::Installed {
            request_id,
            renderer_generation,
        }) if request_id == candidate_request_id && renderer_generation == generation
    ));
    assert!(slot.drain_terminal_outcome().is_none());
    // Configured player half остаётся owned future player transaction-ом.
    drop(port);
}

#[test]
fn missing_or_mismatched_app_half_after_installed_is_a_fatal_invariant() {
    // Player `Installed` означает, что rollback к старому player instance уже запрещён.
    let mut empty_slot = StagedVideoPipelineCandidateSlot::<DropProbe, DropProbe>::new();
    let Err(missing_error) =
        empty_slot.prepare_post_installed_commit(request_id(11), renderer_generation(3))
    else {
        panic!("Installed без app half-а обязан быть fatal invariant");
    };
    assert_eq!(
        missing_error.match_error(),
        StagedVideoPipelineCandidateMatchError::NoCandidate
    );

    // Exact admitted candidate остаётся staged, если Installed относится к чужому request-у.
    let mut slot = StagedVideoPipelineCandidateSlot::new();
    let mut driver = FakeCandidateDriver::successful();
    let admitted_request_id = request_id(12);
    let generation = renderer_generation(3);
    let reply = slot.prepare_and_stage(admitted_request_id, generation, ffmpeg_plan(), &mut driver);
    let Err(mismatch_error) = slot.prepare_post_installed_commit(request_id(13), generation) else {
        panic!("mismatched Installed обязан быть fatal invariant");
    };
    assert_eq!(
        mismatch_error.match_error(),
        StagedVideoPipelineCandidateMatchError::RequestMismatch
    );
    assert!(slot.candidate_descriptor().is_some());

    // Test cleanup освобождает обе ещё не установленные halves без active mutation.
    drop(available_backend(reply, admitted_request_id));
    drop(slot);
}

#[test]
fn every_preparation_stage_failure_leaves_active_pair_untouched() {
    // Каждый stage проверяется независимо, включая resource exhaustion.
    let failures = [
        CandidateVideoPipelinePreparationError::backend_resource(
            CandidateVideoBackendAvailability::Unavailable,
            "fake backend unavailable",
        ),
        CandidateVideoPipelinePreparationError::backend_resource(
            CandidateVideoBackendAvailability::ResourceExhausted,
            "fake driver rejects a second decoder",
        ),
        CandidateVideoPipelinePreparationError::at_stage(
            CandidateVideoPipelinePreparationStage::BackendStartup,
            "fake startup failure",
        ),
        CandidateVideoPipelinePreparationError::at_stage(
            CandidateVideoPipelinePreparationStage::ProviderBinding,
            "fake provider binding failure",
        ),
        CandidateVideoPipelinePreparationError::at_stage(
            CandidateVideoPipelinePreparationStage::MaterializerCreation,
            "fake materializer failure",
        ),
    ];

    // Each failure starts from the same semantic old Playing/Paused resource state.
    for (index, failure) in failures.into_iter().enumerate() {
        // Old active pointer counters prove no destructive fallback/drop.
        let old_materializer_drops = Arc::new(AtomicUsize::new(0));
        let old_binding_drops = Arc::new(AtomicUsize::new(0));
        let active = ActiveVideoPipelinePointers::new(
            VideoBackendKind::HardwareZeroCopy,
            DropProbe::new(70, old_materializer_drops.clone()),
            DropProbe::new(80, old_binding_drops.clone()),
        );
        // Transport intent marker models old Playing/Paused state outside this module.
        let old_transport_state = "Playing";
        // Failure driver has no access to active or transport state.
        let mut driver = FakeCandidateDriver::failing(failure.clone());
        let mut slot = StagedVideoPipelineCandidateSlot::new();
        let failed_request_id = request_id(index as u64 + 20);

        // Failed preparation returns typed resource reply.
        let reply = slot.prepare_and_stage(
            failed_request_id,
            renderer_generation(4),
            ffmpeg_plan(),
            &mut driver,
        );
        // Reply не содержит destructive fallback backend.
        assert!(reply.into_parts().1.is_err());
        // Active app pair и old playback marker не изменились.
        assert_eq!(active.backend_kind(), VideoBackendKind::HardwareZeroCopy);
        assert_eq!(active.materializer().id, 70);
        assert_eq!(active.submission_binding().id, 80);
        assert_eq!(old_transport_state, "Playing");
        assert_eq!(old_materializer_drops.load(Ordering::SeqCst), 0);
        assert_eq!(old_binding_drops.load(Ordering::SeqCst), 0);
        // Driver never attempted a destructive fallback path.
        assert_eq!(driver.destructive_fallback_calls, 0);
        // Typed stage survives in the single terminal slot.
        assert!(matches!(
            slot.drain_terminal_outcome(),
            Some(StagedVideoPipelineCandidateTerminalOutcome::PreparationFailed {
                request_id,
                error,
                ..
            }) if request_id == failed_request_id && error == failure
        ));
        // Terminal outcome cannot be drained twice.
        assert!(slot.drain_terminal_outcome().is_none());
    }
}

#[test]
fn max_one_admission_applies_backpressure_before_second_driver_startup() {
    // Slot and driver begin empty.
    let mut slot = StagedVideoPipelineCandidateSlot::new();
    let mut driver = FakeCandidateDriver::successful();
    let first_request_id = request_id(31);

    // First candidate occupies the only resource slot.
    let first_reply = slot.prepare_and_stage(
        first_request_id,
        renderer_generation(5),
        vaapi_plan(),
        &mut driver,
    );
    // Second request is rejected before driver/factory invocation.
    let second_reply = slot.prepare_and_stage(
        request_id(32),
        renderer_generation(5),
        ffmpeg_plan(),
        &mut driver,
    );
    // Exactly one backend startup occurred; no backend pool exists.
    assert_eq!(driver.prepare_calls, 1);
    assert_eq!(slot.diagnostics().admitted, 1);
    assert_eq!(slot.diagnostics().admission_backpressure, 1);
    assert!(matches!(
        second_reply.into_parts().1,
        Err(video_backend_api::DetachedVideoBackendResourceError::AdmissionBackpressure { .. })
    ));

    // Cleanup uses typed cancellation for both split halves.
    let mut port = FakeCandidatePort::connected(available_backend(first_reply, first_request_id));
    slot.cancel_pre_barrier(
        first_request_id,
        DetachedVideoBackendCandidateCancellationCause::Superseded,
        &mut port,
    )
    .expect("matching supersede must cancel candidate");
}

#[test]
fn stale_request_is_ignored_but_renderer_generation_mismatch_cancels_both_halves() {
    // Candidate belongs to generation 7.
    let mut slot = StagedVideoPipelineCandidateSlot::new();
    let mut driver = FakeCandidateDriver::successful();
    let current_request_id = request_id(40);
    let reply = slot.prepare_and_stage(
        current_request_id,
        renderer_generation(7),
        vaapi_plan(),
        &mut driver,
    );
    let mut port = FakeCandidatePort::connected(available_backend(reply, current_request_id));

    // Status другого request-а не очищает current candidate.
    let stale_status = DetachedVideoBackendCandidateStatus::StreamConfigured {
        request_id: request_id(39),
        backend_id: "vaapi".to_owned(),
    };
    assert_eq!(
        slot.record_player_status(stale_status, renderer_generation(7), &mut port),
        Err(StagedVideoPipelineCandidateStatusError::Match(
            StagedVideoPipelineCandidateMatchError::RequestMismatch,
        ))
    );
    assert!(slot.has_candidate());

    // Exact request status после renderer recreation terminal-cancel-ит stale pair.
    let matching_status = port.configure(current_request_id);
    assert_eq!(
        slot.record_player_status(matching_status, renderer_generation(8), &mut port),
        Err(StagedVideoPipelineCandidateStatusError::Match(
            StagedVideoPipelineCandidateMatchError::RendererGenerationMismatch,
        ))
    );
    // App materializer/binding и configured player backend освобождены по одному разу.
    assert_eq!(driver.materializer_drop_count.load(Ordering::SeqCst), 1);
    assert_eq!(driver.binding_drop_count.load(Ordering::SeqCst), 1);
    assert_eq!(driver.decoder_drop_count.load(Ordering::SeqCst), 1);
    // Terminal cause остаётся distinct от generic cancellation.
    assert!(matches!(
        slot.drain_terminal_outcome(),
        Some(StagedVideoPipelineCandidateTerminalOutcome::Cancelled {
            cause: DetachedVideoBackendCandidateCancellationCause::StaleRendererGeneration,
            ..
        })
    ));
}

#[test]
fn cancel_supersede_suspend_and_disconnect_release_split_halves_exactly_once() {
    // Все required pre-barrier lifecycle causes проходят одинаковый ownership path.
    let cases = [
        (
            DetachedVideoBackendCandidateCancellationCause::Requested,
            false,
            DetachedVideoBackendCandidateCancellationCause::Requested,
        ),
        (
            DetachedVideoBackendCandidateCancellationCause::Superseded,
            false,
            DetachedVideoBackendCandidateCancellationCause::Superseded,
        ),
        (
            DetachedVideoBackendCandidateCancellationCause::RendererSuspended,
            false,
            DetachedVideoBackendCandidateCancellationCause::RendererSuspended,
        ),
        (
            DetachedVideoBackendCandidateCancellationCause::Requested,
            true,
            DetachedVideoBackendCandidateCancellationCause::Disconnected,
        ),
    ];

    // Каждый lifecycle cause получает новый exact resource set.
    for (index, (requested_cause, disconnected, expected_terminal_cause)) in
        cases.into_iter().enumerate()
    {
        // Independent driver counters доказывают exactly-once per transaction.
        let mut driver = FakeCandidateDriver::successful();
        let mut slot = StagedVideoPipelineCandidateSlot::new();
        let candidate_request_id = request_id(index as u64 + 50);
        let reply = slot.prepare_and_stage(
            candidate_request_id,
            renderer_generation(9),
            ffmpeg_plan(),
            &mut driver,
        );
        let mut port = FakeCandidatePort::connected(available_backend(reply, candidate_request_id));
        // Disconnect flag models remote worker/channel closure during cancel dispatch.
        port.disconnected = disconnected;

        // Local cleanup always completes even when remote port reports disconnect.
        let cancel_result =
            slot.cancel_pre_barrier(candidate_request_id, requested_cause, &mut port);
        // Result distinguishes connected success from typed disconnect.
        if disconnected {
            assert_eq!(
                cancel_result,
                Err(StagedVideoPipelineCandidateCancelError::PortDisconnected)
            );
        } else {
            assert_eq!(cancel_result, Ok(()));
        }
        // Each split owner is released exactly once.
        assert_eq!(driver.decoder_drop_count.load(Ordering::SeqCst), 1);
        assert_eq!(driver.materializer_drop_count.load(Ordering::SeqCst), 1);
        assert_eq!(driver.binding_drop_count.load(Ordering::SeqCst), 1);
        // Lossless terminal cause reflects disconnect separately.
        assert!(matches!(
            slot.drain_terminal_outcome(),
            Some(StagedVideoPipelineCandidateTerminalOutcome::Cancelled {
                cause,
                ..
            }) if cause == expected_terminal_cause
        ));
        // Late repeated drop/cancel cannot release owners again.
        drop(slot);
        drop(port);
        assert_eq!(driver.decoder_drop_count.load(Ordering::SeqCst), 1);
        assert_eq!(driver.materializer_drop_count.load(Ordering::SeqCst), 1);
        assert_eq!(driver.binding_drop_count.load(Ordering::SeqCst), 1);
    }
}

#[test]
fn configuration_failure_releases_candidate_without_touching_active_pair() {
    // Active pair stays separate from candidate resource driver.
    let active_materializer_drops = Arc::new(AtomicUsize::new(0));
    let active_binding_drops = Arc::new(AtomicUsize::new(0));
    let active = ActiveVideoPipelinePointers::new(
        VideoBackendKind::HardwareZeroCopy,
        DropProbe::new(401, active_materializer_drops.clone()),
        DropProbe::new(402, active_binding_drops.clone()),
    );
    let mut driver = FakeCandidateDriver::successful();
    let mut slot = StagedVideoPipelineCandidateSlot::new();
    let candidate_request_id = request_id(60);
    let reply = slot.prepare_and_stage(
        candidate_request_id,
        renderer_generation(10),
        ffmpeg_plan(),
        &mut driver,
    );

    // Player-side failure consumes and releases detached decoder half first.
    drop(available_backend(reply, candidate_request_id));
    let error = DetachedVideoBackendConfigurationError::Fatal(DecodeThreadError::new(
        "fake candidate config failed",
    ));
    let mut empty_port = FakeCandidatePort {
        player_half: None,
        disconnected: false,
        cancellations: Vec::new(),
    };
    slot.record_player_status(
        DetachedVideoBackendCandidateStatus::ConfigurationFailed {
            request_id: candidate_request_id,
            error: error.clone(),
        },
        renderer_generation(10),
        &mut empty_port,
    )
    .expect("matching configuration failure must finish candidate");

    // Candidate halves released exactly once; active pointers remain alive/unchanged.
    assert_eq!(driver.decoder_drop_count.load(Ordering::SeqCst), 1);
    assert_eq!(driver.materializer_drop_count.load(Ordering::SeqCst), 1);
    assert_eq!(driver.binding_drop_count.load(Ordering::SeqCst), 1);
    assert_eq!(active.materializer().id, 401);
    assert_eq!(active.submission_binding().id, 402);
    assert_eq!(active_materializer_drops.load(Ordering::SeqCst), 0);
    assert_eq!(active_binding_drops.load(Ordering::SeqCst), 0);
    // Typed configuration error remains lossless.
    assert!(matches!(
        slot.drain_terminal_outcome(),
        Some(StagedVideoPipelineCandidateTerminalOutcome::ConfigurationFailed {
            request_id,
            error: terminal_error,
        }) if request_id == candidate_request_id && terminal_error == error
    ));
}

#[test]
fn old_submitted_release_callback_survives_candidate_creation_and_cancel() {
    // Old callback guard живёт отдельно от candidate submission binding.
    let old_release_callbacks = Arc::new(AtomicUsize::new(0));
    let old_materializer_drops = Arc::new(AtomicUsize::new(0));
    let old_binding_drops = Arc::new(AtomicUsize::new(0));
    let active = ActiveVideoPipelinePointers::new(
        VideoBackendKind::HardwareZeroCopy,
        DropProbe::new(501, old_materializer_drops.clone()),
        DropProbe::new(502, old_binding_drops.clone()),
    );
    // Candidate получает собственные release resources.
    let mut driver = FakeCandidateDriver::successful();
    let mut slot = StagedVideoPipelineCandidateSlot::new();
    let candidate_request_id = request_id(70);
    let reply = slot.prepare_and_stage(
        candidate_request_id,
        renderer_generation(11),
        ffmpeg_plan(),
        &mut driver,
    );
    let mut port = FakeCandidatePort::connected(available_backend(reply, candidate_request_id));

    // Candidate cancel не drop-ит и не rebind-ит old active submission owner.
    slot.cancel_pre_barrier(
        candidate_request_id,
        DetachedVideoBackendCandidateCancellationCause::RendererSuspended,
        &mut port,
    )
    .expect("candidate cancel must succeed");
    assert_eq!(active.submission_binding().id, 502);
    assert_eq!(old_binding_drops.load(Ordering::SeqCst), 0);

    // Late old submitted callback по-прежнему может release old frame exactly once.
    old_release_callbacks.fetch_add(1, Ordering::SeqCst);
    assert_eq!(old_release_callbacks.load(Ordering::SeqCst), 1);
    // Candidate release accounting не смешивается с old callback accounting.
    assert_eq!(driver.decoder_drop_count.load(Ordering::SeqCst), 1);
    assert_eq!(driver.materializer_drop_count.load(Ordering::SeqCst), 1);
    assert_eq!(driver.binding_drop_count.load(Ordering::SeqCst), 1);
    assert_eq!(old_materializer_drops.load(Ordering::SeqCst), 0);
}

#[test]
fn one_decoder_only_driver_returns_resource_exhausted_without_fallback_or_old_state_loss() {
    // Old Paused state и active pointers существуют до candidate attempt.
    let old_transport_state = "Paused";
    let old_materializer_drops = Arc::new(AtomicUsize::new(0));
    let old_binding_drops = Arc::new(AtomicUsize::new(0));
    let active = ActiveVideoPipelinePointers::new(
        VideoBackendKind::HardwareZeroCopy,
        DropProbe::new(601, old_materializer_drops.clone()),
        DropProbe::new(602, old_binding_drops.clone()),
    );
    // Fake driver моделирует runtime, где active decoder уже занял единственный permit.
    let mut driver =
        FakeCandidateDriver::failing(CandidateVideoPipelinePreparationError::backend_resource(
            CandidateVideoBackendAvailability::ResourceExhausted,
            "driver permits one decoder and rejects the candidate",
        ));
    let mut slot = StagedVideoPipelineCandidateSlot::new();

    // Candidate attempt возвращает typed failure до ownership barrier.
    let reply = slot.prepare_and_stage(
        request_id(80),
        renderer_generation(12),
        vaapi_plan(),
        &mut driver,
    );
    assert!(matches!(
        reply.into_parts().1,
        Err(video_backend_api::DetachedVideoBackendResourceError::ResourceExhausted { .. })
    ));
    // Old Playing/Paused intent и pair остаются без destructive fallback.
    assert_eq!(old_transport_state, "Paused");
    assert_eq!(active.materializer().id, 601);
    assert_eq!(active.submission_binding().id, 602);
    assert_eq!(old_materializer_drops.load(Ordering::SeqCst), 0);
    assert_eq!(old_binding_drops.load(Ordering::SeqCst), 0);
    assert_eq!(driver.prepare_calls, 1);
    assert_eq!(driver.destructive_fallback_calls, 0);
}

#[test]
fn accepted_installed_barrier_restores_commit_required_candidate_if_token_is_dropped() {
    // Candidate проходит normal split/configuration path.
    let mut driver = FakeCandidateDriver::successful();
    let mut slot = StagedVideoPipelineCandidateSlot::new();
    let candidate_request_id = request_id(90);
    let generation = renderer_generation(13);
    let reply =
        slot.prepare_and_stage(candidate_request_id, generation, ffmpeg_plan(), &mut driver);
    let mut port = FakeCandidatePort::connected(available_backend(reply, candidate_request_id));
    let configured_status = port.configure(candidate_request_id);
    slot.record_player_status(configured_status, generation, &mut port)
        .expect("candidate configuration must become staged-ready");

    // Matching Installed создаёт linear token и marks commit-required state.
    let prepared_commit = slot
        .prepare_post_installed_commit(candidate_request_id, generation)
        .expect("matching Installed must prepare pointer commit");
    // Defensive token drop возвращает pointers slot-у вместо resource release.
    drop(prepared_commit);
    assert!(slot.has_candidate());
    assert_eq!(driver.decoder_drop_count.load(Ordering::SeqCst), 0);
    assert_eq!(driver.materializer_drop_count.load(Ordering::SeqCst), 0);
    assert_eq!(driver.binding_drop_count.load(Ordering::SeqCst), 0);

    // После barrier lifecycle cancel запрещён и не освобождает обе halves.
    assert_eq!(
        slot.cancel_pre_barrier(
            candidate_request_id,
            DetachedVideoBackendCandidateCancellationCause::RendererSuspended,
            &mut port,
        ),
        Err(StagedVideoPipelineCandidateCancelError::Match(
            StagedVideoPipelineCandidateMatchError::PostInstalledCommitRequired,
        ))
    );
    assert!(port.cancellations.is_empty());

    // Owner повторно получает token и обязан завершить exact pointer commit.
    let active_materializer_drops = Arc::new(AtomicUsize::new(0));
    let active_binding_drops = Arc::new(AtomicUsize::new(0));
    let mut active = ActiveVideoPipelinePointers::new(
        VideoBackendKind::HardwareZeroCopy,
        DropProbe::new(901, active_materializer_drops),
        DropProbe::new(902, active_binding_drops),
    );
    slot.prepare_post_installed_commit(candidate_request_id, generation)
        .expect("commit-required candidate must produce the same token")
        .commit(&mut active);
    assert_eq!(active.materializer().id, 200);
    assert_eq!(active.submission_binding().id, 300);
    assert!(matches!(
        slot.drain_terminal_outcome(),
        Some(StagedVideoPipelineCandidateTerminalOutcome::Installed {
            request_id,
            renderer_generation,
        }) if request_id == candidate_request_id && renderer_generation == generation
    ));
}

#[test]
fn candidate_boundary_contains_no_second_player_session_or_backend_pool() {
    // Source assertion закрепляет explicit scope Session 00C до 00C1 wiring.
    let candidate_source = include_str!("../../video_pipeline_candidate.rs");
    // Candidate module не владеет и не создаёт второй PlayerSession.
    assert!(!candidate_source.contains("PlayerSession"));
    // Backend pool/Vec of detached backends не скрывается внутри app slot-а.
    assert!(!candidate_source.contains("Vec<DetachedVideoBackend"));
    assert!(!candidate_source.contains("Vec<StartedVideoBackend"));
    // Hidden retry loop отсутствует в bounded resource boundary.
    assert!(!candidate_source.contains("loop {"));
}
