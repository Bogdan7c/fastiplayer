//! Счётчики библиотеки шрифтов, которые обновляются из методов `&self`.
//!
//! Чтение метрик и покрытия идёт через разделяемые ссылки (`&SubtitleFontLibrary`), поэтому
//! счётчики атомарные: так не нужен `&mut` ради простого учёта и библиотека остаётся `Sync`.

use std::sync::atomic::{AtomicU64, Ordering};

/// Счётчики событий ввода-вывода и сбоев чтения шрифтов.
#[derive(Debug, Default)]
pub(super) struct LibraryCounters {
    /// Сколько раз открывался файл системного шрифта (mmap/чтение).
    system_file_opens: AtomicU64,
    /// Сколько раз лицо не удалось разобрать как шрифт.
    unreadable_faces: AtomicU64,
}

impl LibraryCounters {
    /// Записывает открытие файла системного шрифта.
    pub(super) fn note_system_file_open(&self) {
        self.system_file_opens.fetch_add(1, Ordering::Relaxed);
    }

    /// Записывает лицо, которое не разобралось как шрифт.
    pub(super) fn note_unreadable_face(&self) {
        self.unreadable_faces.fetch_add(1, Ordering::Relaxed);
    }

    /// Открытий файлов системных шрифтов.
    pub(super) fn system_file_opens(&self) -> u64 {
        self.system_file_opens.load(Ordering::Relaxed)
    }

    /// Неразобранных лиц.
    pub(super) fn unreadable_faces(&self) -> u64 {
        self.unreadable_faces.load(Ordering::Relaxed)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn counters_start_at_zero_and_count_independently() {
        let counters = LibraryCounters::default();
        assert_eq!(
            (counters.system_file_opens(), counters.unreadable_faces()),
            (0, 0)
        );
        counters.note_system_file_open();
        counters.note_system_file_open();
        counters.note_unreadable_face();
        assert_eq!(
            (counters.system_file_opens(), counters.unreadable_faces()),
            (2, 1)
        );
    }
}
