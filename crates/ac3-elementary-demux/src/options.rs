//! Ограничения памяти и работы demuxer-а «сырого» AC-3.

use std::num::NonZeroUsize;

use crate::Ac3ElementaryOptionsError;

/// Именованный ненулевой предел: нулём нельзя случайно «выключить» границу.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Ac3ElementaryLimit(NonZeroUsize);

impl Ac3ElementaryLimit {
    /// Проверяет значение; `name` попадает в ошибку, чтобы было видно, какой предел нулевой.
    pub fn new(value: usize, name: &'static str) -> Result<Self, Ac3ElementaryOptionsError> {
        NonZeroUsize::new(value)
            .map(Self)
            .ok_or(Ac3ElementaryOptionsError::ZeroLimit { name })
    }

    /// Проверенное значение.
    #[must_use]
    pub const fn get(self) -> usize {
        self.0.get()
    }
}

/// Все пределы demuxer-а. Значения по умолчанию рассчитаны на локальные файлы и HTTP.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Ac3ElementaryDemuxOptions {
    /// Сколько байт читать из источника за один запрос.
    pub read_chunk_bytes: Ac3ElementaryLimit,
    /// Окно открытия: в нём обязан найтись первый access unit, и по всем
    /// найденным в нём кадрам меряется «байт на сэмпл» для длительности и seek.
    /// Должно вмещать хотя бы два максимальных кадра (E-AC-3 пакет закрывается
    /// только заголовком следующего).
    pub initial_scan_bytes: Ac3ElementaryLimit,
    /// Сколько байт подряд можно отбросить при поиске синхронизации (мусор,
    /// повреждение, dependent-ы без своего substream-а 0), прежде чем поток
    /// считается не-AC-3 (typed `SyncLost`).
    pub max_resync_bytes: Ac3ElementaryLimit,
    /// Максимальный размер одного ID3v2-тега в начале файла (обложки бывают
    /// большими, но не бесконечными: без предела битый размер заставил бы
    /// дочитывать гигабайты потока).
    pub max_leading_tag_bytes: Ac3ElementaryLimit,
}

impl Default for Ac3ElementaryDemuxOptions {
    fn default() -> Self {
        Self {
            read_chunk_bytes: Ac3ElementaryLimit(
                const { NonZeroUsize::new(64 * 1024).expect("non-zero") },
            ),
            initial_scan_bytes: Ac3ElementaryLimit(
                const { NonZeroUsize::new(256 * 1024).expect("non-zero") },
            ),
            max_resync_bytes: Ac3ElementaryLimit(
                const { NonZeroUsize::new(1024 * 1024).expect("non-zero") },
            ),
            max_leading_tag_bytes: Ac3ElementaryLimit(
                const { NonZeroUsize::new(16 * 1024 * 1024).expect("non-zero") },
            ),
        }
    }
}
