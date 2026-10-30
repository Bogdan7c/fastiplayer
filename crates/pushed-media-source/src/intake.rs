//! Общий учёт байтов, которые держит поток, с жёстким потолком.
//!
//! Это защита памяти от страницы, которая шлёт быстрее, чем мы играем. Мягкий
//! сигнал «полон» странице — [`crate::BufferIntake::Full`]; здесь жёсткий потолок:
//! сверх него приём — ошибка потока, а не рост памяти.
//!
//! Правило учёта (с 03b): целый сегмент зачитывается в учёт ОДИН раз, когда поток
//! его принял ([`ChargedSegment::charge`]), и выходит из учёта, когда исчезает
//! последняя его копия — на полке, в трубе эпизода или в ключе эпизода. Копии —
//! это общий `Bytes` (одна память), поэтому полка и труба не считаются дважды.

use std::sync::{Arc, Mutex};

use bytes::Bytes;

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

/// Целый сегмент, зачтённый в учёт потока, пока жива хоть одна его копия.
///
/// Клонирование дёшево (счётчик ссылок) и не меняет учёт: память одна.
#[derive(Clone)]
pub(crate) struct ChargedSegment {
    /// Байты и ручка учёта; учёт освобождается в `Drop` последней копии.
    inner: Arc<ChargedBytes>,
}

/// Байты сегмента и учёт, в котором они зачтены.
struct ChargedBytes {
    /// Байты сегмента.
    bytes: Bytes,
    /// Учёт потока.
    ledger: Arc<IntakeLedger>,
}

impl Drop for ChargedBytes {
    /// Последняя копия сегмента исчезла — его байты больше не держит никто.
    fn drop(&mut self) {
        self.ledger.release(self.bytes.len());
    }
}

impl ChargedSegment {
    /// Зачитывает сегмент в учёт; при превышении потолка учёт не меняется.
    pub(crate) fn charge(
        ledger: &Arc<IntakeLedger>,
        bytes: Bytes,
    ) -> Result<Self, IntakeLimitExceeded> {
        ledger.try_reserve(bytes.len())?;
        Ok(Self {
            inner: Arc::new(ChargedBytes {
                bytes,
                ledger: Arc::clone(ledger),
            }),
        })
    }

    /// Байты сегмента.
    pub(crate) fn bytes(&self) -> &Bytes {
        &self.inner.bytes
    }

    /// Размер сегмента в байтах.
    pub(crate) fn len(&self) -> usize {
        self.inner.bytes.len()
    }

    /// Держит ли сегмент кто-то ещё, кроме этой копии (труба эпизода, ключ).
    ///
    /// Снимок без замка: другая копия может исчезнуть сразу после ответа (труба
    /// дочитала). Ответ «держит» поэтому осторожный — его используют, чтобы НЕ
    /// снимать с полки то, что памяти сейчас не освободит.
    pub(crate) fn is_shared(&self) -> bool {
        Arc::strong_count(&self.inner) > 1
    }

    /// Это копия того же принятого сегмента (а не просто те же байты).
    ///
    /// Нужна для ключа эпизода: init, принятый заново после `changeType`, — другой
    /// init, даже если байты совпали.
    pub(crate) fn is_same_segment(&self, other: &Self) -> bool {
        Arc::ptr_eq(&self.inner, &other.inner)
    }
}

impl std::fmt::Debug for ChargedSegment {
    /// Содержимое сегмента в диагностику не выводится — только размер.
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("ChargedSegment")
            .field("len", &self.len())
            .finish()
    }
}

#[cfg(test)]
mod tests {
    use std::sync::Arc;

    use bytes::Bytes;

    use super::{ChargedSegment, IntakeLedger, IntakeLimitExceeded};

    #[test]
    fn charged_segment_is_released_only_when_its_last_copy_drops() {
        let ledger = Arc::new(IntakeLedger::new(100));
        let segment =
            ChargedSegment::charge(&ledger, Bytes::from_static(b"0123456789")).expect("room");
        let shelf_copy = segment.clone();
        assert!(segment.is_same_segment(&shelf_copy));
        // Копии не считаются дважды.
        assert_eq!(ledger.held_bytes(), 10);
        drop(segment);
        assert_eq!(ledger.held_bytes(), 10);
        drop(shelf_copy);
        assert_eq!(ledger.held_bytes(), 0);
        // Тот же байт в байт, но принятый заново, — другой сегмент.
        let first = ChargedSegment::charge(&ledger, Bytes::from_static(b"init")).expect("room");
        let again = ChargedSegment::charge(&ledger, Bytes::from_static(b"init")).expect("room");
        assert!(!first.is_same_segment(&again));
        assert_eq!(
            ChargedSegment::charge(&ledger, Bytes::from(vec![0; 93])).map(|_| ()),
            Err(IntakeLimitExceeded { limit: 100 })
        );
        assert_eq!(ledger.held_bytes(), 8);
    }

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
