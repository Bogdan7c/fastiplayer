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
