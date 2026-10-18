//! Учёт памяти с общими данными: одна и та же картинка или палитра (разделяемая через `Arc`
//! между композициями) считается один раз, пока на неё ссылается хотя бы одна композиция.

use std::collections::HashMap;
use std::sync::Arc;

/// Подсчёт ссылок по адресу общего объекта.
#[derive(Default)]
pub(super) struct SharedBytesLedger {
    /// Адрес объекта -> (его размер, число ссылающихся реплик).
    entries: HashMap<usize, (usize, usize)>,
    total_bytes: usize,
}

impl SharedBytesLedger {
    pub(super) fn total_bytes(&self) -> usize {
        self.total_bytes
    }

    /// Регистрирует ссылку на объект размера `bytes`.
    pub(super) fn add<T>(&mut self, shared: &Arc<T>, bytes: usize) {
        let entry = self
            .entries
            .entry(Arc::as_ptr(shared) as usize)
            .or_insert((bytes, 0));
        if entry.1 == 0 {
            self.total_bytes = self.total_bytes.saturating_add(bytes);
        }
        entry.1 += 1;
    }

    /// Снимает ссылку; когда она последняя, размер вычитается из итога.
    pub(super) fn remove<T>(&mut self, shared: &Arc<T>) {
        let key = Arc::as_ptr(shared) as usize;
        if let Some(entry) = self.entries.get_mut(&key) {
            entry.1 -= 1;
            if entry.1 == 0 {
                self.total_bytes = self.total_bytes.saturating_sub(entry.0);
                self.entries.remove(&key);
            }
        }
    }
}
