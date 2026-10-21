//! Пробуждение UI по публикации кадра показа и событий плеера (cpu-parity, сессия 04).

use super::*;
use std::sync::atomic::{AtomicU64, Ordering};

/// Считает вызовы bridge-а; payload через него не идёт, как и в production.
struct CountingPresentationWake {
    wakes: AtomicU64,
}

impl PlayerWorkerPresentationWake for CountingPresentationWake {
    fn wake_presentation(&self) {
        self.wakes.fetch_add(1, Ordering::AcqRel);
    }
}

/// Playing session с fake decoder thread: без активного декодера у session нет identity
/// кадра показа, и bridge не публикует кадр (как в реальном плеере без видео).
fn install_playing_session_with_fake_decoder(runtime: &mut PlayerWorkerRuntime) {
    runtime.session.install_output_clamped_playback_for_tests(
        WorkerActivityDecoderThread::new(VideoDecoderActivitySnapshot::unsupported()),
        Vec::new(),
        Duration::from_millis(400),
    );
}

fn runtime_with_counting_wake() -> (PlayerWorkerRuntime, Arc<CountingPresentationWake>) {
    let mut runtime = runtime_for_tests(Instant::now());
    let wake = Arc::new(CountingPresentationWake {
        wakes: AtomicU64::new(0),
    });
    runtime.config.presentation_wake =
        Some(Arc::clone(&wake) as Arc<dyn PlayerWorkerPresentationWake>);
    (runtime, wake)
}

/// Новый кадр в слоте показа будит UI ровно один раз; повторная публикация того же кадра — нет.
#[test]
fn new_present_frame_wakes_ui_once_and_repeat_publish_stays_silent() {
    let (mut runtime, wake) = runtime_with_counting_wake();
    install_playing_session_with_fake_decoder(&mut runtime);
    // Старт воспроизведения публикует свои события; считаем только дельту от показа кадра.
    runtime.publish_session_outputs();
    let before_frame = wake.wakes.load(Ordering::Acquire);

    runtime
        .session
        .show_frame_on_screen_for_tests(decoded_frame_for_tests(FrameResourceHandle(7)));
    runtime.publish_session_outputs();
    assert_eq!(
        wake.wakes.load(Ordering::Acquire) - before_frame,
        1,
        "the first publication of a new frame must wake the UI exactly once"
    );

    runtime.publish_session_outputs();
    assert_eq!(
        wake.wakes.load(Ordering::Acquire) - before_frame,
        1,
        "re-publishing the same frame must not wake the UI again"
    );
}

/// Событие плеера (здесь — ошибка рендера) будит UI даже без нового кадра.
#[test]
fn lifecycle_event_wakes_ui_without_a_new_frame() {
    let (runtime, wake) = runtime_with_counting_wake();
    let before = wake.wakes.load(Ordering::Acquire);

    runtime.publish_worker_event(PlayerWorkerEvent::RenderError(
        PlayerRenderError::render_device_lost("test"),
    ));

    assert_eq!(wake.wakes.load(Ordering::Acquire) - before, 1);
}

/// Без bridge-а (headless, тесты) публикация остаётся no-op без паники.
#[test]
fn publishing_without_bridge_is_a_silent_noop() {
    let mut runtime = runtime_for_tests(Instant::now());
    assert!(runtime.config.presentation_wake.is_none());

    runtime.publish_session_outputs();
    runtime.publish_worker_event(PlayerWorkerEvent::RenderError(
        PlayerRenderError::render_device_lost("test"),
    ));
}

/// Итог публикации слота показа: новый кадр → `Published`, повтор → `Unchanged`,
/// исчезновение кадра → `Cleared`, пустой слот без кадра → `Unchanged`.
#[test]
fn present_frame_publication_reports_published_unchanged_and_cleared() {
    use crate::render_lease_bridge::PresentFramePublication;

    let mut runtime = runtime_for_tests(Instant::now());
    let mut empty_session = PlayerSession::new();
    assert_eq!(
        runtime
            .render_bridge
            .publish_present_frame(&mut empty_session),
        PresentFramePublication::Unchanged,
        "an empty slot stays unchanged when the session has no frame either"
    );

    install_playing_session_with_fake_decoder(&mut runtime);
    runtime
        .session
        .show_frame_on_screen_for_tests(decoded_frame_for_tests(FrameResourceHandle(3)));
    assert_eq!(
        runtime
            .render_bridge
            .publish_present_frame(&mut runtime.session),
        PresentFramePublication::Published
    );
    assert_eq!(
        runtime
            .render_bridge
            .publish_present_frame(&mut runtime.session),
        PresentFramePublication::Unchanged
    );
    assert_eq!(
        runtime
            .render_bridge
            .publish_present_frame(&mut empty_session),
        PresentFramePublication::Cleared,
        "a session without a frame clears the slot that still held one"
    );
}

/// Сессия 06a: worker выбрал несколько кадров подряд, пока UI их не забирал (главный поток
/// ждал буфер swapchain). Через настоящий путь «сессия → мост → публичный клиент» UI
/// получает их по порядку; кадр сверх ёмкости сбрасывает старший незабранный, и это видно
/// в сводке диагностики. Раньше второй кадр перезаписывал первый молча.
#[test]
fn frames_selected_before_ui_takes_them_reach_render_in_order() {
    let (mut runtime, _command_tx, _shutdown_tx, render_client) =
        runtime_for_tests_with_wakeup_handles(Instant::now());
    install_playing_session_with_fake_decoder(&mut runtime);
    let frame_interval = Duration::from_micros(16_667);
    let select_frame = |runtime: &mut PlayerWorkerRuntime, frame_index: u32| {
        runtime
            .session
            .show_frame_on_screen_for_tests(decoded_frame_with_pts_for_tests(
                frame_interval * frame_index,
                FrameResourceHandle(u64::from(frame_index) + 1),
            ));
        runtime.publish_session_outputs();
    };
    let take_pts = |render_client: &RenderLeaseBridgeClient| {
        render_client
            .try_acquire_present_frame()
            .map(|frame| frame.decoded_frame().pts)
    };

    select_frame(&mut runtime, 0);
    assert_eq!(take_pts(&render_client), Some(Duration::ZERO));
    select_frame(&mut runtime, 1);
    select_frame(&mut runtime, 2);
    assert_eq!(take_pts(&render_client), Some(frame_interval));
    assert_eq!(take_pts(&render_client), Some(frame_interval * 2));
    assert_eq!(
        runtime
            .session
            .diagnostics_log_summary()
            .present_frames_dropped_untaken,
        0,
        "two frames between UI reads fit the handoff without a drop"
    );

    select_frame(&mut runtime, 3);
    select_frame(&mut runtime, 4);
    select_frame(&mut runtime, 5);
    assert_eq!(take_pts(&render_client), Some(frame_interval * 4));
    assert_eq!(take_pts(&render_client), Some(frame_interval * 5));
    assert_eq!(
        runtime
            .session
            .diagnostics_log_summary()
            .present_frames_dropped_untaken,
        1,
        "the third untaken frame drops exactly the oldest one and it is counted"
    );
}
