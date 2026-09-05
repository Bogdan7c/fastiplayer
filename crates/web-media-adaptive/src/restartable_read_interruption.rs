//! Lock-free сигнал физическому body read-у уже committed adaptive resource-а.
//!
//! Request-scoped seek cancellation продолжает владеть offside replacement-ом.
//! Этот controller активируется только после transactional commit и прерывает
//! исключительно current committed body, не публикуя seek receipt самостоятельно.

use std::fmt;
use std::future::Future;
use std::pin::Pin;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::task::{Context, Poll};

use atomic_waker::AtomicWaker;

/// Число младших битов, зарезервированных для current attempt phase.
const ATTEMPT_PHASE_BITS: u32 = 2;
/// Маска current attempt phase внутри одного atomic state word.
const ATTEMPT_PHASE_MASK: u64 = (1_u64 << ATTEMPT_PHASE_BITS) - 1;
/// Current attempt существует, но network body read сейчас не pending.
const ATTEMPT_PHASE_QUIESCENT: u64 = 0;
/// Current attempt держит ровно один pending network body future.
const ATTEMPT_PHASE_READING: u64 = 1;
/// Active interruption уже принято и должно terminalize current resource read.
const ATTEMPT_PHASE_INTERRUPTED: u64 = 2;
/// Максимальная identity, которая помещается рядом с phase без wrap/collision.
const MAXIMUM_ATTEMPT_IDENTITY: u64 = u64::MAX >> ATTEMPT_PHASE_BITS;

/// Stable lock-free controller одного adaptive component lineage.
#[derive(Clone)]
pub struct AdaptiveRestartableReadInterruption {
    /// Shared state не содержит URL, headers, cookies либо другого request material.
    shared: Arc<AdaptiveRestartableReadSharedState>,
}

/// Один resource attempt, который остаётся disarmed до authoritative commit-а.
#[derive(Clone)]
pub struct AdaptiveRestartableReadAttempt {
    /// Clone-ы transport-а и HLS proof owner-а разделяют exact attempt identity.
    shared: Arc<AdaptiveRestartableReadAttemptState>,
}

/// Результат nonblocking active-read signal-а.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum AdaptiveRestartableReadInterruptionRequest {
    /// Сигнал атомарно принят для current pending body read-а.
    InterruptionRequested,
    /// Тот же current read уже получил signal, но ещё может unwind-иться на worker-е.
    InterruptionAlreadyRequested,
    /// Pending network body read отсутствует; future либо cache replay не отравляются.
    AlreadyQuiescent,
}

/// Результат однократной активации proven resource attempt-а.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum AdaptiveRestartableReadArmOutcome {
    /// Attempt впервые стал current committed transport owner-ом.
    Armed,
    /// Повторный вызов относится к тому же current attempt-у и ничего не меняет.
    AlreadyCurrent,
    /// Attempt раньше был current, но уже вытеснен более новым commit-ом.
    StaleAttemptRejected,
}

/// Ошибка выделения новой bounded attempt identity.
#[derive(Debug, Clone, Copy, PartialEq, Eq, thiserror::Error)]
pub enum AdaptiveRestartableReadAttemptError {
    /// 62-bit identity space исчерпано; collision запрещён fail-closed.
    #[error("adaptive restartable read attempt identity space exhausted")]
    IdentitySpaceExhausted,
}

/// Общий state word и ровно один waker current committed read-а.
struct AdaptiveRestartableReadSharedState {
    /// Монотонная attempt identity никогда не переиспользуется внутри lineage.
    next_attempt_identity: AtomicU64,
    /// Atomic `(attempt identity, phase)` исключает torn re-arm/signal decisions.
    current_attempt_state: AtomicU64,
    /// Current-thread executor одновременно ждёт не больше одного active body future.
    current_read_waker: AtomicWaker,
}

/// Локальное состояние одного attempt-а отделяет never-armed replacement от stale owner-а.
struct AdaptiveRestartableReadAttemptState {
    /// Stable lineage controller переживает transactional demux replacements.
    controller: Arc<AdaptiveRestartableReadSharedState>,
    /// Exact non-zero identity этого resource attempt-а.
    identity: u64,
    /// Attempt разрешено arm-ить только один раз, чтобы stale owner не украл current slot.
    was_armed: AtomicBool,
}

/// Internal старт network read-а сохраняет disarmed/offside и stale состояния раздельно.
pub(crate) enum AdaptiveRestartableReadStart {
    /// Offside proof ещё не committed и слушает только request-scoped cancellation.
    Disarmed,
    /// Exact current attempt занял единственный active-read slot.
    Armed(AdaptiveRestartableReadGuard),
    /// Ранее committed attempt уже interrupted либо вытеснен новым commit-ом.
    InterruptedOrSuperseded,
}

/// RAII guard quiesce-ит exact current attempt после обычного completion/error.
pub(crate) struct AdaptiveRestartableReadGuard {
    /// Attempt identity нужна для generation-safe finish и future poll-а.
    attempt: AdaptiveRestartableReadAttempt,
}

/// Outcome completion race-а между body future и restartable signal-ом.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum AdaptiveRestartableReadCompletion {
    /// Body operation завершилась до accepted interruption.
    Completed,
    /// Accepted interruption либо newer committed attempt выиграли race.
    InterruptedOrSuperseded,
}

/// Future current attempt-а; `AtomicWaker` хранит ровно один bounded waiter.
pub(crate) struct AdaptiveRestartableReadInterrupted<'a> {
    /// Borrow запрещает пережить guard и случайно зарегистрировать stale waiter повторно.
    guard: &'a AdaptiveRestartableReadGuard,
}

impl AdaptiveRestartableReadInterruption {
    /// Создаёт stable controller без active resource и без background work.
    #[must_use]
    pub fn new() -> Self {
        Self {
            shared: Arc::new(AdaptiveRestartableReadSharedState {
                next_attempt_identity: AtomicU64::new(0),
                current_attempt_state: AtomicU64::new(0),
                current_read_waker: AtomicWaker::new(),
            }),
        }
    }

    /// Выделяет disarmed attempt; HLS arm-ит его только после proof и commit-а.
    pub fn new_attempt(
        &self,
    ) -> Result<AdaptiveRestartableReadAttempt, AdaptiveRestartableReadAttemptError> {
        let previous_identity = self
            .shared
            .next_attempt_identity
            .fetch_update(Ordering::AcqRel, Ordering::Acquire, |current| {
                (current < MAXIMUM_ATTEMPT_IDENTITY).then_some(current.saturating_add(1))
            })
            .map_err(|_| AdaptiveRestartableReadAttemptError::IdentitySpaceExhausted)?;
        let identity = previous_identity.saturating_add(1);
        Ok(AdaptiveRestartableReadAttempt {
            shared: Arc::new(AdaptiveRestartableReadAttemptState {
                controller: Arc::clone(&self.shared),
                identity,
                was_armed: AtomicBool::new(false),
            }),
        })
    }

    /// Сигналит только реально pending read-у current committed attempt-а.
    ///
    /// Метод lock-free, не выполняет I/O и не ждёт worker. `wake()` уведомляет
    /// единственный registered current-read future без polling thread-а.
    #[must_use]
    pub fn request_active_read_interruption(&self) -> AdaptiveRestartableReadInterruptionRequest {
        loop {
            let current = self.shared.current_attempt_state.load(Ordering::Acquire);
            match attempt_phase(current) {
                ATTEMPT_PHASE_QUIESCENT => {
                    return AdaptiveRestartableReadInterruptionRequest::AlreadyQuiescent;
                }
                ATTEMPT_PHASE_INTERRUPTED => {
                    return AdaptiveRestartableReadInterruptionRequest::InterruptionAlreadyRequested;
                }
                ATTEMPT_PHASE_READING => {
                    let interrupted =
                        encode_attempt_state(attempt_identity(current), ATTEMPT_PHASE_INTERRUPTED);
                    if self
                        .shared
                        .current_attempt_state
                        .compare_exchange(current, interrupted, Ordering::AcqRel, Ordering::Acquire)
                        .is_ok()
                    {
                        self.shared.current_read_waker.wake();
                        return AdaptiveRestartableReadInterruptionRequest::InterruptionRequested;
                    }
                }
                _ => unreachable!("phase mask допускает только три owner-defined состояния"),
            }
        }
    }
}

impl Default for AdaptiveRestartableReadInterruption {
    /// Default не arm-ит resource и эквивалентен явному constructor-у.
    fn default() -> Self {
        Self::new()
    }
}

impl fmt::Debug for AdaptiveRestartableReadInterruption {
    /// Diagnostics не раскрывает runtime state либо request material.
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("AdaptiveRestartableReadInterruption")
            .finish_non_exhaustive()
    }
}

impl AdaptiveRestartableReadAttempt {
    /// Делает proven attempt current ровно один раз после transactional commit-а.
    #[must_use]
    pub fn arm_as_current(&self) -> AdaptiveRestartableReadArmOutcome {
        if self
            .shared
            .was_armed
            .compare_exchange(false, true, Ordering::AcqRel, Ordering::Acquire)
            .is_err()
        {
            let current = self
                .shared
                .controller
                .current_attempt_state
                .load(Ordering::Acquire);
            return if attempt_identity(current) == self.shared.identity {
                AdaptiveRestartableReadArmOutcome::AlreadyCurrent
            } else {
                AdaptiveRestartableReadArmOutcome::StaleAttemptRejected
            };
        }

        self.shared.controller.current_attempt_state.store(
            encode_attempt_state(self.shared.identity, ATTEMPT_PHASE_QUIESCENT),
            Ordering::Release,
        );
        self.shared.controller.current_read_waker.wake();
        AdaptiveRestartableReadArmOutcome::Armed
    }

    /// Начинает exact current network read либо сохраняет offside proof disarmed.
    pub(crate) fn begin_network_read(&self) -> AdaptiveRestartableReadStart {
        loop {
            let current = self
                .shared
                .controller
                .current_attempt_state
                .load(Ordering::Acquire);
            if attempt_identity(current) != self.shared.identity {
                return if self.shared.was_armed.load(Ordering::Acquire) {
                    AdaptiveRestartableReadStart::InterruptedOrSuperseded
                } else {
                    AdaptiveRestartableReadStart::Disarmed
                };
            }
            match attempt_phase(current) {
                ATTEMPT_PHASE_QUIESCENT => {
                    let reading = encode_attempt_state(self.shared.identity, ATTEMPT_PHASE_READING);
                    if self
                        .shared
                        .controller
                        .current_attempt_state
                        .compare_exchange(current, reading, Ordering::AcqRel, Ordering::Acquire)
                        .is_ok()
                    {
                        return AdaptiveRestartableReadStart::Armed(AdaptiveRestartableReadGuard {
                            attempt: self.clone(),
                        });
                    }
                }
                ATTEMPT_PHASE_READING | ATTEMPT_PHASE_INTERRUPTED => {
                    return AdaptiveRestartableReadStart::InterruptedOrSuperseded;
                }
                _ => unreachable!("phase mask допускает только три owner-defined состояния"),
            }
        }
    }
}

impl fmt::Debug for AdaptiveRestartableReadAttempt {
    /// Attempt identity внутренний и не нужен generic caller diagnostics.
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("AdaptiveRestartableReadAttempt")
            .finish_non_exhaustive()
    }
}

impl AdaptiveRestartableReadGuard {
    /// Возвращает future exact attempt epoch-а без timer либо polling.
    pub(crate) fn interrupted(&self) -> AdaptiveRestartableReadInterrupted<'_> {
        AdaptiveRestartableReadInterrupted { guard: self }
    }

    /// Завершает ordinary body operation и обнаруживает concurrent accepted signal.
    pub(crate) fn finish(self) -> AdaptiveRestartableReadCompletion {
        let reading = encode_attempt_state(self.attempt.shared.identity, ATTEMPT_PHASE_READING);
        let quiescent = encode_attempt_state(self.attempt.shared.identity, ATTEMPT_PHASE_QUIESCENT);
        if self
            .attempt
            .shared
            .controller
            .current_attempt_state
            .compare_exchange(reading, quiescent, Ordering::AcqRel, Ordering::Acquire)
            .is_ok()
        {
            AdaptiveRestartableReadCompletion::Completed
        } else {
            AdaptiveRestartableReadCompletion::InterruptedOrSuperseded
        }
    }
}

impl Drop for AdaptiveRestartableReadGuard {
    /// Source/request cancellation и ordinary error возвращают current attempt в quiescent state.
    fn drop(&mut self) {
        let reading = encode_attempt_state(self.attempt.shared.identity, ATTEMPT_PHASE_READING);
        let quiescent = encode_attempt_state(self.attempt.shared.identity, ATTEMPT_PHASE_QUIESCENT);
        let _ = self
            .attempt
            .shared
            .controller
            .current_attempt_state
            .compare_exchange(reading, quiescent, Ordering::AcqRel, Ordering::Acquire);
    }
}

impl Future for AdaptiveRestartableReadInterrupted<'_> {
    type Output = ();

    /// Register-before-recheck следует `AtomicWaker` contract и исключает lost wake.
    fn poll(self: Pin<&mut Self>, context: &mut Context<'_>) -> Poll<Self::Output> {
        let reading =
            encode_attempt_state(self.guard.attempt.shared.identity, ATTEMPT_PHASE_READING);
        let controller = &self.guard.attempt.shared.controller;
        if controller.current_attempt_state.load(Ordering::Acquire) != reading {
            return Poll::Ready(());
        }
        controller.current_read_waker.register(context.waker());
        if controller.current_attempt_state.load(Ordering::Acquire) != reading {
            Poll::Ready(())
        } else {
            Poll::Pending
        }
    }
}

/// Кодирует non-zero attempt identity рядом с bounded phase.
const fn encode_attempt_state(identity: u64, phase: u64) -> u64 {
    (identity << ATTEMPT_PHASE_BITS) | phase
}

/// Извлекает attempt identity из atomic state word.
const fn attempt_identity(state: u64) -> u64 {
    state >> ATTEMPT_PHASE_BITS
}

/// Извлекает bounded phase из atomic state word.
const fn attempt_phase(state: u64) -> u64 {
    state & ATTEMPT_PHASE_MASK
}

#[cfg(test)]
mod tests {
    use std::sync::atomic::AtomicUsize;
    use std::task::{Wake, Waker};

    use super::*;

    /// Один deterministic counter проверяет register/wake без executor thread-а.
    struct WakeCounter {
        wake_count: AtomicUsize,
    }

    impl Wake for WakeCounter {
        fn wake(self: Arc<Self>) {
            self.wake_count.fetch_add(1, Ordering::SeqCst);
        }
    }

    #[test]
    fn disarmed_attempt_is_separate_from_current_and_stale_attempt_cannot_rearm() {
        let controller = AdaptiveRestartableReadInterruption::new();
        let first = controller.new_attempt().expect("first attempt");
        let replacement = controller.new_attempt().expect("replacement attempt");

        assert!(matches!(
            replacement.begin_network_read(),
            AdaptiveRestartableReadStart::Disarmed
        ));
        assert_eq!(
            first.arm_as_current(),
            AdaptiveRestartableReadArmOutcome::Armed
        );
        assert_eq!(
            replacement.arm_as_current(),
            AdaptiveRestartableReadArmOutcome::Armed
        );
        assert_eq!(
            first.arm_as_current(),
            AdaptiveRestartableReadArmOutcome::StaleAttemptRejected
        );
        assert!(matches!(
            first.begin_network_read(),
            AdaptiveRestartableReadStart::InterruptedOrSuperseded
        ));
    }

    #[test]
    fn current_read_signal_is_one_shot_and_next_commit_rearms_controller() {
        let controller = AdaptiveRestartableReadInterruption::new();
        let first = controller.new_attempt().expect("first attempt");
        assert_eq!(
            controller.request_active_read_interruption(),
            AdaptiveRestartableReadInterruptionRequest::AlreadyQuiescent
        );
        assert_eq!(
            first.arm_as_current(),
            AdaptiveRestartableReadArmOutcome::Armed
        );
        let first_guard = match first.begin_network_read() {
            AdaptiveRestartableReadStart::Armed(guard) => guard,
            _ => panic!("current attempt должен занять read slot"),
        };
        assert_eq!(
            controller.request_active_read_interruption(),
            AdaptiveRestartableReadInterruptionRequest::InterruptionRequested
        );
        assert_eq!(
            controller.request_active_read_interruption(),
            AdaptiveRestartableReadInterruptionRequest::InterruptionAlreadyRequested
        );
        assert_eq!(
            first_guard.finish(),
            AdaptiveRestartableReadCompletion::InterruptedOrSuperseded
        );

        let replacement = controller.new_attempt().expect("replacement attempt");
        assert_eq!(
            replacement.arm_as_current(),
            AdaptiveRestartableReadArmOutcome::Armed
        );
        let replacement_guard = match replacement.begin_network_read() {
            AdaptiveRestartableReadStart::Armed(guard) => guard,
            _ => panic!("replacement commit должен re-arm controller"),
        };
        assert_eq!(
            replacement_guard.finish(),
            AdaptiveRestartableReadCompletion::Completed
        );
    }

    #[test]
    fn registered_current_read_future_is_woken_once_without_lost_signal() {
        let controller = AdaptiveRestartableReadInterruption::new();
        let attempt = controller.new_attempt().expect("attempt");
        assert_eq!(
            attempt.arm_as_current(),
            AdaptiveRestartableReadArmOutcome::Armed
        );
        let guard = match attempt.begin_network_read() {
            AdaptiveRestartableReadStart::Armed(guard) => guard,
            _ => panic!("current attempt должен занять read slot"),
        };
        let wake_counter = Arc::new(WakeCounter {
            wake_count: AtomicUsize::new(0),
        });
        let waker = Waker::from(Arc::clone(&wake_counter));
        let mut context = Context::from_waker(&waker);
        let mut interrupted = Box::pin(guard.interrupted());
        assert!(interrupted.as_mut().poll(&mut context).is_pending());

        assert_eq!(
            controller.request_active_read_interruption(),
            AdaptiveRestartableReadInterruptionRequest::InterruptionRequested
        );
        assert_eq!(wake_counter.wake_count.load(Ordering::SeqCst), 1);
        assert!(interrupted.as_mut().poll(&mut context).is_ready());
        drop(interrupted);
        assert_eq!(
            guard.finish(),
            AdaptiveRestartableReadCompletion::InterruptedOrSuperseded
        );
    }
}

#[cfg(test)]
#[path = "tests/restartable_read_observation.rs"]
mod test_observation;
