//! Запись одного запроса: где он в жизни, что пришло, кого будить.
//!
//! Чистая логика под замком очереди: никаких ожиданий, будильники только
//! складываются в [`WakeList`] и зовутся ядром после снятия замка.
//!
//! Жизнь записи:
//! ```text
//! Queued ──take_requests──▶ Receiving ──итог страницы──▶ Finished ──потребитель ушёл──▶ (нет записи)
//!   │                         │  └──молчание──▶ Expired ──потребитель ушёл──▶ Abandoned{Released}
//!   │                         └──потребитель ушёл──▶ Abandoned{Held} ──итог/молчание──▶ …
//!   └──потребитель ушёл──▶ (нет записи, запрос так и не отправлен)
//! ```
//! Место «в полёте» занимают `Queued`, `Receiving` и `Abandoned{Held}`: брошенный
//! запрос страница всё ещё качает (протокол не умеет отменять), поэтому его место
//! освобождает итог страницы или её молчание, а не уход потребителя.

use std::collections::VecDeque;
use std::task::Waker;
use std::time::{Duration, Instant};

use bytes::Bytes;

use super::error::{ResourceFetchError, ResourceFetchViolation};
use super::request::{ResourceExtent, ResourceFetchSpec};
use super::stats::{ResourceFetchStats, bump};
use super::writer::{ResourceFetchFailure, ResourceFetchOutcome};

/// Будильники, которые ядро позовёт после снятия замка.
#[derive(Default)]
pub(super) struct WakeList {
    /// Кого будить.
    wakers: Vec<Waker>,
}

impl WakeList {
    /// Добавляет будильник (если он был).
    pub(super) fn push(&mut self, waker: Option<Waker>) {
        if let Some(waker) = waker {
            self.wakers.push(waker);
        }
    }

    /// Есть ли кого будить.
    pub(super) const fn is_empty(&self) -> bool {
        self.wakers.is_empty()
    }

    /// Будит всех; звать строго без замка очереди.
    pub(super) fn wake_all(self) {
        for waker in self.wakers {
            waker.wake();
        }
    }
}

/// Держит ли брошенный запрос место в полёте.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) enum SlotHold {
    /// Держит: страница ещё качает.
    Held,
    /// Отпустил: страница замолчала дольше срока.
    Released,
}

/// Чем запрос кончился у внешней стороны.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) enum Ending {
    /// Тело пришло целиком.
    Completed,
    /// Диапазон пришёл короче запрошенного.
    ShortRange {
        /// Длина диапазона.
        requested: u64,
        /// Сколько пришло.
        received: u64,
    },
    /// Внешняя сторона сообщила об отказе.
    Failed(ResourceFetchFailure),
    /// Источник закрыт до итога.
    SourceClosed,
}

/// Где запрос в жизни.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) enum RecordState {
    /// Номер выдан, приложение ещё не забрало запрос.
    Queued,
    /// Отправлен, внешняя сторона шлёт куски.
    Receiving {
        /// Отправка или последний кусок.
        last_activity: Instant,
    },
    /// Итог получен (или источник закрыт); потребитель дочитывает.
    Finished(Ending),
    /// Внешняя сторона молчала дольше срока; потребитель получит ошибку молчания.
    Expired,
    /// Потребитель ушёл; запоздавшие рамки отбрасываются молча.
    Abandoned {
        /// Держит ли место в полёте.
        slot: SlotHold,
        /// Последний признак жизни страницы по этому запросу.
        last_activity: Instant,
    },
}

/// Что получил потребитель при очередном чтении.
#[derive(Debug, PartialEq, Eq)]
pub(super) enum ChunkStep {
    /// Очередной кусок тела.
    Chunk(Bytes),
    /// Тело кончилось целиком.
    End,
    /// Запрос кончился ошибкой (после всех пришедших кусков).
    Failed(ResourceFetchError),
    /// Ждать.
    Pending,
}

/// Что делать с рамкой (куском или итогом), которая к записи не прилипла.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) enum FrameEffect {
    /// Рамка принята.
    Accepted,
    /// Устаревшая рамка, запись остаётся.
    StaleKeep,
    /// Устаревший итог брошенного запроса: запись больше не нужна.
    StaleForget,
}

/// Нужна ли запись дальше (после ухода потребителя или закрытия источника).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) enum RecordRetention {
    /// Запрос не отправлен: убрать запись и из исходящих.
    ForgetUnsent,
    /// Запись больше не нужна.
    Forget,
    /// Запись остаётся ловить запоздавшие рамки.
    Keep,
}

/// Запись одного запроса.
#[derive(Debug)]
pub(super) struct Record {
    /// Что просили.
    spec: ResourceFetchSpec,
    /// Потолок тела.
    limit: u64,
    /// Сколько байтов пришло.
    received: u64,
    /// Пришедшие, но ещё не забранные куски.
    chunks: VecDeque<Bytes>,
    /// Будильник асинхронного потребителя.
    waker: Option<Waker>,
    /// Где запрос в жизни.
    state: RecordState,
}

impl Record {
    /// Запись нового запроса (ещё не отправлен).
    pub(super) fn new(spec: ResourceFetchSpec) -> Self {
        Self {
            limit: spec.body_limit(),
            spec,
            received: 0,
            chunks: VecDeque::new(),
            waker: None,
            state: RecordState::Queued,
        }
    }

    /// Что просили.
    pub(super) const fn spec(&self) -> &ResourceFetchSpec {
        &self.spec
    }

    /// Сколько байтов пришло.
    pub(super) const fn received(&self) -> u64 {
        self.received
    }

    /// Где запрос в жизни (для тестов и сторожа).
    #[cfg(test)]
    pub(super) const fn state(&self) -> RecordState {
        self.state
    }

    /// Занимает ли запрос место в полёте.
    pub(super) const fn occupies_slot(&self) -> bool {
        matches!(
            self.state,
            RecordState::Queued
                | RecordState::Receiving { .. }
                | RecordState::Abandoned {
                    slot: SlotHold::Held,
                    ..
                }
        )
    }

    /// Приложение забрало запрос: с этого мгновения считается молчание.
    ///
    /// Асинхронный потребитель мог зарегистрировать будильник ещё до отправки (движок
    /// опросил тело раньше, чем главный цикл забрал запрос). Тогда у записи не было
    /// срока, и сторож молчания мог уже остановиться: ждать было некого. Будильник
    /// будится здесь — потребитель опросит снова, перерегистрируется уже со сроком и
    /// при нужде запустит свежего сторожа. Без этого молчащая страница вешала бы
    /// такое ожидание до отмены или печати (инвариант 1).
    pub(super) fn mark_sent(&mut self, now: Instant, wake: &mut WakeList) {
        if self.state == RecordState::Queued {
            self.state = RecordState::Receiving { last_activity: now };
            wake.push(self.waker.take());
        }
    }

    /// Запоминает будильник асинхронного потребителя.
    pub(super) fn register_waker(&mut self, waker: &Waker) {
        match &self.waker {
            Some(current) if current.will_wake(waker) => {}
            _ => self.waker = Some(waker.clone()),
        }
    }

    /// Срок молчания для сторожа: только у отправленных запросов, которых ждут
    /// асинхронно (блокирующий потребитель проверяет срок сам).
    pub(super) fn async_deadline(&self, idle_timeout: Duration) -> Option<Instant> {
        match self.state {
            RecordState::Receiving { last_activity } if self.waker.is_some() => {
                last_activity.checked_add(idle_timeout)
            }
            _ => None,
        }
    }

    /// Признаёт запрос мёртвым, если страница молчит дольше срока.
    pub(super) fn expire_if_silent(
        &mut self,
        now: Instant,
        idle_timeout: Duration,
        stats: &mut ResourceFetchStats,
        wake: &mut WakeList,
    ) {
        let silent_since = |last_activity: Instant| {
            now.checked_duration_since(last_activity)
                .is_some_and(|silence| silence >= idle_timeout)
        };
        match self.state {
            RecordState::Receiving { last_activity } if silent_since(last_activity) => {
                self.state = RecordState::Expired;
                bump(&mut stats.failures.page_silent);
                wake.push(self.waker.take());
                tracing::debug!(
                    idle_timeout_ms = idle_timeout.as_millis(),
                    "resource request expired: the page stayed silent"
                );
            }
            RecordState::Abandoned {
                slot: SlotHold::Held,
                last_activity,
            } if silent_since(last_activity) => {
                self.state = RecordState::Abandoned {
                    slot: SlotHold::Released,
                    last_activity,
                };
            }
            _ => {}
        }
    }

    /// Кусок тела от внешней стороны.
    pub(super) fn accept_bytes(
        &mut self,
        id: u32,
        offset: u64,
        payload: &[u8],
        now: Instant,
        stats: &mut ResourceFetchStats,
        wake: &mut WakeList,
    ) -> Result<FrameEffect, ResourceFetchViolation> {
        match &mut self.state {
            RecordState::Queued => Err(ResourceFetchViolation::RequestNotOpen { id }),
            RecordState::Finished(_) => Err(ResourceFetchViolation::AlreadyFinished { id }),
            RecordState::Expired => Ok(FrameEffect::StaleKeep),
            RecordState::Abandoned { last_activity, .. } => {
                // Страница жива и досылает брошенное: место отпустим по её итогу.
                *last_activity = now;
                Ok(FrameEffect::StaleKeep)
            }
            RecordState::Receiving { last_activity } => {
                if offset != self.received {
                    return Err(ResourceFetchViolation::NonContiguousBytes {
                        id,
                        expected: self.received,
                        actual: offset,
                    });
                }
                let length = u64::try_from(payload.len()).unwrap_or(u64::MAX);
                let end = self
                    .received
                    .checked_add(length)
                    .filter(|end| *end <= self.limit)
                    .ok_or(ResourceFetchViolation::BytesBeyondLimit {
                        id,
                        limit: self.limit,
                    })?;
                *last_activity = now;
                if !payload.is_empty() {
                    // Единственная копия тела: из рамки — в `Bytes`, дальше без копий.
                    self.chunks.push_back(Bytes::copy_from_slice(payload));
                    self.received = end;
                    stats.bytes_accepted = stats.bytes_accepted.saturating_add(length);
                    wake.push(self.waker.take());
                }
                Ok(FrameEffect::Accepted)
            }
        }
    }

    /// Итог запроса от внешней стороны.
    pub(super) fn finish(
        &mut self,
        id: u32,
        outcome: ResourceFetchOutcome,
        stats: &mut ResourceFetchStats,
        wake: &mut WakeList,
    ) -> Result<FrameEffect, ResourceFetchViolation> {
        match self.state {
            RecordState::Queued => Err(ResourceFetchViolation::RequestNotOpen { id }),
            RecordState::Finished(_) => Err(ResourceFetchViolation::AlreadyFinished { id }),
            RecordState::Expired => Ok(FrameEffect::StaleKeep),
            RecordState::Abandoned { .. } => Ok(FrameEffect::StaleForget),
            RecordState::Receiving { .. } => {
                let ending = self.ending_of(id, outcome)?;
                match ending {
                    Ending::Completed | Ending::ShortRange { .. } => bump(&mut stats.completed),
                    Ending::Failed(failure) => stats.count_page_failure(failure),
                    Ending::SourceClosed => {}
                }
                if matches!(ending, Ending::ShortRange { .. }) {
                    bump(&mut stats.failures.short_range);
                }
                self.state = RecordState::Finished(ending);
                wake.push(self.waker.take());
                Ok(FrameEffect::Accepted)
            }
        }
    }

    /// Сверяет итог с пришедшим телом.
    fn ending_of(
        &self,
        id: u32,
        outcome: ResourceFetchOutcome,
    ) -> Result<Ending, ResourceFetchViolation> {
        match outcome {
            ResourceFetchOutcome::Failed(failure) => Ok(Ending::Failed(failure)),
            ResourceFetchOutcome::Completed { size_bytes } if size_bytes != self.received => {
                Err(ResourceFetchViolation::SizeMismatch {
                    id,
                    reported: size_bytes,
                    received: self.received,
                })
            }
            ResourceFetchOutcome::Completed { .. } => match self.spec.extent {
                // Короткий диапазон — не нарушение: сервер вправе отдать меньше.
                ResourceExtent::Range(range) if self.received < range.length().get() => {
                    Ok(Ending::ShortRange {
                        requested: range.length().get(),
                        received: self.received,
                    })
                }
                ResourceExtent::Range(_) | ResourceExtent::Whole => Ok(Ending::Completed),
            },
        }
    }

    /// Следующий шаг чтения: сначала все пришедшие куски, потом итог.
    pub(super) fn next_chunk(&mut self, idle_timeout: Duration) -> ChunkStep {
        if let Some(chunk) = self.chunks.pop_front() {
            return ChunkStep::Chunk(chunk);
        }
        match self.state {
            RecordState::Queued | RecordState::Receiving { .. } => ChunkStep::Pending,
            RecordState::Finished(Ending::Completed) => ChunkStep::End,
            RecordState::Finished(Ending::ShortRange {
                requested,
                received,
            }) => ChunkStep::Failed(ResourceFetchError::ShortRange {
                requested,
                received,
            }),
            RecordState::Finished(Ending::Failed(failure)) => {
                ChunkStep::Failed(failure.into_fetch_error())
            }
            RecordState::Finished(Ending::SourceClosed) => {
                ChunkStep::Failed(ResourceFetchError::SourceClosed)
            }
            RecordState::Expired => {
                ChunkStep::Failed(ResourceFetchError::PageSilent { idle_timeout })
            }
            // Потребитель, ушедший из записи, читать не может; честный отказ вместо паники.
            RecordState::Abandoned { .. } => ChunkStep::Failed(ResourceFetchError::SourceClosed),
        }
    }

    /// Потребитель ушёл.
    pub(super) fn abandon(
        &mut self,
        now: Instant,
        stats: &mut ResourceFetchStats,
    ) -> RecordRetention {
        self.chunks.clear();
        self.waker = None;
        match self.state {
            RecordState::Queued => {
                bump(&mut stats.abandoned);
                RecordRetention::ForgetUnsent
            }
            RecordState::Receiving { last_activity } => {
                bump(&mut stats.abandoned);
                self.state = RecordState::Abandoned {
                    slot: SlotHold::Held,
                    last_activity,
                };
                RecordRetention::Keep
            }
            RecordState::Expired => {
                // Место уже отпущено молчанием; запись ловит поздние рамки страницы.
                self.state = RecordState::Abandoned {
                    slot: SlotHold::Released,
                    last_activity: now,
                };
                RecordRetention::Keep
            }
            RecordState::Finished(_) | RecordState::Abandoned { .. } => RecordRetention::Forget,
        }
    }

    /// Источник закрыт: ждущие получат «закрыто» после уже пришедших кусков.
    pub(super) fn seal(&mut self, wake: &mut WakeList) -> RecordRetention {
        match self.state {
            RecordState::Queued | RecordState::Receiving { .. } => {
                self.state = RecordState::Finished(Ending::SourceClosed);
                wake.push(self.waker.take());
                RecordRetention::Keep
            }
            RecordState::Abandoned { .. } => RecordRetention::Forget,
            RecordState::Finished(_) | RecordState::Expired => RecordRetention::Keep,
        }
    }

    /// Очередь мертва (нарушение страницы): разбудить ждущего, куски больше не нужны.
    pub(super) fn wake_on_failure(&mut self, wake: &mut WakeList) {
        self.chunks.clear();
        wake.push(self.waker.take());
    }
}
