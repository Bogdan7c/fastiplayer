//! Планирование wakeup-а, когда установившееся воспроизведение зажато очередью показа.
//!
//! Регрессия cpu-parity сессии 02: очередь показа набрана, следующий video packet
//! ждёт слота, а planner выбирал 2-мс `DecodeReadiness` poll. Освободить слот может
//! только presentation на дедлайне PTS первого кадра, поэтому worker просыпался
//! ~500 раз в секунду впустую. Теперь при доступном decoder activity notifier-е
//! worker спит до дедлайна показа; без notifier-а, при seek и при недоборе очереди
//! остаётся прежний bounded poll.

use super::*;

/// Production default интервала decoder readiness poll-а.
const DECODER_READINESS_POLL_INTERVAL: Duration = Duration::from_millis(2);

/// Резервный coarse interval worker-а (production default — 250 мс).
const COARSE_WAKEUP_INTERVAL: Duration = Duration::from_millis(250);

/// PTS первого кадра очереди: заведомо дальше 2-мс poll-а.
const FRONT_FRAME_PTS: Duration = Duration::from_millis(100);

/// Шаг PTS между кадрами очереди (60 fps).
const FRAME_STEP: Duration = Duration::from_millis(16);

/// Как decoder сообщает о своей activity в сценарии теста.
enum DecoderActivitySupport {
    /// Neutral notifier доступен: decoder сам разбудит worker.
    Available(video_core::VideoDecoderActivitySnapshot),

    /// Backend не поддерживает notifier: нужен bounded poll fallback.
    Unsupported,
}

/// Собирает Playing session с выбранным video track-ом, `queued_frames` кадрами
/// в очереди показа и одним pending packet-ом, который ждёт слота.
fn playing_session_with_present_queue(
    queued_frames: usize,
    activity_support: DecoderActivitySupport,
) -> PlayerSession {
    let decoder_thread = match activity_support {
        DecoderActivitySupport::Available(snapshot) => {
            RecordingVideoDecoderThread::new_with_activity_snapshot(snapshot)
        }
        DecoderActivitySupport::Unsupported => RecordingVideoDecoderThread::new(),
    };
    let mut session = PlayerSession::new();
    session.begin_playback_without_media_for_test().unwrap();
    session.update_current_position(Duration::ZERO);
    session.pipeline.set_video_decoder_thread(decoder_thread);
    session.pipeline.select_video_track(
        TrackId::new(1),
        VideoDecodeRequirement::new(VideoCodec::Vp9),
    );
    for frame_index in 0..queued_frames {
        let pts = FRONT_FRAME_PTS + FRAME_STEP * u32::try_from(frame_index).unwrap();
        session
            .pipeline
            .enqueue_queued_video_frame(decoded_frame(pts, frame_index as u64 + 1));
    }
    enqueue_selected_video_packet(
        &mut session,
        Duration::from_millis(400),
        Bytes::from_static(b"packet-waiting-for-present-slot"),
        false,
    );
    session
}

/// Планирует wakeup с production-интервалами.
fn plan_wakeup(session: &PlayerSession, tick_config: &PlayerTickConfig) -> PlayerWorkerWakeupPlan {
    session.worker_wakeup_plan(
        Instant::now(),
        tick_config,
        DECODER_READINESS_POLL_INTERVAL,
        COARSE_WAKEUP_INTERVAL,
    )
}

#[test]
fn full_present_queue_with_activity_notifier_sleeps_until_frame_deadline() {
    let (_activity_notifier, activity_subscription) =
        video_core::VideoDecoderActivityNotifier::new();
    let tick_config = PlayerTickConfig::default();
    let session = playing_session_with_present_queue(
        tick_config.max_video_present_queue,
        DecoderActivitySupport::Available(activity_subscription.snapshot()),
    );

    let plan = plan_wakeup(&session, &tick_config);

    assert_eq!(plan.reason, WorkerWakeupReason::FramePtsDeadline);
    assert!(
        plan.delay
            .is_some_and(|delay| delay > Duration::from_millis(50)),
        "worker must sleep until the front frame deadline, not poll: {plan:?}"
    );
    assert!(
        plan.wait_for_decoder_activity,
        "real decoder events must still wake the worker before the deadline"
    );
}

#[test]
fn full_present_queue_without_activity_notifier_keeps_bounded_poll_fallback() {
    let tick_config = PlayerTickConfig::default();
    let session = playing_session_with_present_queue(
        tick_config.max_video_present_queue,
        DecoderActivitySupport::Unsupported,
    );

    let plan = plan_wakeup(&session, &tick_config);

    assert_eq!(plan.reason, WorkerWakeupReason::DecodeReadiness);
    assert_eq!(plan.delay, Some(DECODER_READINESS_POLL_INTERVAL));
    assert!(!plan.wait_for_decoder_activity);
}

#[test]
fn underfilled_present_queue_keeps_short_decoder_wakeup_with_activity_notifier() {
    let (_activity_notifier, activity_subscription) =
        video_core::VideoDecoderActivityNotifier::new();
    let tick_config = PlayerTickConfig::default();
    let session = playing_session_with_present_queue(
        1,
        DecoderActivitySupport::Available(activity_subscription.snapshot()),
    );

    let plan = plan_wakeup(&session, &tick_config);

    assert!(
        plan.delay
            .is_some_and(|delay| delay <= DECODER_READINESS_POLL_INTERVAL),
        "underfilled queue must keep the decoder busy without sleeping to PTS: {plan:?}"
    );
    assert_ne!(plan.reason, WorkerWakeupReason::FramePtsDeadline);
}

/// Состояние «очередь набрана, packet ждёт слота» для чистой policy.
fn output_clamped_state() -> DecoderReadinessPollState {
    let target = video_present_queue_target(&PlayerTickConfig::default());
    DecoderReadinessPollState {
        video_track_selected: true,
        video_frame_queue_len: target,
        video_present_queue_target: target,
        worker_has_pending_video_packets: true,
        worker_has_demuxer: true,
        decoder_has_queued_packets: false,
        decoder_has_in_flight_packets: false,
        seek_commit_active: false,
        decoder_activity_wakes_worker: true,
    }
}

#[test]
fn output_clamped_policy_waits_until_presentation_deadline_with_notifier() {
    assert_eq!(
        decoder_readiness_wait_for_state(output_clamped_state()),
        DecoderReadinessWait::UntilPresentationDeadline
    );
}

#[test]
fn output_clamped_policy_without_notifier_keeps_bounded_poll() {
    let state = DecoderReadinessPollState {
        decoder_activity_wakes_worker: false,
        ..output_clamped_state()
    };

    assert_eq!(
        decoder_readiness_wait_for_state(state),
        DecoderReadinessWait::BoundedPoll
    );
}

#[test]
fn output_clamped_policy_during_seek_commit_keeps_bounded_poll() {
    let state = DecoderReadinessPollState {
        seek_commit_active: true,
        ..output_clamped_state()
    };

    assert_eq!(
        decoder_readiness_wait_for_state(state),
        DecoderReadinessWait::BoundedPoll
    );
}

#[test]
fn output_clamped_policy_without_pending_packets_needs_no_decoder_wait() {
    let state = DecoderReadinessPollState {
        worker_has_pending_video_packets: false,
        ..output_clamped_state()
    };

    assert_eq!(
        decoder_readiness_wait_for_state(state),
        DecoderReadinessWait::NotNeeded
    );
}

#[test]
fn underfilled_queue_policy_keeps_bounded_poll_with_notifier() {
    let state = DecoderReadinessPollState {
        video_frame_queue_len: 0,
        ..output_clamped_state()
    };

    assert_eq!(
        decoder_readiness_wait_for_state(state),
        DecoderReadinessWait::BoundedPoll
    );
}
