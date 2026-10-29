//! Общий учёт байтов, которые поток принял, но демуксеры ещё не прочитали.
//!
//! Это защита памяти от страницы, которая шлёт быстрее, чем мы играем. Мягкий
//! сигнал «полон» странице (обратное давление) — забота 03b; здесь жёсткий потолок:
//! сверх него приём — ошибка потока, а не рост памяти.

use std::sync::Mutex;

/// Счётчик удерживаемых байтов потока с жёстким потолком.
#[derive(Debug)]
pub(crate) struct IntakeLedger {
    /// Сколько байтов сейчас удерживается в трубах эпизодов.
    held_bytes: Mutex<usize>,
    /// Потолок.
    limit: usize,
}

/// Потолок превышен: принять сегмент нельзя.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) struct IntakeLimitExceeded {
    /// Потолок потока.
    pub(crate) limit: usize,
}

impl IntakeLedger {
    /// Пустой учёт с потолком.
    pub(crate) fn new(limit: usize) -> Self {
        Self {
            held_bytes: Mutex::new(0),
            limit,
        }
    }

    /// Резервирует место под сегмент; при превышении потолка ничего не меняет.
    pub(crate) fn try_reserve(&self, segment_bytes: usize) -> Result<(), IntakeLimitExceeded> {
        let mut held = self.lock();
        let after = held
            .checked_add(segment_bytes)
            .filter(|after| *after <= self.limit)
            .ok_or(IntakeLimitExceeded { limit: self.limit })?;
        *held = after;
        Ok(())
    }

    /// Возвращает место: байты прочитаны демуксером или выброшены вместе с эпизодом.
    pub(crate) fn release(&self, segment_bytes: usize) {
        let mut held = self.lock();
        // Возврат больше удержанного — ошибка учёта; насыщение не даёт счётчику
        // уйти «в минус» и навсегда закрыть приём.
        debug_assert!(
            segment_bytes <= *held,
            "intake ledger released more than held"
        );
        *held = held.saturating_sub(segment_bytes);
    }

    /// Сколько байтов удерживается сейчас.
    pub(crate) fn held_bytes(&self) -> usize {
        *self.lock()
    }

    /// Захватывает счётчик; отравленный mutex не мешает учёту.
    fn lock(&self) -> std::sync::MutexGuard<'_, usize> {
        self.held_bytes
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
    }
}

#[cfg(test)]
mod tests {
    use super::{IntakeLedger, IntakeLimitExceeded};

    #[test]
    fn reserve_up_to_limit_then_refuse_without_changing_held_bytes() {
        let ledger = IntakeLedger::new(100);
        assert_eq!(ledger.try_reserve(60), Ok(()));
        assert_eq!(ledger.try_reserve(40), Ok(()));
        assert_eq!(
            ledger.try_reserve(1),
            Err(IntakeLimitExceeded { limit: 100 })
        );
        assert_eq!(ledger.held_bytes(), 100);
        ledger.release(30);
        assert_eq!(ledger.try_reserve(30), Ok(()));
        assert_eq!(ledger.held_bytes(), 100);
    }

    #[test]
    fn overflowing_reservation_is_refused() {
        let ledger = IntakeLedger::new(usize::MAX);
        assert_eq!(ledger.try_reserve(usize::MAX), Ok(()));
        assert!(ledger.try_reserve(1).is_err());
    }
}
