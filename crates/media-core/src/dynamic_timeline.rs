//! Нейтральный revisioned boundary между live source и потребителем timeline.
//!
//! Producer публикует только уже доказанное live edge/DVR-окно, а consumer читает
//! последний immutable snapshot. Канал ёмкости один переносит лишь activity edge:
//! payload остаётся под mutex, поэтому burst обновлений не создаёт unbounded очередь.

use std::num::NonZeroU64;
use std::sync::{Arc, Mutex, MutexGuard};

use crossbeam_channel::{Receiver, Sender, TrySendError, bounded};
use thiserror::Error;

use crate::{MediaTime, TimelineRange};

/// Идентичность одного port-а, назначенная source owner-ом.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub struct DynamicMediaTimelinePortGeneration(NonZeroU64);

impl DynamicMediaTimelinePortGeneration {
    /// Создаёт explicit generation без process-global allocator-а.
    #[must_use]
    pub const fn new(value: NonZeroU64) -> Self {
        Self(value)
    }

    /// Возвращает wire-neutral числовое значение.
    #[must_use]
    pub const fn get(self) -> NonZeroU64 {
        self.0
    }
}

/// Source-owned эпоха содержимого внутри одного port-а.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub struct DynamicMediaTimelineEpoch(u64);

impl DynamicMediaTimelineEpoch {
    /// Создаёт source epoch.
    #[must_use]
    pub const fn new(value: u64) -> Self {
        Self(value)
    }

    /// Возвращает числовое значение эпохи.
    #[must_use]
    pub const fn get(self) -> u64 {
        self.0
    }
}

/// Consumer-visible revision последнего immutable snapshot-а.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub struct DynamicMediaTimelineRevision(NonZeroU64);

impl DynamicMediaTimelineRevision {
    const INITIAL: Self = Self(NonZeroU64::MIN);

    /// Возвращает числовое значение revision.
    #[must_use]
    pub const fn get(self) -> NonZeroU64 {
        self.0
    }

    fn checked_next(self) -> Option<Self> {
        self.0
            .get()
            .checked_add(1)
            .and_then(NonZeroU64::new)
            .map(Self)
    }
}

/// Live-состояние без provider-specific vocabulary.
///
/// Manifest availability и packet-proven seekability разделены намеренно:
/// первая определяет expiry старой позиции, вторая — допустимые user seek-и.
/// Поля закрыты: диапазоны нельзя собрать в обход validator-а.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct DynamicMediaTimelineState {
    live_edge: MediaTime,
    availability_range: Option<TimelineRange>,
    seekable_range: Option<TimelineRange>,
}

impl DynamicMediaTimelineState {
    /// Создаёт no-DVR состояние.
    #[must_use]
    pub const fn without_dvr(live_edge: MediaTime) -> Self {
        Self {
            live_edge,
            availability_range: None,
            seekable_range: None,
        }
    }

    /// Создаёт DVR состояние после проверки непустого range и live edge.
    pub fn with_dvr(
        live_edge: MediaTime,
        seekable_range: TimelineRange,
    ) -> Result<Self, DynamicMediaTimelineValidationError> {
        if seekable_range.start >= seekable_range.end {
            return Err(DynamicMediaTimelineValidationError::EmptyDvrRange { seekable_range });
        }
        if live_edge < seekable_range.end {
            return Err(DynamicMediaTimelineValidationError::LiveEdgeBeforeDvrEnd {
                live_edge,
                seekable_range,
            });
        }
        Ok(Self {
            live_edge,
            availability_range: Some(seekable_range),
            seekable_range: Some(seekable_range),
        })
    }

    /// Создаёт DVR availability без ещё не полученного packet evidence.
    pub fn with_available_dvr(
        live_edge: MediaTime,
        availability_range: TimelineRange,
    ) -> Result<Self, DynamicMediaTimelineValidationError> {
        validate_availability_range(live_edge, availability_range)?;
        Ok(Self {
            live_edge,
            availability_range: Some(availability_range),
            seekable_range: None,
        })
    }

    /// Создаёт state с authoritative availability и его доказанным поддиапазоном.
    pub fn with_available_and_seekable_dvr(
        live_edge: MediaTime,
        availability_range: TimelineRange,
        seekable_range: TimelineRange,
    ) -> Result<Self, DynamicMediaTimelineValidationError> {
        validate_availability_range(live_edge, availability_range)?;
        if seekable_range.start >= seekable_range.end {
            return Err(DynamicMediaTimelineValidationError::EmptyDvrRange { seekable_range });
        }
        if seekable_range.start < availability_range.start
            || seekable_range.end > availability_range.end
        {
            return Err(
                DynamicMediaTimelineValidationError::SeekableRangeOutsideAvailability {
                    availability_range,
                    seekable_range,
                },
            );
        }
        Ok(Self {
            live_edge,
            availability_range: Some(availability_range),
            seekable_range: Some(seekable_range),
        })
    }

    /// Возвращает live edge независимо от наличия DVR.
    #[must_use]
    pub const fn live_edge(self) -> MediaTime {
        self.live_edge
    }

    /// Возвращает authoritative server availability window, если оно известно.
    #[must_use]
    pub const fn availability_range(self) -> Option<TimelineRange> {
        self.availability_range
    }

    /// Возвращает authoritative DVR range, если он сейчас существует.
    #[must_use]
    pub const fn seekable_range(self) -> Option<TimelineRange> {
        self.seekable_range
    }
}

/// Проверяет manifest/server availability независимо от packet evidence.
fn validate_availability_range(
    live_edge: MediaTime,
    availability_range: TimelineRange,
) -> Result<(), DynamicMediaTimelineValidationError> {
    if availability_range.start >= availability_range.end {
        return Err(
            DynamicMediaTimelineValidationError::EmptyAvailabilityRange { availability_range },
        );
    }
    if live_edge < availability_range.end {
        return Err(
            DynamicMediaTimelineValidationError::LiveEdgeBeforeAvailabilityEnd {
                live_edge,
                availability_range,
            },
        );
    }
    Ok(())
}

/// Initial producer state для создания связанной пары port/publisher.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct DynamicMediaTimelineInitial {
    /// Идентичность port-а для fencing после reopen/replace.
    pub port_generation: DynamicMediaTimelinePortGeneration,
    /// Initial source epoch.
    pub source_epoch: DynamicMediaTimelineEpoch,
    /// Initial live/DVR состояние.
    pub state: DynamicMediaTimelineState,
}

/// Immutable latest snapshot.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct DynamicMediaTimelineSnapshot {
    /// Идентичность port-а.
    pub port_generation: DynamicMediaTimelinePortGeneration,
    /// Monotonic source epoch.
    pub source_epoch: DynamicMediaTimelineEpoch,
    /// Monotonic consumer revision.
    pub revision: DynamicMediaTimelineRevision,
    /// Live edge и optional DVR window.
    pub state: DynamicMediaTimelineState,
}

/// Результат первой фазы observe.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct DynamicMediaTimelineObservation {
    /// Immutable latest snapshot на момент observe.
    pub snapshot: DynamicMediaTimelineSnapshot,
}

impl DynamicMediaTimelineObservation {
    /// Revision, которую consumer должен перепроверить после arm.
    #[must_use]
    pub const fn revision(self) -> DynamicMediaTimelineRevision {
        self.snapshot.revision
    }
}

/// Ошибка построения логически невозможного timeline.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Error)]
pub enum DynamicMediaTimelineValidationError {
    /// Availability window должен иметь положительную длину.
    #[error("dynamic availability range must be non-empty: {availability_range:?}")]
    EmptyAvailabilityRange {
        /// Некорректный authoritative диапазон.
        availability_range: TimelineRange,
    },
    /// Live edge не может быть раньше availability end.
    #[error("dynamic live edge {live_edge:?} is before availability end in {availability_range:?}")]
    LiveEdgeBeforeAvailabilityEnd {
        /// Некорректный live edge.
        live_edge: MediaTime,
        /// Availability, которому edge противоречит.
        availability_range: TimelineRange,
    },
    /// DVR range должен иметь положительную длину.
    #[error("dynamic DVR range must be non-empty: {seekable_range:?}")]
    EmptyDvrRange {
        /// Некорректный диапазон.
        seekable_range: TimelineRange,
    },
    /// Live edge не может быть раньше конца уже доступного DVR range.
    #[error("dynamic live edge {live_edge:?} is before DVR range end in {seekable_range:?}")]
    LiveEdgeBeforeDvrEnd {
        /// Некорректный live edge.
        live_edge: MediaTime,
        /// Диапазон, которому edge противоречит.
        seekable_range: TimelineRange,
    },
    /// Packet-proven seekability не может выходить за server availability.
    #[error(
        "dynamic seekable range {seekable_range:?} is outside availability {availability_range:?}"
    )]
    SeekableRangeOutsideAvailability {
        /// Authoritative доступное окно.
        availability_range: TimelineRange,
        /// Некорректный доказанный поддиапазон.
        seekable_range: TimelineRange,
    },
}

/// Итог producer publication.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum DynamicMediaTimelinePublishOutcome {
    /// Snapshot изменён, activity edge поставлен либо уже был coalesced.
    Published {
        /// Новая revision.
        revision: DynamicMediaTimelineRevision,
        /// `true`, если capacity-one канал уже содержал wake edge.
        coalesced: bool,
    },
    /// Полностью идентичный update не меняет revision и не будит consumer.
    Unchanged {
        /// Текущая revision.
        revision: DynamicMediaTimelineRevision,
    },
}

/// Typed producer failure без silent stale overwrite.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Error)]
pub enum DynamicMediaTimelinePublishError {
    /// Старый source epoch не имеет права переписать новый.
    #[error("stale dynamic timeline source epoch: current={current:?}, attempted={attempted:?}")]
    StaleSourceEpoch {
        /// Уже опубликованная эпоха.
        current: DynamicMediaTimelineEpoch,
        /// Отклонённая эпоха.
        attempted: DynamicMediaTimelineEpoch,
    },
    /// Revision исчерпала `u64`.
    #[error("dynamic timeline revision overflow")]
    RevisionOverflow,
    /// Consumer уже уничтожил port; producer не должен spin-ить.
    #[error("dynamic timeline consumer disconnected")]
    ConsumerDisconnected,
}

#[derive(Debug)]
struct DynamicMediaTimelineShared {
    latest: Mutex<DynamicMediaTimelineSnapshot>,
}

/// Read-only consumer boundary.
#[derive(Debug)]
pub struct DynamicMediaTimelinePort {
    shared: Arc<DynamicMediaTimelineShared>,
    activity_rx: Receiver<()>,
}

impl DynamicMediaTimelinePort {
    /// Возвращает generation без блокирующего I/O.
    #[must_use]
    pub fn port_generation(&self) -> DynamicMediaTimelinePortGeneration {
        self.lock_latest().port_generation
    }

    /// Первая фаза lost-wakeup-safe протокола: observe latest snapshot.
    #[must_use]
    pub fn observe(&self) -> DynamicMediaTimelineObservation {
        DynamicMediaTimelineObservation {
            snapshot: *self.lock_latest(),
        }
    }

    /// Arm-фаза: receiver клонируется только для того же consumer owner-а и wait-set.
    #[must_use]
    pub fn activity_receiver(&self) -> Receiver<()> {
        self.activity_rx.clone()
    }

    /// Recheck-фаза: возвращает новый snapshot, если publish пересёк observe/arm окно.
    #[must_use]
    pub fn recheck_after_arm(
        &self,
        observed_revision: DynamicMediaTimelineRevision,
    ) -> Option<DynamicMediaTimelineSnapshot> {
        let latest = *self.lock_latest();
        (latest.revision != observed_revision).then_some(latest)
    }

    fn lock_latest(&self) -> MutexGuard<'_, DynamicMediaTimelineSnapshot> {
        self.shared
            .latest
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
    }
}

/// Source-owned writer boundary.
#[derive(Debug, Clone)]
pub struct DynamicMediaTimelinePublisher {
    shared: Arc<DynamicMediaTimelineShared>,
    activity_tx: Sender<()>,
}

impl DynamicMediaTimelinePublisher {
    /// Атомарно заменяет latest snapshot и поднимает coalesced activity edge.
    pub fn publish(
        &self,
        source_epoch: DynamicMediaTimelineEpoch,
        state: DynamicMediaTimelineState,
    ) -> Result<DynamicMediaTimelinePublishOutcome, DynamicMediaTimelinePublishError> {
        let revision = {
            let mut latest = self
                .shared
                .latest
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner);
            if source_epoch < latest.source_epoch {
                return Err(DynamicMediaTimelinePublishError::StaleSourceEpoch {
                    current: latest.source_epoch,
                    attempted: source_epoch,
                });
            }
            if source_epoch == latest.source_epoch && state == latest.state {
                return Ok(DynamicMediaTimelinePublishOutcome::Unchanged {
                    revision: latest.revision,
                });
            }
            let revision = latest
                .revision
                .checked_next()
                .ok_or(DynamicMediaTimelinePublishError::RevisionOverflow)?;
            let port_generation = latest.port_generation;
            *latest = DynamicMediaTimelineSnapshot {
                port_generation,
                source_epoch,
                revision,
                state,
            };
            revision
        };

        match self.activity_tx.try_send(()) {
            Ok(()) => Ok(DynamicMediaTimelinePublishOutcome::Published {
                revision,
                coalesced: false,
            }),
            Err(TrySendError::Full(())) => Ok(DynamicMediaTimelinePublishOutcome::Published {
                revision,
                coalesced: true,
            }),
            Err(TrySendError::Disconnected(())) => {
                Err(DynamicMediaTimelinePublishError::ConsumerDisconnected)
            }
        }
    }
}

/// Создаёт единственную связанную пару consumer port/source publisher.
#[must_use]
pub fn dynamic_media_timeline(
    initial: DynamicMediaTimelineInitial,
) -> (DynamicMediaTimelinePort, DynamicMediaTimelinePublisher) {
    let initial_snapshot = DynamicMediaTimelineSnapshot {
        port_generation: initial.port_generation,
        source_epoch: initial.source_epoch,
        revision: DynamicMediaTimelineRevision::INITIAL,
        state: initial.state,
    };
    let shared = Arc::new(DynamicMediaTimelineShared {
        latest: Mutex::new(initial_snapshot),
    });
    let (activity_tx, activity_rx) = bounded(1);
    (
        DynamicMediaTimelinePort {
            shared: Arc::clone(&shared),
            activity_rx,
        },
        DynamicMediaTimelinePublisher {
            shared,
            activity_tx,
        },
    )
}

#[cfg(test)]
mod tests {
    use crossbeam_channel::TryRecvError;

    use super::*;

    fn generation(value: u64) -> DynamicMediaTimelinePortGeneration {
        DynamicMediaTimelinePortGeneration::new(
            NonZeroU64::new(value).expect("test generation must be non-zero"),
        )
    }

    fn no_dvr_pair() -> (DynamicMediaTimelinePort, DynamicMediaTimelinePublisher) {
        dynamic_media_timeline(DynamicMediaTimelineInitial {
            port_generation: generation(1),
            source_epoch: DynamicMediaTimelineEpoch::new(4),
            state: DynamicMediaTimelineState::without_dvr(MediaTime::from_secs(50)),
        })
    }

    #[test]
    fn no_dvr_and_non_zero_dvr_states_are_explicit() {
        let (port, publisher) = no_dvr_pair();
        assert_eq!(port.observe().snapshot.state.availability_range(), None);
        assert_eq!(port.observe().snapshot.state.seekable_range(), None);

        let dvr_range = TimelineRange::new(MediaTime::from_secs(20), MediaTime::from_secs(60))
            .expect("ordered DVR range");
        publisher
            .publish(
                DynamicMediaTimelineEpoch::new(5),
                DynamicMediaTimelineState::with_dvr(MediaTime::from_secs(61), dvr_range)
                    .expect("valid DVR state"),
            )
            .expect("publication");

        assert_eq!(
            port.observe().snapshot.state.seekable_range(),
            Some(dvr_range)
        );
        assert_eq!(
            port.observe().snapshot.state.availability_range(),
            Some(dvr_range)
        );
    }

    #[test]
    fn availability_and_packet_proof_are_distinct_and_nested() {
        let availability = TimelineRange::new(MediaTime::from_secs(20), MediaTime::from_secs(60))
            .expect("ordered availability");
        let proven = TimelineRange::new(MediaTime::from_secs(30), MediaTime::from_secs(50))
            .expect("ordered proof");
        let state = DynamicMediaTimelineState::with_available_and_seekable_dvr(
            MediaTime::from_secs(60),
            availability,
            proven,
        )
        .expect("nested proof");
        assert_eq!(state.availability_range(), Some(availability));
        assert_eq!(state.seekable_range(), Some(proven));

        let outside = TimelineRange::new(MediaTime::from_secs(10), MediaTime::from_secs(50))
            .expect("ordered outside proof");
        assert!(matches!(
            DynamicMediaTimelineState::with_available_and_seekable_dvr(
                MediaTime::from_secs(60),
                availability,
                outside,
            ),
            Err(DynamicMediaTimelineValidationError::SeekableRangeOutsideAvailability { .. })
        ));
    }

    #[test]
    fn invalid_empty_or_future_dvr_window_is_rejected() {
        let empty_range = TimelineRange::new(MediaTime::from_secs(5), MediaTime::from_secs(5))
            .expect("equal bounds are a valid generic timeline range");
        assert!(matches!(
            DynamicMediaTimelineState::with_dvr(MediaTime::from_secs(5), empty_range),
            Err(DynamicMediaTimelineValidationError::EmptyDvrRange { .. })
        ));

        let range = TimelineRange::new(MediaTime::from_secs(5), MediaTime::from_secs(15))
            .expect("ordered range");
        assert!(matches!(
            DynamicMediaTimelineState::with_dvr(MediaTime::from_secs(14), range),
            Err(DynamicMediaTimelineValidationError::LiveEdgeBeforeDvrEnd { .. })
        ));
    }

    #[test]
    fn observe_arm_recheck_closes_lost_wakeup_window() {
        let (port, publisher) = no_dvr_pair();
        let observed = port.observe();
        let activity_rx = port.activity_receiver();
        publisher
            .publish(
                DynamicMediaTimelineEpoch::new(5),
                DynamicMediaTimelineState::without_dvr(MediaTime::from_secs(51)),
            )
            .expect("publication in observe-arm window");

        let rechecked = port
            .recheck_after_arm(observed.revision())
            .expect("new revision must be visible before wait");
        assert_eq!(rechecked.state.live_edge(), MediaTime::from_secs(51));
        activity_rx
            .try_recv()
            .expect("wake edge remains observable");
    }

    #[test]
    fn burst_is_coalesced_but_latest_snapshot_is_not_lost() {
        let (port, publisher) = no_dvr_pair();
        let activity_rx = port.activity_receiver();
        for second in 51..=80 {
            publisher
                .publish(
                    DynamicMediaTimelineEpoch::new(5),
                    DynamicMediaTimelineState::without_dvr(MediaTime::from_secs(second)),
                )
                .expect("burst publication");
        }

        activity_rx.try_recv().expect("one coalesced activity edge");
        assert_eq!(activity_rx.try_recv(), Err(TryRecvError::Empty));
        assert_eq!(
            port.observe().snapshot.state.live_edge(),
            MediaTime::from_secs(80)
        );
    }

    #[test]
    fn stale_epoch_and_disconnected_consumer_are_typed() {
        let (port, publisher) = no_dvr_pair();
        assert!(matches!(
            publisher.publish(
                DynamicMediaTimelineEpoch::new(3),
                DynamicMediaTimelineState::without_dvr(MediaTime::from_secs(52)),
            ),
            Err(DynamicMediaTimelinePublishError::StaleSourceEpoch { .. })
        ));

        drop(port);
        assert_eq!(
            publisher.publish(
                DynamicMediaTimelineEpoch::new(5),
                DynamicMediaTimelineState::without_dvr(MediaTime::from_secs(52)),
            ),
            Err(DynamicMediaTimelinePublishError::ConsumerDisconnected)
        );
    }

    #[test]
    fn old_port_and_publisher_are_isolated_from_reopened_pair() {
        let (old_port, old_publisher) = no_dvr_pair();
        let (new_port, _new_publisher) = dynamic_media_timeline(DynamicMediaTimelineInitial {
            port_generation: generation(2),
            source_epoch: DynamicMediaTimelineEpoch::new(1),
            state: DynamicMediaTimelineState::without_dvr(MediaTime::from_secs(500)),
        });

        old_publisher
            .publish(
                DynamicMediaTimelineEpoch::new(5),
                DynamicMediaTimelineState::without_dvr(MediaTime::from_secs(99)),
            )
            .expect("old pair remains internally valid");

        assert_eq!(
            old_port.observe().snapshot.state.live_edge(),
            MediaTime::from_secs(99)
        );
        assert_eq!(
            new_port.observe().snapshot.state.live_edge(),
            MediaTime::from_secs(500)
        );
    }
}
