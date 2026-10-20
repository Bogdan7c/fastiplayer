//! Ожидание decoder-а, упёршегося в полный frame channel (cpu-parity сессия 02).
//!
//! Раньше decoder опрашивал канал каждые 2 мс и на каждом повторе слал событие
//! давления и пульс worker-у. Теперь он спит до жетона «worker забрал кадр» или
//! control message, а событие/пульс отправляет только на входе в давление.

use std::thread;

use super::super::frame_room::{
    PublishBlockedWake, frame_room_channel, wait_for_frame_room_or_control,
};
use super::*;

/// Верхняя граница «сразу» для пробуждений: на порядки меньше любого таймаута
/// и с большим запасом на загруженный CI.
const PROMPT_WAKE_LIMIT: Duration = Duration::from_secs(1);

/// Задержка, с которой фоновый поток имитирует worker/control sender.
const BACKGROUND_EVENT_DELAY: Duration = Duration::from_millis(20);

/// Достаёт publish-pressure событие или паникует с понятной причиной.
fn expect_publish_pressure(
    diagnostic_rx: &std::sync::mpsc::Receiver<VideoDecoderDiagnosticEvent>,
    context: &str,
) -> VideoFramePublishPressureDiagnostics {
    match diagnostic_rx.try_recv() {
        Ok(VideoDecoderDiagnosticEvent::DecodedFramePublishPressure { pressure }) => pressure,
        other => panic!("{context}: expected publish pressure event, got {other:?}"),
    }
}

#[test]
fn full_channel_emits_one_pressure_event_and_pulse_per_episode_not_per_retry() {
    let (frame_tx, frame_rx) = bounded(1);
    let (diagnostic_tx, diagnostic_rx) = std::sync::mpsc::sync_channel(8);
    let (activity_notifier, activity_subscription) = VideoDecoderActivityNotifier::new();
    let mut publish_pressure = FramePublishPressureCounters::default();
    frame_tx
        .try_send(decoded_frame_for_tests(1))
        .expect("test channel has one free slot");
    let mut pending_publish = Some(PendingFramePublish::new(decoded_frame_for_tests(2)));
    let before_episode = activity_subscription.current_epoch();

    // Вход в давление: одно событие и один пульс.
    assert!(publish_pending_frame(
        &frame_tx,
        &mut pending_publish,
        &mut publish_pressure,
        &diagnostic_tx,
        &activity_notifier,
    ));
    let entered_pressure_epoch =
        assert_activity_received_after(&activity_subscription, before_episode);
    let entered = expect_publish_pressure(&diagnostic_rx, "entering pressure");
    assert_eq!(entered.frame_publish_channel_full_count, 1);

    // Повтор в ещё полный канал: счётчики растут, но ни события, ни пульса.
    assert!(publish_pending_frame(
        &frame_tx,
        &mut pending_publish,
        &mut publish_pressure,
        &diagnostic_tx,
        &activity_notifier,
    ));
    assert!(pending_publish.is_some());
    assert_no_activity_after(&activity_subscription, entered_pressure_epoch);
    assert!(
        diagnostic_rx.try_recv().is_err(),
        "a retry into a still-full channel must not emit a pressure event"
    );

    // Worker забрал кадр — успешная публикация несёт накопленные счётчики.
    frame_rx.try_recv().expect("first frame is in the channel");
    assert!(publish_pending_frame(
        &frame_tx,
        &mut pending_publish,
        &mut publish_pressure,
        &diagnostic_tx,
        &activity_notifier,
    ));
    assert!(pending_publish.is_none());
    assert_activity_received_after(&activity_subscription, entered_pressure_epoch);
    let published = expect_publish_pressure(&diagnostic_rx, "successful retry");
    assert_eq!(published.frame_publish_channel_full_count, 2);
    assert_eq!(published.pending_publish_retry_count, 2);
    assert_eq!(
        frame_rx.try_recv().map(|frame| frame.resource_handle),
        Ok(FrameResourceHandle(2))
    );
}

#[test]
fn frame_room_token_wakes_blocked_publish_without_timeout_polling() {
    let (frame_tx, frame_rx) = bounded(1);
    let (diagnostic_tx, diagnostic_rx) = std::sync::mpsc::sync_channel(8);
    let (activity_notifier, _activity_subscription) = VideoDecoderActivityNotifier::new();
    let (_control_tx, control_rx) = bounded::<ThreadControlMsg>(1);
    let (frame_room_notifier, frame_room_waiter) = frame_room_channel();
    let mut publish_pressure = FramePublishPressureCounters::default();
    frame_tx
        .try_send(decoded_frame_for_tests(1))
        .expect("test channel has one free slot");
    let mut pending_publish = Some(PendingFramePublish::new(decoded_frame_for_tests(2)));
    assert!(publish_pending_frame(
        &frame_tx,
        &mut pending_publish,
        &mut publish_pressure,
        &diagnostic_tx,
        &activity_notifier,
    ));
    assert!(
        pending_publish.is_some(),
        "channel is full, frame must stay pending"
    );

    // Worker в другом потоке забирает кадр так же, как `try_recv_frame`.
    let worker = thread::spawn(move || {
        thread::sleep(BACKGROUND_EVENT_DELAY);
        let taken = frame_rx.try_recv().expect("worker takes the queued frame");
        frame_room_notifier.notify_frame_taken();
        (taken.resource_handle, frame_rx, frame_room_notifier)
    });
    let wait_started_at = Instant::now();
    let wake = wait_for_frame_room_or_control(&control_rx, &frame_room_waiter);
    let waited = wait_started_at.elapsed();
    let (taken_handle, frame_rx, _frame_room_notifier) =
        worker.join().expect("worker thread finishes");

    assert!(matches!(wake, PublishBlockedWake::FrameRoom));
    assert_eq!(taken_handle, FrameResourceHandle(1));
    assert!(
        waited < PROMPT_WAKE_LIMIT,
        "decoder must wake on the room token, waited {waited:?}"
    );
    publish_pressure.record_frame_room_wakeup();
    assert!(publish_pending_frame(
        &frame_tx,
        &mut pending_publish,
        &mut publish_pressure,
        &diagnostic_tx,
        &activity_notifier,
    ));
    assert!(pending_publish.is_none());
    assert_eq!(
        frame_rx.try_recv().map(|frame| frame.resource_handle),
        Ok(FrameResourceHandle(2))
    );
    let _entered = expect_publish_pressure(&diagnostic_rx, "entering pressure");
    let published = expect_publish_pressure(&diagnostic_rx, "publish after room");
    assert_eq!(published.frame_room_wakeup_count, 1);
}

#[test]
fn room_token_sent_before_wait_is_not_lost_and_tokens_coalesce() {
    let (control_tx, control_rx) = bounded::<ThreadControlMsg>(1);
    let (frame_room_notifier, frame_room_waiter) = frame_room_channel();

    // Worker забрал кадры между неудачным try_send decoder-а и входом в ожидание.
    frame_room_notifier.notify_frame_taken();
    frame_room_notifier.notify_frame_taken();

    assert!(matches!(
        wait_for_frame_room_or_control(&control_rx, &frame_room_waiter),
        PublishBlockedWake::FrameRoom
    ));
    // Второй жетон слился с первым: следующее ожидание видит уже control message.
    control_tx
        .try_send(ThreadControlMsg::ReleaseZeroCopy(FrameResourceHandle(7)))
        .expect("control channel has capacity");
    assert!(matches!(
        wait_for_frame_room_or_control(&control_rx, &frame_room_waiter),
        PublishBlockedWake::Control(ThreadControlMsg::ReleaseZeroCopy(FrameResourceHandle(7)))
    ));
}

#[test]
fn flush_interrupts_blocked_publish_wait_immediately() {
    let (control_tx, control_rx) = bounded::<ThreadControlMsg>(1);
    let (_frame_room_notifier, frame_room_waiter) = frame_room_channel();
    let (flush_ack_tx, _flush_ack_rx) = bounded(1);

    let control_sender = thread::spawn(move || {
        thread::sleep(BACKGROUND_EVENT_DELAY);
        control_tx
            .try_send(ThreadControlMsg::Flush(flush_ack_tx))
            .expect("control channel has capacity");
        control_tx
    });
    let wait_started_at = Instant::now();
    let wake = wait_for_frame_room_or_control(&control_rx, &frame_room_waiter);
    let waited = wait_started_at.elapsed();
    let _control_tx = control_sender.join().expect("control sender finishes");

    assert!(matches!(
        wake,
        PublishBlockedWake::Control(ThreadControlMsg::Flush(_))
    ));
    assert!(
        waited < PROMPT_WAKE_LIMIT,
        "flush must interrupt the wait at once, waited {waited:?}"
    );
}

#[test]
fn control_message_wins_over_simultaneous_room_token() {
    let (control_tx, control_rx) = bounded::<ThreadControlMsg>(1);
    let (frame_room_notifier, frame_room_waiter) = frame_room_channel();
    let (flush_ack_tx, _flush_ack_rx) = bounded(1);
    frame_room_notifier.notify_frame_taken();
    control_tx
        .try_send(ThreadControlMsg::Flush(flush_ack_tx))
        .expect("control channel has capacity");

    assert!(matches!(
        wait_for_frame_room_or_control(&control_rx, &frame_room_waiter),
        PublishBlockedWake::Control(ThreadControlMsg::Flush(_))
    ));
}

#[test]
fn dropped_frame_consumer_ends_blocked_publish_wait() {
    let (_control_tx, control_rx) = bounded::<ThreadControlMsg>(1);
    let (frame_room_notifier, frame_room_waiter) = frame_room_channel();
    drop(frame_room_notifier);

    assert!(matches!(
        wait_for_frame_room_or_control(&control_rx, &frame_room_waiter),
        PublishBlockedWake::FrameConsumerGone
    ));
}

#[test]
fn closed_control_channel_ends_blocked_publish_wait() {
    let (control_tx, control_rx) = bounded::<ThreadControlMsg>(1);
    let (_frame_room_notifier, frame_room_waiter) = frame_room_channel();
    drop(control_tx);

    assert!(matches!(
        wait_for_frame_room_or_control(&control_rx, &frame_room_waiter),
        PublishBlockedWake::ControlDisconnected
    ));
}

#[test]
fn received_frame_through_handle_wakes_blocked_decoder() {
    let (control_tx, _control_rx) = bounded(4);
    let mut decoder_thread = decoder_thread_for_control_tests(
        control_tx,
        Arc::new(DecoderControlChannelPressureCounters::default()),
        DecoderThreadState::new(),
    );
    let (frame_tx, frame_rx) = bounded(1);
    let (frame_room_notifier, frame_room_waiter) = frame_room_channel();
    decoder_thread.frame_rx = frame_rx;
    decoder_thread.frame_room = frame_room_notifier;
    let (_wait_control_tx, wait_control_rx) = bounded::<ThreadControlMsg>(1);
    frame_tx
        .try_send(decoded_frame_for_tests(3))
        .expect("test channel has one free slot");

    let received = decoder_thread.try_recv_frame();

    assert_eq!(
        received.map(|frame| frame.resource_handle),
        Some(FrameResourceHandle(3))
    );
    assert!(matches!(
        wait_for_frame_room_or_control(&wait_control_rx, &frame_room_waiter),
        PublishBlockedWake::FrameRoom
    ));
}
