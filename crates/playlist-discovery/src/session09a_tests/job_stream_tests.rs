use super::*;

#[test]
fn sibling_filters_use_topology_and_snapshot_is_immutable() {
    let video = LocalMediaKind::VideoContaining;
    let audio = LocalMediaKind::AudioOnly;
    assert!(SiblingFilter::VideoOnly.admits(video, video));
    assert!(!SiblingFilter::VideoOnly.admits(video, audio));
    assert!(SiblingFilter::AudioOnly.admits(video, audio));
    assert!(SiblingFilter::AllMedia.admits(audio, video));
    assert!(SiblingFilter::SameAsOpened.admits(video, video));
    assert!(!SiblingFilter::SameAsOpened.admits(video, audio));

    let snapshot = SiblingDiscoveryPolicySnapshot::new(
        true,
        SiblingFilter::SameAsOpened,
        SiblingPolicyRevision::new(41),
    );
    assert_eq!(snapshot.filter(), SiblingFilter::SameAsOpened);
    assert_eq!(snapshot.revision().get(), 41);
}

#[test]
fn disabled_sibling_snapshot_completes_without_probe_io() {
    let directory = TestDirectory::new();
    let target = directory.path.join("01-target.media");
    fs::write(&target, b"target").unwrap();
    fs::write(directory.path.join("02-video.media"), b"sibling").unwrap();
    let request = SiblingDiscoveryRequest::new(
        Arc::new(build_directory_manifest(&target).unwrap()),
        LocalMediaKind::VideoContaining,
        SiblingDiscoveryPolicySnapshot::new(
            false,
            SiblingFilter::AllMedia,
            SiblingPolicyRevision::new(42),
        ),
        DiscoveryRequestRevision::new(42),
    );
    let (executor, started, _gate) = fake_executor();
    let handle = executor.submit(DiscoveryRequest::Sibling(request)).unwrap();
    let summary = wait_for_final(&handle);
    assert_eq!(summary.outcome, DiscoveryFinalOutcome::Completed);
    assert_eq!(summary.verified, 0);
    assert_eq!(summary.failed, 0);
    assert!(started.recv_timeout(Duration::from_millis(50)).is_err());
    assert!(handle.drain_events().is_empty());
}

#[test]
fn mixed_manual_failures_publish_success_once_and_bounded_summary() {
    let (executor, _started, _gate) = fake_executor();
    let handle = executor
        .submit(DiscoveryRequest::ManualBatch {
            locators: vec!["video-ok".into(), "fail-bad".into(), "audio-ok".into()],
            request_revision: DiscoveryRequestRevision::new(7),
        })
        .unwrap();
    let summary = wait_for_final(&handle);
    assert_eq!(summary.outcome, DiscoveryFinalOutcome::Completed);
    assert_eq!(summary.verified, 2);
    assert_eq!(summary.failed, 1);
    assert_eq!(summary.failure_counts.probe_failed, 1);
    let events = handle.drain_events();
    assert_eq!(
        events
            .iter()
            .map(|event| match event {
                DiscoveryEvent::AdmittedBatch(batch) => batch.records().len(),
                DiscoveryEvent::AdmissionAdvanced(_) | DiscoveryEvent::FrontierReady(_) => 0,
            })
            .sum::<usize>(),
        2
    );
    assert!(events.iter().any(|event| matches!(
        event,
        DiscoveryEvent::AdmittedBatch(batch)
            if batch.apply_semantics()
                == BatchApplySemantics::AccumulateUntilTerminalAtomicApply
    )));
}

#[test]
fn manual_cancel_discards_successful_job_owned_chunks() {
    let (executor, started, gate) = fake_executor_with_worker_count(2);
    let handle = executor
        .submit(DiscoveryRequest::ManualBatch {
            locators: vec!["video-manual-success".into(), "block-manual-tail".into()],
            request_revision: DiscoveryRequestRevision::new(43),
        })
        .unwrap();
    assert_eq!(
        started.recv_timeout(Duration::from_secs(2)).unwrap(),
        "video-manual-success"
    );
    assert_eq!(
        started.recv_timeout(Duration::from_secs(2)).unwrap(),
        "block-manual-tail"
    );
    wait_until_processed(&handle, 1);
    assert!(handle.cancel(DiscoveryCancellationCause::UserCancelled));
    assert!(handle.drain_events().is_empty());
    gate.release();
    assert_eq!(
        wait_for_final(&handle).outcome,
        DiscoveryFinalOutcome::Cancelled(DiscoveryCancellationCause::UserCancelled)
    );
    assert!(handle.drain_events().is_empty());
}

#[test]
fn freeze_holds_sibling_batch_until_same_job_resumes_and_ack_gates_ready() {
    let directory = TestDirectory::new();
    let target = directory.path.join("02-target.media");
    let sibling = directory.path.join("03-block-next.media");
    fs::write(&target, b"target").unwrap();
    fs::write(&sibling, b"sibling").unwrap();
    let manifest = Arc::new(build_directory_manifest(&target).unwrap());
    let sibling_key = manifest
        .records()
        .iter()
        .find(|record| record.original_locator() == sibling)
        .unwrap()
        .candidate_key();
    let request = SiblingDiscoveryRequest::new(
        manifest,
        LocalMediaKind::VideoContaining,
        SiblingDiscoveryPolicySnapshot::new(
            true,
            SiblingFilter::SameAsOpened,
            SiblingPolicyRevision::new(5),
        ),
        DiscoveryRequestRevision::new(8),
    );
    let (executor, started, gate) = fake_executor();
    let handle = executor.submit(DiscoveryRequest::Sibling(request)).unwrap();
    assert_eq!(
        started.recv_timeout(Duration::from_secs(2)).unwrap(),
        "03-block-next.media"
    );
    assert!(handle.freeze_admission());
    gate.release();
    wait_until_processed(&handle, 1);
    assert!(handle.take_final_summary().is_none());
    assert!(handle.drain_events().is_empty());
    assert!(handle.resume_admission());
    let _ = wait_for_final(&handle);
    let batch = handle
        .drain_events()
        .into_iter()
        .find_map(|event| match event {
            DiscoveryEvent::AdmittedBatch(batch) => Some(batch),
            DiscoveryEvent::AdmissionAdvanced(_) | DiscoveryEvent::FrontierReady(_) => None,
        })
        .unwrap();
    assert_eq!(
        batch.records()[0].key(),
        DiscoveryRecordKey::Manifest(sibling_key)
    );
    assert!(handle.freeze_admission());
    assert_eq!(
        handle.acknowledge_admitted_batch(batch.batch_id()),
        AdmissionAckOutcome::AdmissionFrozen
    );
    assert!(handle.resume_admission());
    assert_eq!(
        handle.acknowledge_admitted_batch(batch.batch_id()),
        AdmissionAckOutcome::Accepted
    );
    let expected_revision = batch.frontier_revision().unwrap();
    assert!(handle.drain_events().into_iter().any(|event| matches!(
        event,
        DiscoveryEvent::FrontierReady(ready)
            if ready.candidate_key() == sibling_key && ready.revision() == expected_revision
    )));
}

#[test]
fn cancellation_releases_frozen_verified_buffer_without_record_event() {
    let directory = TestDirectory::new();
    let target = directory.path.join("01-target.media");
    fs::write(&target, b"target").unwrap();
    fs::write(directory.path.join("02-block-video.media"), b"sibling").unwrap();
    let request = SiblingDiscoveryRequest::new(
        Arc::new(build_directory_manifest(&target).unwrap()),
        LocalMediaKind::VideoContaining,
        SiblingDiscoveryPolicySnapshot::new(
            true,
            SiblingFilter::AllMedia,
            SiblingPolicyRevision::new(1),
        ),
        DiscoveryRequestRevision::new(1),
    );
    let (executor, started, gate) = fake_executor();
    let handle = executor.submit(DiscoveryRequest::Sibling(request)).unwrap();
    assert_eq!(
        started.recv_timeout(Duration::from_secs(2)).unwrap(),
        "02-block-video.media"
    );
    assert!(handle.freeze_admission());
    gate.release();
    wait_until_processed(&handle, 1);
    assert!(handle.cancel(DiscoveryCancellationCause::StructuralInvalidation));
    assert_eq!(
        wait_for_final(&handle).outcome,
        DiscoveryFinalOutcome::Cancelled(DiscoveryCancellationCause::StructuralInvalidation)
    );
    assert!(handle.drain_events().is_empty());
}

#[test]
fn acknowledging_farther_batch_first_cannot_publish_nearest_readiness() {
    let directory = TestDirectory::new();
    let target = directory.path.join("01-target.media");
    fs::write(&target, b"target").unwrap();
    for index in 2..=35 {
        fs::write(
            directory.path.join(format!("{index:02}-video.media")),
            b"sibling",
        )
        .unwrap();
    }
    let request = SiblingDiscoveryRequest::new(
        Arc::new(build_directory_manifest(&target).unwrap()),
        LocalMediaKind::VideoContaining,
        SiblingDiscoveryPolicySnapshot::new(
            true,
            SiblingFilter::AllMedia,
            SiblingPolicyRevision::new(3),
        ),
        DiscoveryRequestRevision::new(3),
    );
    let (executor, _started, _gate) = fake_executor();
    let handle = executor.submit(DiscoveryRequest::Sibling(request)).unwrap();
    let _ = wait_for_final(&handle);
    let batches = handle
        .drain_events()
        .into_iter()
        .filter_map(|event| match event {
            DiscoveryEvent::AdmittedBatch(batch) => Some(batch),
            DiscoveryEvent::AdmissionAdvanced(_) | DiscoveryEvent::FrontierReady(_) => None,
        })
        .collect::<Vec<_>>();
    let near_batch_id = batches[0].batch_id();
    let far_batch_id = batches.last().unwrap().batch_id();
    assert_eq!(
        handle.acknowledge_admitted_batch(far_batch_id),
        AdmissionAckOutcome::StaleOrAlreadyAcknowledged
    );
    assert!(handle.drain_events().is_empty());
    assert_eq!(
        handle.acknowledge_admitted_batch(near_batch_id),
        AdmissionAckOutcome::Accepted
    );
    assert!(
        handle
            .drain_events()
            .into_iter()
            .any(|event| matches!(event, DiscoveryEvent::FrontierReady(_)))
    );
}

#[test]
fn reprioritize_is_neutral_and_does_not_change_directional_admission() {
    let directory = TestDirectory::new();
    for name in [
        "01-block-before.media",
        "02-target.media",
        "03-block-near-after.media",
        "04-video-far-after.media",
    ] {
        fs::write(directory.path.join(name), b"fixture").unwrap();
    }
    let target = directory.path.join("02-target.media");
    let manifest = Arc::new(build_directory_manifest(&target).unwrap());
    let far_after_key = manifest
        .records()
        .iter()
        .find(|record| {
            record
                .original_locator()
                .ends_with("04-video-far-after.media")
        })
        .unwrap()
        .candidate_key();
    let target_key = manifest.explicit_target().candidate_key();
    let request = SiblingDiscoveryRequest::new(
        manifest,
        LocalMediaKind::VideoContaining,
        SiblingDiscoveryPolicySnapshot::new(
            true,
            SiblingFilter::AllMedia,
            SiblingPolicyRevision::new(2),
        ),
        DiscoveryRequestRevision::new(2),
    );
    let (executor, started, gate) = fake_executor_with_worker_count(2);
    let handle = executor.submit(DiscoveryRequest::Sibling(request)).unwrap();
    let mut initial_starts = vec![
        started.recv_timeout(Duration::from_secs(2)).unwrap(),
        started.recv_timeout(Duration::from_secs(2)).unwrap(),
    ];
    initial_starts.sort();
    assert_eq!(
        initial_starts,
        vec!["01-block-before.media", "03-block-near-after.media"]
    );
    let outcome = handle.reprioritize(ReprioritizeHint::new(
        vec![far_after_key, target_key].into_boxed_slice(),
    ));
    assert_eq!(outcome.reprioritized, 1);
    assert_eq!(outcome.stale, 1);
    gate.release();
    assert_eq!(
        started.recv_timeout(Duration::from_secs(2)).unwrap(),
        "04-video-far-after.media"
    );
    let _ = wait_for_final(&handle);
    let directions = handle
        .drain_events()
        .into_iter()
        .filter_map(|event| match event {
            DiscoveryEvent::AdmittedBatch(batch) => Some(batch.direction()),
            DiscoveryEvent::AdmissionAdvanced(_) | DiscoveryEvent::FrontierReady(_) => None,
        })
        .collect::<Vec<_>>();
    assert!(directions.contains(&AdmissionDirection::Before));
    assert!(directions.contains(&AdmissionDirection::After));
}

#[test]
fn directional_lookahead_waits_at_256_until_near_hole_closes() {
    let directory = TestDirectory::new();
    let target = directory.path.join("000-target.media");
    fs::write(&target, b"target").unwrap();
    fs::write(directory.path.join("001-block-near.media"), b"near").unwrap();
    for ordinal in 2..=301 {
        fs::write(
            directory.path.join(format!("{ordinal:03}-fail-far.media")),
            b"far",
        )
        .unwrap();
    }
    let request = SiblingDiscoveryRequest::new(
        Arc::new(build_directory_manifest(&target).unwrap()),
        LocalMediaKind::VideoContaining,
        SiblingDiscoveryPolicySnapshot::new(
            true,
            SiblingFilter::AllMedia,
            SiblingPolicyRevision::new(33),
        ),
        DiscoveryRequestRevision::new(33),
    );
    let (executor, started, gate) = fake_executor_with_worker_count(2);
    let handle = executor.submit(DiscoveryRequest::Sibling(request)).unwrap();
    let first_window = (0..DIRECTIONAL_LOOKAHEAD_LIMIT)
        .map(|_| started.recv_timeout(Duration::from_secs(2)).unwrap())
        .collect::<Vec<_>>();
    assert!(
        first_window
            .iter()
            .any(|name| name == "001-block-near.media")
    );
    assert!(started.recv_timeout(Duration::from_millis(100)).is_err());
    gate.release();
    assert_eq!(
        started.recv_timeout(Duration::from_secs(2)).unwrap(),
        "257-fail-far.media"
    );
    let summary = wait_for_final(&handle);
    assert_eq!(summary.verified, 1);
    assert_eq!(summary.failed, 300);
}
