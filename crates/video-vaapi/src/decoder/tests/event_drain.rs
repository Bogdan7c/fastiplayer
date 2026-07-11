use super::*;

/// Проверяет, что `CheckEvents` не теряет стартовый packet после `FormatChanged`.
#[test]
fn check_events_format_change_retries_same_packet_and_preserves_pts() {
    let packet_data = vec![0x82, 0x49, 0x83, 0x42];
    let mut driver = FakeRetryDriver::new(
        vec![
            Err(VaapiAdapterDecodeError::CheckEvents),
            Ok(packet_data.len()),
        ],
        vec![
            vec![FakeDecoderEvent::FormatChanged],
            vec![FakeDecoderEvent::FrameReady {
                pts: Duration::ZERO,
            }],
        ],
    );

    let report = run_decode_with_event_retry(&mut driver, 0, &packet_data, true, 11).unwrap();

    assert_eq!(report.attempts, 2);
    assert_eq!(report.events_count, 2);
    assert!(report.format_changed);
    assert!(!report.skipped_packet);
    assert_eq!(
        driver.submissions,
        vec![(0, packet_data.clone()), (0, packet_data)]
    );
    assert_eq!(
        driver.ready_frames.pop_front(),
        Some(FakeReadyFrame {
            pts: Duration::ZERO,
            generation: 11,
        })
    );
}

/// Проверяет, что `NotEnoughOutputBuffers` не маскируется под принятый packet.
#[test]
fn not_enough_output_buffers_marks_packet_as_backpressured() {
    let packet_data = vec![0x82, 0x49, 0x83, 0x42];
    let mut driver = FakeRetryDriver::new(
        vec![Err(VaapiAdapterDecodeError::NotEnoughOutputBuffers(1))],
        vec![Vec::new()],
    );

    let report = run_decode_with_event_retry(&mut driver, 0, &packet_data, true, 17).unwrap();

    assert_eq!(report.attempts, 1);
    assert!(report.output_backpressured);
    assert!(!report.skipped_packet);
    assert_eq!(driver.submissions, vec![(0, packet_data)]);
}

/// Проверяет seek flush policy: H.264 tail event не попадает в ready queue.
#[test]
fn seek_flush_discard_policy_drops_tail_frame_events() {
    let tail_pts = Duration::from_millis(33);
    let mut driver = FakeRetryDriver::new(
        Vec::new(),
        vec![vec![FakeDecoderEvent::FrameReady { pts: tail_pts }]],
    );

    let report = driver
        .drain_events(DecoderEventDrainPolicy::Discard { reason: "flush" })
        .unwrap();

    assert_eq!(report.events_count, 1);
    assert!(!report.format_changed);
    assert!(driver.ready_frames.is_empty());
    assert_eq!(driver.discarded_pts, vec![tail_pts]);
}

/// Проверяет EOF drain policy: tail frame публикуется с generation запроса.
#[test]
fn eof_drain_publish_policy_keeps_tail_frame_generation() {
    let tail_pts = Duration::from_millis(67);
    let mut driver = FakeRetryDriver::new(
        Vec::new(),
        vec![vec![FakeDecoderEvent::FrameReady { pts: tail_pts }]],
    );

    let report = driver
        .drain_events(DecoderEventDrainPolicy::Publish { generation: 23 })
        .unwrap();

    assert_eq!(report.events_count, 1);
    assert_eq!(
        driver.ready_frames.pop_front(),
        Some(FakeReadyFrame {
            pts: tail_pts,
            generation: 23,
        })
    );
    assert!(driver.discarded_pts.is_empty());
}

/// Проверяет burst из нескольких `FrameReady`: весь drain получает одну generation.
#[test]
fn burst_frames_drained_in_one_generation_keep_that_generation() {
    let mut driver = FakeRetryDriver::new(
        vec![Ok(4)],
        vec![vec![
            FakeDecoderEvent::FrameReady {
                pts: Duration::from_millis(10),
            },
            FakeDecoderEvent::FrameReady {
                pts: Duration::from_millis(20),
            },
            FakeDecoderEvent::FrameReady {
                pts: Duration::from_millis(30),
            },
        ]],
    );

    let report = run_decode_with_event_retry(&mut driver, 0, &[1, 2, 3, 4], true, 31).unwrap();

    assert_eq!(report.events_count, 3);
    assert_eq!(
        driver.ready_frames.into_iter().collect::<Vec<_>>(),
        vec![
            FakeReadyFrame {
                pts: Duration::from_millis(10),
                generation: 31,
            },
            FakeReadyFrame {
                pts: Duration::from_millis(20),
                generation: 31,
            },
            FakeReadyFrame {
                pts: Duration::from_millis(30),
                generation: 31,
            },
        ]
    );
}

/// Проверяет reconfigure/flush scope: release берёт только decoder-owned ready frames.
#[test]
fn reconfigure_release_scope_drains_only_decoder_owned_ready_frames() {
    let renderer_owned_handle = FrameResourceHandle(99);
    let mut ready_queue = VecDeque::from([
        decoded_frame_for_tests(FrameResourceHandle(1)),
        decoded_frame_for_tests(FrameResourceHandle(2)),
    ]);

    let decoder_owned_handles = drain_decoder_owned_ready_frame_handles(&mut ready_queue);

    assert_eq!(
        decoder_owned_handles,
        vec![FrameResourceHandle(1), FrameResourceHandle(2)]
    );
    assert!(!decoder_owned_handles.contains(&renderer_owned_handle));
    assert!(ready_queue.is_empty());
}
