use std::num::NonZeroUsize;
use std::sync::{Arc, Mutex};

use tracing::warn;

/// VAAPI-local контекст для reservation accounting playback decoder-а.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) struct VaapiSharedHardwareOwnerContext {
    /// Сколько VA surfaces принадлежит active playback decoder-у.
    playback_surface_budget: NonZeroUsize,
}

impl VaapiSharedHardwareOwnerContext {
    /// Создаёт контекст с уже нормализованным playback budget-ом.
    #[must_use]
    pub(crate) const fn new(playback_surface_budget: NonZeroUsize) -> Self {
        Self {
            playback_surface_budget,
        }
    }

    /// Строит контекст из surface pool size, нормализуя ноль до одного surface.
    #[must_use]
    pub(crate) fn from_surface_accounting(surface_pool_frames: usize) -> Self {
        Self::new(non_zero_or_one(surface_pool_frames))
    }

    /// Возвращает playback surface budget без раскрытия внутреннего layout-а.
    #[must_use]
    pub(crate) const fn playback_surface_budget(self) -> NonZeroUsize {
        self.playback_surface_budget
    }
}

/// Owner reservation state для VAAPI playback branch.
#[derive(Debug, Clone)]
pub(crate) struct VaapiSharedHardwareOwner {
    /// Shared state нужен, чтобы reservation token мог release-иться в `Drop`.
    inner: Arc<Mutex<VaapiSharedHardwareOwnerInner>>,
}

impl VaapiSharedHardwareOwner {
    /// Создаёт owner для одного VAAPI decoder-а.
    #[must_use]
    pub(crate) fn new(context: VaapiSharedHardwareOwnerContext) -> Self {
        Self {
            inner: Arc::new(Mutex::new(VaapiSharedHardwareOwnerInner::new(context))),
        }
    }

    /// Резервирует playback branch ровно один раз на lifetime decoder-а.
    pub(crate) fn reserve_playback_branch(
        &self,
    ) -> Result<VaapiPlaybackHardwareReservation, VaapiPlaybackReservationError> {
        let mut inner = self
            .inner
            .lock()
            .map_err(|_| VaapiPlaybackReservationError::OwnerUnavailable)?;
        let active = inner.reserve_playback_branch()?;
        Ok(VaapiPlaybackHardwareReservation::new(
            Arc::clone(&self.inner),
            active,
        ))
    }

    /// Возвращает snapshot для unit tests без выдачи mutable state наружу.
    #[cfg(test)]
    fn snapshot_for_tests(&self) -> VaapiSharedHardwareOwnerSnapshot {
        let inner = self
            .inner
            .lock()
            .expect("VAAPI shared owner mutex must not be poisoned in tests");
        VaapiSharedHardwareOwnerSnapshot {
            playback_active: inner.playback_reservation.is_some(),
            playback_surface_budget: inner.context.playback_surface_budget(),
        }
    }
}

#[derive(Debug)]
struct VaapiSharedHardwareOwnerInner {
    /// Immutable accounting facts для active decoder-а.
    context: VaapiSharedHardwareOwnerContext,

    /// Monotonic id защищает release от stale token-а.
    next_reservation_id: u64,

    /// Active playback reservation, если decoder уже стартовал.
    playback_reservation: Option<ActiveHardwareReservation>,
}

impl VaapiSharedHardwareOwnerInner {
    /// Создаёт пустой owner state.
    fn new(context: VaapiSharedHardwareOwnerContext) -> Self {
        Self {
            context,
            next_reservation_id: 1,
            playback_reservation: None,
        }
    }

    /// Резервирует playback surfaces, если branch ещё не занят.
    fn reserve_playback_branch(
        &mut self,
    ) -> Result<ActiveHardwareReservation, VaapiPlaybackReservationError> {
        if self.playback_reservation.is_some() {
            return Err(VaapiPlaybackReservationError::ExistingPlaybackReservation);
        }

        let active = self.create_active_reservation(self.context.playback_surface_budget());
        self.playback_reservation = Some(active);
        Ok(active)
    }

    /// Освобождает playback reservation, если token всё ещё актуален.
    fn release_reservation(
        &mut self,
        reservation_id: VaapiHardwareReservationId,
        surface_frames: NonZeroUsize,
    ) -> VaapiHardwareReservationReleaseOutcome {
        let descriptor = VaapiHardwareReservationRelease {
            reservation_id,
            surface_frames,
        };

        match self.playback_reservation {
            Some(active) if active.reservation_id == reservation_id => {
                self.playback_reservation = None;
                VaapiHardwareReservationReleaseOutcome::Released(descriptor)
            }
            Some(_) | None => VaapiHardwareReservationReleaseOutcome::StaleReservation(descriptor),
        }
    }

    /// Создаёт active reservation с новым monotonically increasing id.
    fn create_active_reservation(
        &mut self,
        surface_frames: NonZeroUsize,
    ) -> ActiveHardwareReservation {
        let reservation_id = VaapiHardwareReservationId(self.next_reservation_id);
        self.next_reservation_id = self.next_reservation_id.saturating_add(1).max(1);
        ActiveHardwareReservation {
            reservation_id,
            surface_frames,
        }
    }
}

/// Internal id active VAAPI reservation-а.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub(crate) struct VaapiHardwareReservationId(u64);

/// Active reservation descriptor, хранимый owner-ом.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct ActiveHardwareReservation {
    /// Guard против stale release token-а.
    reservation_id: VaapiHardwareReservationId,

    /// Сколько surfaces удерживает branch.
    surface_frames: NonZeroUsize,
}

/// RAII reservation playback branch-а.
pub(crate) struct VaapiPlaybackHardwareReservation {
    /// Token release-ит owner state при drop-е.
    token: VaapiHardwareReservationToken,
}

impl VaapiPlaybackHardwareReservation {
    /// Создаёт reservation из owner-issued descriptor-а.
    fn new(
        owner: Arc<Mutex<VaapiSharedHardwareOwnerInner>>,
        active: ActiveHardwareReservation,
    ) -> Self {
        Self {
            token: VaapiHardwareReservationToken::new(owner, active),
        }
    }

    /// Возвращает число зарезервированных playback surfaces.
    #[must_use]
    pub(crate) const fn surface_frames(&self) -> NonZeroUsize {
        self.token.surface_frames()
    }
}

impl Drop for VaapiPlaybackHardwareReservation {
    fn drop(&mut self) {
        self.token.release_for_drop();
    }
}

/// Shared RAII token, который знает, как release-ить owner state.
struct VaapiHardwareReservationToken {
    /// Owner state, где хранится active reservation.
    owner: Arc<Mutex<VaapiSharedHardwareOwnerInner>>,

    /// Guard id reservation-а.
    reservation_id: VaapiHardwareReservationId,

    /// Зарезервированное число surfaces.
    surface_frames: NonZeroUsize,

    /// Защита от double release.
    released: bool,
}

impl VaapiHardwareReservationToken {
    /// Создаёт token из active descriptor-а.
    fn new(
        owner: Arc<Mutex<VaapiSharedHardwareOwnerInner>>,
        active: ActiveHardwareReservation,
    ) -> Self {
        Self {
            owner,
            reservation_id: active.reservation_id,
            surface_frames: active.surface_frames,
            released: false,
        }
    }

    /// Возвращает число surfaces, не раскрывая token internals.
    const fn surface_frames(&self) -> NonZeroUsize {
        self.surface_frames
    }

    /// Освобождает reservation явно или из `Drop`.
    fn release(&mut self) -> VaapiHardwareReservationReleaseOutcome {
        if self.released {
            return VaapiHardwareReservationReleaseOutcome::AlreadyReleased(
                self.release_descriptor(),
            );
        }

        self.released = true;
        match self.owner.lock() {
            Ok(mut owner) => owner.release_reservation(self.reservation_id, self.surface_frames),
            Err(_) => {
                VaapiHardwareReservationReleaseOutcome::OwnerUnavailable(self.release_descriptor())
            }
        }
    }

    /// Release из `Drop` не может вернуть ошибку caller-у, поэтому только логирует abnormal state.
    fn release_for_drop(&mut self) {
        match self.release() {
            VaapiHardwareReservationReleaseOutcome::Released(_)
            | VaapiHardwareReservationReleaseOutcome::AlreadyReleased(_) => {}
            VaapiHardwareReservationReleaseOutcome::OwnerUnavailable(release)
            | VaapiHardwareReservationReleaseOutcome::StaleReservation(release) => {
                warn!(
                    ?release,
                    "VAAPI playback hardware reservation release did not reach active owner state"
                );
            }
        }
    }

    /// Собирает release descriptor из immutable token fields.
    fn release_descriptor(&self) -> VaapiHardwareReservationRelease {
        VaapiHardwareReservationRelease {
            reservation_id: self.reservation_id,
            surface_frames: self.surface_frames,
        }
    }
}

/// Descriptor release попытки, пригодный для diagnostics/tests.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) struct VaapiHardwareReservationRelease {
    /// Reservation id, который пытались освободить.
    pub(crate) reservation_id: VaapiHardwareReservationId,

    /// Число surfaces в reservation-е.
    pub(crate) surface_frames: NonZeroUsize,
}

/// Итог release-а playback reservation-а.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum VaapiHardwareReservationReleaseOutcome {
    Released(VaapiHardwareReservationRelease),
    AlreadyReleased(VaapiHardwareReservationRelease),
    OwnerUnavailable(VaapiHardwareReservationRelease),
    StaleReservation(VaapiHardwareReservationRelease),
}

/// Ошибка создания playback reservation-а.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum VaapiPlaybackReservationError {
    OwnerUnavailable,
    ExistingPlaybackReservation,
}

impl std::fmt::Display for VaapiPlaybackReservationError {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::OwnerUnavailable => write!(formatter, "VAAPI reservation owner unavailable"),
            Self::ExistingPlaybackReservation => {
                write!(formatter, "VAAPI playback reservation already exists")
            }
        }
    }
}

impl std::error::Error for VaapiPlaybackReservationError {}

#[cfg(test)]
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct VaapiSharedHardwareOwnerSnapshot {
    playback_active: bool,
    playback_surface_budget: NonZeroUsize,
}

/// Нормализует `usize` budget в positive non-zero значение.
fn non_zero_or_one(value: usize) -> NonZeroUsize {
    NonZeroUsize::new(value).unwrap_or_else(|| NonZeroUsize::new(1).expect("1 is non-zero"))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn owner(surface_frames: usize) -> VaapiSharedHardwareOwner {
        VaapiSharedHardwareOwner::new(VaapiSharedHardwareOwnerContext::from_surface_accounting(
            surface_frames,
        ))
    }

    #[test]
    fn surface_accounting_normalizes_zero_to_one() {
        let context = VaapiSharedHardwareOwnerContext::from_surface_accounting(0);

        assert_eq!(context.playback_surface_budget().get(), 1);
    }

    #[test]
    fn playback_reservation_is_single_owner_until_drop() {
        let owner = owner(8);
        let reservation = owner
            .reserve_playback_branch()
            .expect("first playback reservation must fit");

        assert_eq!(reservation.surface_frames().get(), 8);
        assert!(matches!(
            owner.reserve_playback_branch(),
            Err(VaapiPlaybackReservationError::ExistingPlaybackReservation)
        ));
        assert!(owner.snapshot_for_tests().playback_active);

        drop(reservation);

        assert!(!owner.snapshot_for_tests().playback_active);
        let second_reservation = owner
            .reserve_playback_branch()
            .expect("drop must release playback reservation");
        assert_eq!(second_reservation.surface_frames().get(), 8);
    }

    #[test]
    fn release_is_idempotent_for_token_owner() {
        let owner = owner(4);
        let mut reservation = owner
            .reserve_playback_branch()
            .expect("playback reservation must fit");

        let released = reservation.token.release();
        assert!(matches!(
            released,
            VaapiHardwareReservationReleaseOutcome::Released(_)
        ));
        assert!(!owner.snapshot_for_tests().playback_active);

        let repeated = reservation.token.release();
        assert!(matches!(
            repeated,
            VaapiHardwareReservationReleaseOutcome::AlreadyReleased(_)
        ));
    }
}
