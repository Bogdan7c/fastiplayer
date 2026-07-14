//! Process-lifetime bridge между background owners и winit UI thread.
//!
//! В `AppWakeEvent` намеренно нет result payload: событие только выбирает owner,
//! чей bounded mailbox нужно неблокирующе опустошить. Сам payload сначала
//! публикуется под mutex, и только затем producer поднимает atomic wake edge.

use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};

use winit::event_loop::EventLoopProxy;

/// Владелец mailbox-а, который UI thread должен опустошить.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum AppWakeOwner {
    /// Подготовка startup URL из CLI.
    StartupMedia,
    /// Диалог и подготовка выбранного локального файла.
    LocalFileOpen,
    /// Фоновое обновление dynamic settings options.
    SettingsDynamicOptions,
    /// Process-lifetime playlist runtime и его будущие coordinators.
    PlaylistRuntime,
}

/// Лёгкое typed событие winit: payload остаётся в owner mailbox-е.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) struct AppWakeEvent {
    owner: AppWakeOwner,
}

impl AppWakeEvent {
    /// Возвращает owner, не раскрывая способ хранения его payload-а.
    pub(crate) const fn owner(self) -> AppWakeOwner {
        self.owner
    }
}

/// Результат попытки разбудить UI после успешной публикации payload-а.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum WakeDelivery {
    /// False→true edge отправил новое winit event.
    Armed,
    /// Edge уже поднят, поэтому дополнительное событие не требуется.
    Coalesced,
    /// Event loop закрыт; disconnect sticky и повторных send не будет.
    EventLoopClosed,
}

/// Ошибка terminal slot: второй final result никогда не перезаписывает первый.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum CompletionPublishError {
    /// Final result уже был опубликован, даже если UI успел его забрать.
    AlreadyPublished,
}

/// Результат latest-progress публикации.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) struct ProgressPublishOutcome {
    /// `true`, когда новый progress заменил ещё не забранный старый snapshot.
    pub(crate) replaced_pending_progress: bool,
    /// Состояние wake edge после публикации.
    pub(crate) wake_delivery: WakeDelivery,
}

/// Один неблокирующий UI drain owner mailbox-а.
#[derive(Debug)]
pub(crate) struct OwnerMailboxDrain<Progress, Completion> {
    /// Самый новый progress snapshot, если он был опубликован.
    pub(crate) latest_progress: Option<Progress>,
    /// Lossless terminal result, который можно получить только один раз.
    pub(crate) completion: Option<Completion>,
    /// Последний producer исчез, не опубликовав terminal result.
    pub(crate) producer_disconnected_without_completion: bool,
}

impl<Progress, Completion> OwnerMailboxDrain<Progress, Completion> {
    /// Показывает, изменил ли этот drain видимое owner state.
    pub(crate) fn has_payload(&self) -> bool {
        self.latest_progress.is_some()
            || self.completion.is_some()
            || self.producer_disconnected_without_completion
    }
}

/// Абстракция над единственным production `EventLoopProxy` для deterministic tests.
pub(crate) trait WakeEmitter: Send + Sync {
    /// Отправляет одно лёгкое owner-событие либо сообщает о закрытом loop-е.
    fn emit(&self, event: AppWakeEvent) -> Result<(), ()>;
}

/// Production adapter, который единственный хранит winit proxy.
struct WinitWakeEmitter {
    proxy: EventLoopProxy<AppWakeEvent>,
}

/// Fallback emitter для isolated unit tests и owners без process proxy.
#[cfg(test)]
struct ClosedWakeEmitter;

#[cfg(test)]
impl WakeEmitter for ClosedWakeEmitter {
    fn emit(&self, _event: AppWakeEvent) -> Result<(), ()> {
        Err(())
    }
}

impl WakeEmitter for WinitWakeEmitter {
    fn emit(&self, event: AppWakeEvent) -> Result<(), ()> {
        self.proxy.send_event(event).map_err(|_closed| ())
    }
}

/// Cloneable factory owner-портов поверх одного winit proxy.
#[derive(Clone)]
pub(crate) struct AppWakeProxy {
    emitter: Arc<dyn WakeEmitter>,
}

impl AppWakeProxy {
    /// Забирает единственный proxy, созданный process bootstrap-ом.
    pub(crate) fn new(proxy: EventLoopProxy<AppWakeEvent>) -> Self {
        Self {
            emitter: Arc::new(WinitWakeEmitter { proxy }),
        }
    }

    /// Создаёт sticky owner port; payload через этот API передать невозможно.
    pub(crate) fn port(&self, owner: AppWakeOwner) -> AppWakePort {
        AppWakePort::new(owner, self.emitter.clone())
    }
}

/// Per-owner wake edge, который живёт столько же, сколько producer/receiver ports.
#[derive(Clone)]
pub(crate) struct AppWakePort {
    inner: Arc<AppWakePortInner>,
}

struct AppWakePortInner {
    owner: AppWakeOwner,
    emitter: Arc<dyn WakeEmitter>,
    wake_pending: AtomicBool,
    event_loop_closed: AtomicBool,
}

impl AppWakePort {
    pub(crate) fn new(owner: AppWakeOwner, emitter: Arc<dyn WakeEmitter>) -> Self {
        Self {
            inner: Arc::new(AppWakePortInner {
                owner,
                emitter,
                wake_pending: AtomicBool::new(false),
                event_loop_closed: AtomicBool::new(false),
            }),
        }
    }

    /// Создаёт sticky-closed port для isolated tests; production использует proxy factory.
    #[cfg(test)]
    pub(crate) fn disconnected(owner: AppWakeOwner) -> Self {
        Self::new(owner, Arc::new(ClosedWakeEmitter))
    }

    /// Поднимает только false→true edge; payload должен быть опубликован раньше.
    fn request_wake(&self) -> WakeDelivery {
        if self.inner.event_loop_closed.load(Ordering::Acquire) {
            return WakeDelivery::EventLoopClosed;
        }

        if self.inner.wake_pending.swap(true, Ordering::AcqRel) {
            return WakeDelivery::Coalesced;
        }

        let event = AppWakeEvent {
            owner: self.inner.owner,
        };
        if self.inner.emitter.emit(event).is_ok() {
            WakeDelivery::Armed
        } else {
            // Disconnect sticky: producer не spin-ит и больше не обращается к proxy.
            self.inner.event_loop_closed.store(true, Ordering::Release);
            WakeDelivery::EventLoopClosed
        }
    }

    /// UI очистил текущий edge перед обязательной повторной проверкой mailbox-а.
    fn clear_pending_for_drain(&self) {
        self.inner.wake_pending.store(false, Ordering::Release);
    }

    /// Очищает edge, когда owner mailbox уже уничтожен и payload намеренно abandoned.
    ///
    /// Здесь recheck невозможен по определению: receiver больше не существует. Метод
    /// нужен renderer-bound jobs после suspend, чтобы их stale event не заблокировал
    /// false→true edge следующего AppState/job-а того же process owner kind.
    pub(crate) fn acknowledge_abandoned_mailbox(&self) {
        self.clear_pending_for_drain();
    }
}

/// Внутреннее bounded состояние: один latest progress и один lossless terminal slot.
struct OwnerMailboxState<Progress, Completion> {
    latest_progress: Option<Progress>,
    completion: Option<Completion>,
    completion_was_published: bool,
    active_publishers: usize,
    producer_disconnect_pending: bool,
}

/// Producer port, безопасный для передачи background worker-у.
pub(crate) struct OwnerMailboxPublisher<Progress, Completion> {
    state: Arc<Mutex<OwnerMailboxState<Progress, Completion>>>,
    wake_port: AppWakePort,
}

impl<Progress, Completion> Clone for OwnerMailboxPublisher<Progress, Completion> {
    fn clone(&self) -> Self {
        {
            let mut state = self
                .state
                .lock()
                .expect("owner wake mailbox mutex poisoned during publisher clone");
            state.active_publishers = state
                .active_publishers
                .checked_add(1)
                .expect("owner wake publisher count overflow");
        }
        Self {
            state: self.state.clone(),
            wake_port: self.wake_port.clone(),
        }
    }
}

impl<Progress, Completion> Drop for OwnerMailboxPublisher<Progress, Completion> {
    fn drop(&mut self) {
        let must_report_disconnect = {
            let mut state = self
                .state
                .lock()
                .expect("owner wake mailbox mutex poisoned during publisher drop");
            state.active_publishers = state
                .active_publishers
                .checked_sub(1)
                .expect("owner wake publisher count underflow");
            if state.active_publishers == 0 && !state.completion_was_published {
                state.producer_disconnect_pending = true;
                true
            } else {
                false
            }
        };
        if must_report_disconnect {
            let _delivery = self.wake_port.request_wake();
        }
    }
}

impl<Progress, Completion> OwnerMailboxPublisher<Progress, Completion> {
    /// Coalesce-ит progress в latest slot и после unlock поднимает wake edge.
    pub(crate) fn publish_progress(&self, progress: Progress) -> ProgressPublishOutcome {
        let replaced_pending_progress = {
            let mut state = self
                .state
                .lock()
                .expect("owner wake mailbox mutex poisoned during progress publish");
            state.latest_progress.replace(progress).is_some()
        };

        ProgressPublishOutcome {
            replaced_pending_progress,
            wake_delivery: self.wake_port.request_wake(),
        }
    }

    /// Публикует final result без blocking channel send и без права перезаписи.
    pub(crate) fn publish_completion(
        &self,
        completion: Completion,
    ) -> Result<WakeDelivery, CompletionPublishError> {
        {
            let mut state = self
                .state
                .lock()
                .expect("owner wake mailbox mutex poisoned during completion publish");
            if state.completion_was_published {
                return Err(CompletionPublishError::AlreadyPublished);
            }
            state.completion_was_published = true;
            state.completion = Some(completion);
        }

        Ok(self.wake_port.request_wake())
    }
}

/// UI-owned receiver. Его drain никогда не ждёт worker/channel.
pub(crate) struct OwnerMailboxReceiver<Progress, Completion> {
    state: Arc<Mutex<OwnerMailboxState<Progress, Completion>>>,
    wake_port: AppWakePort,
}

impl<Progress, Completion> OwnerMailboxReceiver<Progress, Completion> {
    /// Забирает current slots, затем выполняет clear→recheck→re-arm protocol.
    pub(crate) fn drain(&self) -> OwnerMailboxDrain<Progress, Completion> {
        self.drain_with_hooks(|| {}, || {})
    }

    #[cfg(test)]
    fn drain_with_after_take_hook(
        &self,
        after_take: impl FnOnce(),
    ) -> OwnerMailboxDrain<Progress, Completion> {
        self.drain_with_hooks(after_take, || {})
    }

    #[cfg(test)]
    fn drain_with_after_clear_hook(
        &self,
        after_clear: impl FnOnce(),
    ) -> OwnerMailboxDrain<Progress, Completion> {
        self.drain_with_hooks(|| {}, after_clear)
    }

    fn drain_with_hooks(
        &self,
        after_take: impl FnOnce(),
        after_clear: impl FnOnce(),
    ) -> OwnerMailboxDrain<Progress, Completion> {
        let drain = {
            let mut state = self
                .state
                .lock()
                .expect("owner wake mailbox mutex poisoned during UI drain");
            OwnerMailboxDrain {
                latest_progress: state.latest_progress.take(),
                completion: state.completion.take(),
                producer_disconnected_without_completion: std::mem::take(
                    &mut state.producer_disconnect_pending,
                ),
            }
        };

        // Test hook моделирует publish ровно между payload take и edge clear.
        after_take();
        self.wake_port.clear_pending_for_drain();
        // Второй test hook моделирует publish после clear, но до mailbox recheck.
        after_clear();

        let payload_arrived_during_drain = {
            let state = self
                .state
                .lock()
                .expect("owner wake mailbox mutex poisoned during drain recheck");
            state.latest_progress.is_some()
                || state.completion.is_some()
                || state.producer_disconnect_pending
        };
        if payload_arrived_during_drain {
            // Даже если producer уже успел re-arm-ить edge, request coalesce-ится.
            let _delivery = self.wake_port.request_wake();
        }

        drain
    }
}

/// Создаёт ровно один latest/completion mailbox для конкретного owner port-а.
pub(crate) fn owner_mailbox<Progress, Completion>(
    wake_port: AppWakePort,
) -> (
    OwnerMailboxPublisher<Progress, Completion>,
    OwnerMailboxReceiver<Progress, Completion>,
) {
    let state = Arc::new(Mutex::new(OwnerMailboxState {
        latest_progress: None,
        completion: None,
        completion_was_published: false,
        active_publishers: 1,
        producer_disconnect_pending: false,
    }));
    (
        OwnerMailboxPublisher {
            state: state.clone(),
            wake_port: wake_port.clone(),
        },
        OwnerMailboxReceiver { state, wake_port },
    )
}

#[cfg(test)]
mod tests {
    use std::sync::atomic::{AtomicUsize, Ordering};

    use super::*;

    struct RecordingEmitter {
        emits: AtomicUsize,
        fail: AtomicBool,
    }

    impl WakeEmitter for RecordingEmitter {
        fn emit(&self, _event: AppWakeEvent) -> Result<(), ()> {
            self.emits.fetch_add(1, Ordering::Relaxed);
            if self.fail.load(Ordering::Relaxed) {
                Err(())
            } else {
                Ok(())
            }
        }
    }

    fn test_port(emitter: Arc<RecordingEmitter>) -> AppWakePort {
        AppWakePort::new(AppWakeOwner::PlaylistRuntime, emitter)
    }

    #[test]
    fn publish_during_clear_window_is_rearmed_without_lost_wakeup() {
        let emitter = Arc::new(RecordingEmitter {
            emits: AtomicUsize::new(0),
            fail: AtomicBool::new(false),
        });
        let (publisher, receiver) = owner_mailbox::<u32, ()>(test_port(emitter.clone()));
        publisher.publish_progress(1_u32);

        let first = receiver.drain_with_after_take_hook(|| {
            publisher.publish_progress(2_u32);
        });
        assert_eq!(first.latest_progress, Some(1));
        assert_eq!(emitter.emits.load(Ordering::Relaxed), 2);

        let second = receiver.drain();
        assert_eq!(second.latest_progress, Some(2));
        assert!(!receiver.drain().has_payload());
    }

    #[test]
    fn publish_after_clear_before_recheck_keeps_armed_wakeup() {
        let emitter = Arc::new(RecordingEmitter {
            emits: AtomicUsize::new(0),
            fail: AtomicBool::new(false),
        });
        let (publisher, receiver) = owner_mailbox::<u32, ()>(test_port(emitter.clone()));
        publisher.publish_progress(1_u32);

        let first = receiver.drain_with_after_clear_hook(|| {
            publisher.publish_progress(2_u32);
        });
        assert_eq!(first.latest_progress, Some(1));
        assert_eq!(emitter.emits.load(Ordering::Relaxed), 2);

        let second = receiver.drain();
        assert_eq!(second.latest_progress, Some(2));
        assert!(!receiver.drain().has_payload());
    }

    #[test]
    fn progress_flood_with_paused_drain_keeps_one_outstanding_wake() {
        let emitter = Arc::new(RecordingEmitter {
            emits: AtomicUsize::new(0),
            fail: AtomicBool::new(false),
        });
        let (publisher, receiver) = owner_mailbox::<u32, ()>(test_port(emitter.clone()));

        for progress in 0_u32..1_000 {
            publisher.publish_progress(progress);
        }

        assert_eq!(emitter.emits.load(Ordering::Relaxed), 1);
        assert_eq!(receiver.drain().latest_progress, Some(999));
    }

    #[test]
    fn progress_and_completion_coexist_and_terminal_is_exactly_once() {
        let emitter = Arc::new(RecordingEmitter {
            emits: AtomicUsize::new(0),
            fail: AtomicBool::new(false),
        });
        let (publisher, receiver) = owner_mailbox(test_port(emitter));

        publisher.publish_progress("готово на 90%");
        assert_eq!(
            publisher.publish_completion("готово"),
            Ok(WakeDelivery::Coalesced)
        );
        assert_eq!(
            publisher.publish_completion("нельзя перезаписать"),
            Err(CompletionPublishError::AlreadyPublished)
        );

        let drain = receiver.drain();
        assert_eq!(drain.latest_progress, Some("готово на 90%"));
        assert_eq!(drain.completion, Some("готово"));
        assert!(receiver.drain().completion.is_none());
    }

    #[test]
    fn concurrent_progress_and_completion_remain_independently_lossless() {
        let emitter = Arc::new(RecordingEmitter {
            emits: AtomicUsize::new(0),
            fail: AtomicBool::new(false),
        });
        let (publisher, receiver) = owner_mailbox(test_port(emitter.clone()));
        let progress_publisher = publisher.clone();

        std::thread::scope(|scope| {
            scope.spawn(move || {
                progress_publisher.publish_progress("последний progress");
            });
            scope.spawn(move || {
                publisher
                    .publish_completion("terminal")
                    .expect("первый terminal должен быть принят");
            });
        });

        let drain = receiver.drain();
        assert_eq!(drain.latest_progress, Some("последний progress"));
        assert_eq!(drain.completion, Some("terminal"));
        assert_eq!(emitter.emits.load(Ordering::Relaxed), 1);
    }

    #[test]
    fn event_loop_closed_is_sticky_and_does_not_retry() {
        let emitter = Arc::new(RecordingEmitter {
            emits: AtomicUsize::new(0),
            fail: AtomicBool::new(true),
        });
        let (publisher, _receiver) = owner_mailbox::<i32, ()>(test_port(emitter.clone()));

        assert_eq!(
            publisher.publish_progress(1).wake_delivery,
            WakeDelivery::EventLoopClosed
        );
        assert_eq!(
            publisher.publish_progress(2).wake_delivery,
            WakeDelivery::EventLoopClosed
        );
        assert_eq!(emitter.emits.load(Ordering::Relaxed), 1);
    }

    #[test]
    fn queued_extra_event_becomes_bounded_noop() {
        let emitter = Arc::new(RecordingEmitter {
            emits: AtomicUsize::new(0),
            fail: AtomicBool::new(false),
        });
        let (publisher, receiver) = owner_mailbox::<(), u32>(test_port(emitter));
        publisher.publish_completion(7_u32).unwrap();

        assert_eq!(receiver.drain().completion, Some(7));
        assert!(!receiver.drain().has_payload());
    }

    #[test]
    fn producer_disconnect_without_terminal_is_woken_and_taken_once() {
        let emitter = Arc::new(RecordingEmitter {
            emits: AtomicUsize::new(0),
            fail: AtomicBool::new(false),
        });
        let (publisher, receiver) = owner_mailbox::<(), ()>(test_port(emitter.clone()));

        drop(publisher);

        assert_eq!(emitter.emits.load(Ordering::Relaxed), 1);
        assert!(receiver.drain().producer_disconnected_without_completion);
        assert!(!receiver.drain().producer_disconnected_without_completion);
    }

    #[test]
    fn abandoned_renderer_mailbox_does_not_block_next_owner_job() {
        let emitter = Arc::new(RecordingEmitter {
            emits: AtomicUsize::new(0),
            fail: AtomicBool::new(false),
        });
        let wake_port = test_port(emitter.clone());
        let (old_publisher, old_receiver) = owner_mailbox::<u32, ()>(wake_port.clone());
        old_publisher.publish_progress(1);
        drop(old_receiver);
        drop(old_publisher);

        wake_port.acknowledge_abandoned_mailbox();
        let (new_publisher, _new_receiver) = owner_mailbox::<u32, ()>(wake_port);
        assert_eq!(
            new_publisher.publish_progress(2).wake_delivery,
            WakeDelivery::Armed
        );
        assert_eq!(emitter.emits.load(Ordering::Relaxed), 2);
    }
}
