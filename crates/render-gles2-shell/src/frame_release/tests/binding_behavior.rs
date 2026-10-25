//! Поведение освобождения: кто, когда и сколько раз возвращает кадр декодеру.

use super::super::binding::Gles2FrameReleaseBinding;
use super::super::shared::WaiterLifecycle;
use super::fakes::{EVENT_TIMEOUT, FakeFence, Harness, QUIET_WINDOW};

/// Главный сценарий «спящий UI»: после публикации fence рендер больше НЕ вызывает привязку
/// (следующего кадра UI нет), а кадр всё равно возвращается, как только GPU закончил.
#[test]
fn frame_is_released_by_waiter_after_fence_without_any_further_render_calls() {
    let harness = Harness::new();
    let fence = FakeFence::pending();
    harness.publish(&fence);

    harness.request_release(11);

    assert!(
        fence.wait_until_waited(EVENT_TIMEOUT),
        "поток должен ждать fence"
    );
    assert_eq!(harness.inner_log.released_handles(), Vec::<u64>::new());
    assert_eq!(harness.binding.stats().pending_now, 1);

    fence.signal();

    assert!(harness.inner_log.wait_for_release_count(1, EVENT_TIMEOUT));
    assert_eq!(harness.inner_log.released_handles(), vec![11]);
    let stats = harness.binding.stats();
    assert_eq!(stats.released_by_waiter, 1);
    assert_eq!(stats.pending_now, 0);
}

#[test]
fn already_signaled_fence_releases_synchronously_without_starting_waiter() {
    let harness = Harness::new();
    harness.publish(&FakeFence::already_signaled());

    harness.request_release(5);

    assert_eq!(harness.inner_log.released_handles(), vec![5]);
    assert_eq!(
        harness.binding.stats().released_after_fence_signaled_fast,
        1
    );
    assert_eq!(
        harness.binding.core_for_tests().waiter_lifecycle(),
        WaiterLifecycle::NotStarted,
        "быстрый путь не должен создавать поток"
    );
}

#[test]
fn frame_that_never_met_a_presented_fence_is_released_immediately() {
    let harness = Harness::new();

    harness.request_release(8);

    assert_eq!(harness.inner_log.released_handles(), vec![8]);
    assert_eq!(harness.binding.stats().released_without_fence, 1);
}

/// FIFO: ожидающие кадры уходят в порядке запроса. Более поздний fence, сработавший раньше
/// раннего, ждёт ранний (поведение зафиксировано: для настоящего GPU fence монотонны).
#[test]
fn later_fence_signaled_first_waits_for_earlier_one_and_release_order_is_fifo() {
    let harness = Harness::new();
    let first_fence = FakeFence::pending();
    let second_fence = FakeFence::pending();
    harness.publish(&first_fence);
    harness.request_release(1);
    harness.publish(&second_fence);
    harness.request_release(2);
    assert!(first_fence.wait_until_waited(EVENT_TIMEOUT));

    second_fence.signal();

    assert!(
        !harness.inner_log.wait_for_release_count(1, QUIET_WINDOW),
        "второй кадр обязан ждать первый (FIFO)"
    );
    first_fence.signal();
    assert!(harness.inner_log.wait_for_release_count(2, EVENT_TIMEOUT));
    assert_eq!(harness.inner_log.released_handles(), vec![1, 2]);
    assert_eq!(harness.binding.stats().released_by_waiter, 2);
}

/// Потеря контекста: все ожидающие кадры уходят сразу и ровно по разу, повторный вызов вернёт 0,
/// а поток-ожидатель, дождавшись старого fence, не возвращает их второй раз.
#[test]
fn context_loss_releases_all_waiting_frames_once_and_waiter_does_not_repeat() {
    let harness = Harness::new();
    let old_fence = FakeFence::pending();
    harness.publish(&old_fence);
    harness.request_release(1);
    harness.request_release(2);
    assert!(old_fence.wait_until_waited(EVENT_TIMEOUT));

    let released_now = harness.binding.release_after_context_lost();

    assert_eq!(released_now, 2);
    assert_eq!(harness.inner_log.released_handles(), vec![1, 2]);
    assert_eq!(harness.binding.release_after_context_lost(), 0);
    assert_eq!(harness.binding.stats().released_after_context_loss, 2);
    assert_eq!(harness.binding.stats().pending_now, 0);

    // Запрос в режиме «потерян» уходит сразу, без fence и потока.
    harness.request_release(3);
    assert_eq!(harness.inner_log.released_handles(), vec![1, 2, 3]);

    // После пересоздания рендера: новый кадр ждёт новый fence. Поток-ожидатель сперва должен
    // дожить до старого fence (ждёт запись кадра 1): сигналим его, затем новый. Когда
    // кадр 4 вернулся, старые записи уже обработаны потоком, и повторов в журнале нет.
    harness.binding.resume_after_context_recreated();
    let new_fence = FakeFence::pending();
    harness.publish(&new_fence);
    harness.request_release(4);
    old_fence.signal();
    new_fence.signal();
    assert!(harness.inner_log.wait_for_release_count(4, EVENT_TIMEOUT));
    assert_eq!(harness.inner_log.released_handles(), vec![1, 2, 3, 4]);
}

#[test]
fn fence_published_during_context_loss_is_ignored_until_resume() {
    let harness = Harness::new();
    assert_eq!(harness.binding.release_after_context_lost(), 0);
    harness.publish(&FakeFence::pending());

    harness.request_release(1);

    assert_eq!(
        harness.inner_log.released_handles(),
        vec![1],
        "в режиме потери fence мёртвого контекста не должен задерживать кадр"
    );
}

#[test]
fn fence_wait_error_releases_frame_and_counts_failure() {
    let harness = Harness::new();
    let fence = FakeFence::pending();
    harness.publish(&fence);
    harness.request_release(6);
    assert!(fence.wait_until_waited(EVENT_TIMEOUT));

    fence.fail_with_context_loss();

    assert!(harness.inner_log.wait_for_release_count(1, EVENT_TIMEOUT));
    assert_eq!(harness.inner_log.released_handles(), vec![6]);
    let stats = harness.binding.stats();
    assert_eq!(stats.fence_wait_failures, 1);
    assert_eq!(stats.released_by_waiter, 0);
}

#[test]
fn fence_poll_error_releases_frame_immediately() {
    let harness = Harness::new();
    let fence = FakeFence::pending();
    fence.fail_with_context_loss();
    harness.publish(&fence);

    harness.request_release(9);

    assert_eq!(harness.inner_log.released_handles(), vec![9]);
    assert_eq!(harness.binding.stats().fence_wait_failures, 1);
}

/// Рендер уничтожен (все клоны привязки и обёртки отпущены) при ожидающем кадре: кадр всё
/// равно возвращается после сигнала, декодер доживает до этого, поток завершается.
#[test]
fn dropping_every_binding_clone_still_releases_pending_frame_and_stops_waiter() {
    let harness = Harness::new();
    let fence = FakeFence::pending();
    harness.publish(&fence);
    harness.request_release(12);
    assert!(fence.wait_until_waited(EVENT_TIMEOUT));
    let core = harness.binding.core_for_tests();
    let Harness {
        binding,
        renderer_provider,
        backend,
        inner_log,
        decoder_dropped,
        activity_notifier,
    } = harness;
    drop(backend);
    drop(renderer_provider);
    drop(binding);

    assert_eq!(
        decoder_dropped.try_recv().ok(),
        None,
        "ожидающая запись обязана держать декодер живым"
    );
    assert_eq!(inner_log.released_handles(), Vec::<u64>::new());

    fence.signal();

    assert!(inner_log.wait_for_release_count(1, EVENT_TIMEOUT));
    assert_eq!(inner_log.released_handles(), vec![12]);
    assert!(
        decoder_dropped.recv_timeout(EVENT_TIMEOUT).is_ok(),
        "после освобождения последняя ссылка на декодер отпущена"
    );
    assert!(
        core.wait_until_waiter_exited(EVENT_TIMEOUT),
        "поток обязан завершиться"
    );
    drop(activity_notifier);
}

#[test]
fn dropping_binding_without_waiting_frames_never_starts_waiter() {
    let binding = Gles2FrameReleaseBinding::new();
    let core = binding.core_for_tests();

    drop(binding);

    assert_eq!(core.waiter_lifecycle(), WaiterLifecycle::NotStarted);
}

/// Отравленный мьютекс не должен блокировать возврат кадров декодеру.
#[test]
fn poisoned_state_still_releases_frames() {
    let harness = Harness::new();
    harness.binding.core_for_tests().poison_state_for_tests();

    harness.request_release(4);

    assert_eq!(harness.inner_log.released_handles(), vec![4]);
}

#[test]
fn binding_is_send_and_sync() {
    fn assert_send_sync<T: Send + Sync>() {}
    assert_send_sync::<Gles2FrameReleaseBinding>();
}
