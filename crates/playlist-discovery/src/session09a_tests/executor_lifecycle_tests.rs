use super::*;

#[test]
fn every_typed_cancellation_cause_is_first_writer_wins() {
    let causes = [
        DiscoveryCancellationCause::UserCancelled,
        DiscoveryCancellationCause::Superseded,
        DiscoveryCancellationCause::StopAfterCurrent,
        DiscoveryCancellationCause::TransportStop,
        DiscoveryCancellationCause::StructuralInvalidation,
        DiscoveryCancellationCause::LifecycleSuspended,
        DiscoveryCancellationCause::LifecycleShutdown,
    ];
    for cause in causes {
        let cancellation = DiscoveryCancellation::default();
        assert!(cancellation.cancel(cause));
        assert_eq!(cancellation.cause(), Some(cause));
        assert!(!cancellation.cancel(DiscoveryCancellationCause::Superseded));
        assert_eq!(cancellation.cause(), Some(cause));
    }
}

#[test]
fn reserved_foreground_worker_starts_while_all_speculative_workers_block() {
    let (executor, started, gate) = fake_executor();
    let general_workers = executor.worker_count() - FOREGROUND_ONLY_WORKER_COUNT;
    let mut speculative_handles = Vec::new();
    for job_index in 0..ACTIVE_DISCOVERY_JOB_LIMIT - 1 {
        speculative_handles.push(
            executor
                .submit(DiscoveryRequest::VisibleRefresh {
                    locators: (0..PER_JOB_INPUT_LIMIT)
                        .map(|unit_index| {
                            PathBuf::from(format!("block-speculative-{job_index}-{unit_index}"))
                        })
                        .collect(),
                    request_revision: DiscoveryRequestRevision::new(job_index as u64),
                })
                .unwrap(),
        );
    }
    for _ in 0..general_workers {
        assert!(
            started
                .recv_timeout(Duration::from_secs(2))
                .unwrap()
                .starts_with("block-speculative")
        );
    }
    let foreground = executor
        .submit(DiscoveryRequest::ManualBatch {
            locators: vec!["video-foreground".into()],
            request_revision: DiscoveryRequestRevision::new(100),
        })
        .unwrap();
    assert_eq!(
        started.recv_timeout(Duration::from_secs(2)).unwrap(),
        "video-foreground"
    );
    assert_eq!(wait_for_final(&foreground).verified, 1);
    gate.release();
    for handle in speculative_handles {
        let _ = wait_for_final(&handle);
    }
}

#[test]
fn later_foreground_job_waits_for_earlier_blocking_job() {
    let (executor, started, gate) = fake_executor();
    let earlier = executor
        .submit(DiscoveryRequest::ManualBatch {
            locators: vec!["block-earlier".into()],
            request_revision: DiscoveryRequestRevision::new(1),
        })
        .unwrap();
    assert_eq!(
        started.recv_timeout(Duration::from_secs(2)).unwrap(),
        "block-earlier"
    );
    let later = executor
        .submit(DiscoveryRequest::ManualBatch {
            locators: vec!["video-later".into()],
            request_revision: DiscoveryRequestRevision::new(2),
        })
        .unwrap();
    assert!(started.recv_timeout(Duration::from_millis(50)).is_err());
    gate.release();
    assert_eq!(
        started.recv_timeout(Duration::from_secs(2)).unwrap(),
        "video-later"
    );
    let _ = wait_for_final(&earlier);
    let _ = wait_for_final(&later);
}

#[test]
fn foreground_dispatch_rotates_between_jobs_before_same_job_tail() {
    let (executor, started, gate) = fake_executor_with_worker_count(2);
    let first = executor
        .submit(DiscoveryRequest::ManualBatch {
            locators: vec!["block-first-head".into(), "video-first-tail".into()],
            request_revision: DiscoveryRequestRevision::new(31),
        })
        .unwrap();
    assert_eq!(
        started.recv_timeout(Duration::from_secs(2)).unwrap(),
        "block-first-head"
    );
    let second = executor
        .submit(DiscoveryRequest::ManualBatch {
            locators: vec!["video-second-head".into()],
            request_revision: DiscoveryRequestRevision::new(32),
        })
        .unwrap();
    gate.release();
    assert_eq!(
        started.recv_timeout(Duration::from_secs(2)).unwrap(),
        "video-second-head"
    );
    assert_eq!(
        started.recv_timeout(Duration::from_secs(2)).unwrap(),
        "video-first-tail"
    );
    let _ = wait_for_final(&first);
    let _ = wait_for_final(&second);
}

#[test]
fn terminal_slot_survives_result_backpressure_and_progress_coalesces() {
    let wake = wake_port();
    let (executor, started, _gate) = fake_executor_with_wake(wake.clone());
    let handle = executor
        .submit(DiscoveryRequest::MetadataSortPreparation {
            locators: (0..VERIFIED_RECORD_BUFFER_LIMIT)
                .map(|index| PathBuf::from(format!("video-{index}")))
                .collect(),
            request_revision: DiscoveryRequestRevision::new(9),
        })
        .unwrap();
    for _ in 0..VERIFIED_RECORD_BUFFER_LIMIT {
        started.recv_timeout(Duration::from_secs(2)).unwrap();
    }
    assert_eq!(wake.count.load(Ordering::Acquire), 1);
    assert_eq!(
        wait_for_final(&handle).verified,
        VERIFIED_RECORD_BUFFER_LIMIT
    );
    assert!(handle.take_final_summary().is_none());
    assert_eq!(
        handle.take_progress().unwrap().processed,
        VERIFIED_RECORD_BUFFER_LIMIT
    );
    let batch_record_count = handle
        .drain_events()
        .into_iter()
        .map(|event| match event {
            DiscoveryEvent::AdmittedBatch(batch) => batch.records().len(),
            DiscoveryEvent::AdmissionAdvanced(_) | DiscoveryEvent::FrontierReady(_) => 0,
        })
        .sum::<usize>();
    assert_eq!(batch_record_count, VERIFIED_RECORD_BUFFER_LIMIT);
}

#[test]
fn draining_full_record_buffer_resumes_remaining_work_without_polling() {
    let (executor, started, _gate) = fake_executor();
    let total = VERIFIED_RECORD_BUFFER_LIMIT + 88;
    let handle = executor
        .submit(DiscoveryRequest::MetadataSortPreparation {
            locators: (0..total)
                .map(|index| PathBuf::from(format!("video-resume-{index}")))
                .collect(),
            request_revision: DiscoveryRequestRevision::new(12),
        })
        .unwrap();
    for _ in 0..VERIFIED_RECORD_BUFFER_LIMIT {
        started.recv_timeout(Duration::from_secs(2)).unwrap();
    }
    wait_until_processed(&handle, VERIFIED_RECORD_BUFFER_LIMIT);
    assert!(handle.take_final_summary().is_none());
    assert!(!handle.drain_events().is_empty());
    for _ in VERIFIED_RECORD_BUFFER_LIMIT..total {
        started.recv_timeout(Duration::from_secs(2)).unwrap();
    }
    assert_eq!(wait_for_final(&handle).verified, total);
}

#[test]
fn diagnostics_are_capped_and_wake_disconnect_keeps_terminal_ownership() {
    let wake = Arc::new(CountingWake::default());
    wake.disconnected.store(true, Ordering::Release);
    let (executor, _started, _gate) = fake_executor_with_wake(wake);
    let handle = executor
        .submit(DiscoveryRequest::VisibleRefresh {
            locators: (0..DISCOVERY_DIAGNOSTIC_LIMIT + 7)
                .map(|index| PathBuf::from(format!("fail-{index}")))
                .collect(),
            request_revision: DiscoveryRequestRevision::new(11),
        })
        .unwrap();
    let summary = wait_for_final(&handle);
    assert_eq!(summary.diagnostics.len(), DISCOVERY_DIAGNOSTIC_LIMIT);
    assert_eq!(summary.omitted_diagnostics, 7);
    assert!(handle.is_wake_disconnected());
}

#[test]
fn shutdown_cancels_jobs_and_rejects_new_submission() {
    let (executor, started, gate) = fake_executor();
    let handle = executor
        .submit(DiscoveryRequest::VisibleRefresh {
            locators: vec!["block-shutdown".into()],
            request_revision: DiscoveryRequestRevision::new(1),
        })
        .unwrap();
    assert_eq!(
        started.recv_timeout(Duration::from_secs(2)).unwrap(),
        "block-shutdown"
    );
    let report = executor.shutdown();
    assert_eq!(report.cancelled_jobs, 1);
    assert_eq!(report.in_flight_work_units, 1);
    assert!(matches!(
        executor.submit(DiscoveryRequest::ManualBatch {
            locators: vec!["video-rejected".into()],
            request_revision: DiscoveryRequestRevision::new(2),
        }),
        Err(DiscoverySubmitError::ShuttingDown)
    ));
    gate.release();
    assert_eq!(
        wait_for_final(&handle).outcome,
        DiscoveryFinalOutcome::Cancelled(DiscoveryCancellationCause::LifecycleShutdown)
    );
}

#[test]
fn cancelling_blocked_job_removes_queued_work_before_probe_start() {
    let (executor, started, gate) = fake_executor_with_worker_count(2);
    let handle = executor
        .submit(DiscoveryRequest::VisibleRefresh {
            locators: (0..PER_JOB_INPUT_LIMIT)
                .map(|index| PathBuf::from(format!("block-cancel-{index}")))
                .collect(),
            request_revision: DiscoveryRequestRevision::new(21),
        })
        .unwrap();
    assert_eq!(
        started.recv_timeout(Duration::from_secs(2)).unwrap(),
        "block-cancel-0"
    );
    assert!(handle.cancel(DiscoveryCancellationCause::UserCancelled));
    gate.release();
    assert!(started.recv_timeout(Duration::from_millis(100)).is_err());
    assert_eq!(
        wait_for_final(&handle).outcome,
        DiscoveryFinalOutcome::Cancelled(DiscoveryCancellationCause::UserCancelled)
    );
    assert!(handle.drain_events().is_empty());
}

#[test]
fn panicking_probe_completes_exactly_once_as_executor_disconnected() {
    let (executor, _started, _gate) = fake_executor_with_worker_count(2);
    let handle = executor
        .submit(DiscoveryRequest::ManualBatch {
            locators: vec!["panic-probe".into(), "video-never-starts".into()],
            request_revision: DiscoveryRequestRevision::new(22),
        })
        .unwrap();
    assert_eq!(
        wait_for_final(&handle).outcome,
        DiscoveryFinalOutcome::ExecutorDisconnected
    );
    assert!(handle.take_final_summary().is_none());
    assert!(handle.drain_events().is_empty());
}

#[test]
fn request_limit_is_explicit_and_visible_duplicates_keep_distinct_ordinals() {
    let (executor, started, _gate) = fake_executor();
    let duplicate = PathBuf::from("video-visible-once");
    let visible = executor
        .submit(DiscoveryRequest::VisibleRefresh {
            locators: vec![duplicate.clone(), duplicate],
            request_revision: DiscoveryRequestRevision::new(23),
        })
        .unwrap();
    for _ in 0..2 {
        assert_eq!(
            started.recv_timeout(Duration::from_secs(2)).unwrap(),
            "video-visible-once"
        );
    }
    assert_eq!(wait_for_final(&visible).verified, 2);
    let mut records = visible
        .drain_events()
        .into_iter()
        .filter_map(|event| match event {
            DiscoveryEvent::AdmittedBatch(batch) => Some(batch),
            DiscoveryEvent::AdmissionAdvanced(_) | DiscoveryEvent::FrontierReady(_) => None,
        })
        .flat_map(|batch| batch.records().to_vec())
        .collect::<Vec<_>>();
    records.sort_by_key(DiscoveryRecord::key);
    assert_eq!(records[0].key(), DiscoveryRecordKey::Batch(0));
    assert_eq!(records[1].key(), DiscoveryRecordKey::Batch(1));
    assert_eq!(records[0].original_locator(), records[1].original_locator());

    assert!(matches!(
        executor.submit(DiscoveryRequest::ManualBatch {
            locators: (0..=DISCOVERY_REQUEST_ITEM_LIMIT)
                .map(|index| PathBuf::from(format!("video-{index}")))
                .collect(),
            request_revision: DiscoveryRequestRevision::new(24),
        }),
        Err(DiscoverySubmitError::RequestItemLimitReached {
            limit: DISCOVERY_REQUEST_ITEM_LIMIT,
            observed,
        }) if observed == DISCOVERY_REQUEST_ITEM_LIMIT + 1
    ));
}

#[test]
fn wake_edge_is_shared_across_multiple_job_mailboxes() {
    let wake = wake_port();
    let (executor, _started, _gate) = fake_executor_with_wake(wake.clone());
    let first = executor
        .submit(DiscoveryRequest::ManualBatch {
            locators: Vec::new(),
            request_revision: DiscoveryRequestRevision::new(25),
        })
        .unwrap();
    let second = executor
        .submit(DiscoveryRequest::ManualBatch {
            locators: Vec::new(),
            request_revision: DiscoveryRequestRevision::new(26),
        })
        .unwrap();
    assert_eq!(wake.count.load(Ordering::Acquire), 1);
    let _ = first.take_progress();
    assert_eq!(wake.count.load(Ordering::Acquire), 2);
    let _ = second.take_progress();
    let _ = first.take_final_summary();
    let _ = second.take_final_summary();
}
