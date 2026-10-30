//! Источник байтов файла: `ByteSource`, который вместо диска просит страницу.
//!
//! Им владеет поток предзагрузки (`media-prefetch`): он читает кусками, переходит
//! по файлу перемоткой и держит окно в памяти. Здесь только «диск»: чтение с позиции
//! превращается в запрос диапазона, а байты берутся из рамок страницы.

use std::sync::Arc;

use source_core::{
    ByteSource, CancellationToken, NotSeekableReason, Seekability, SourceError, SourceFingerprint,
    SourceResult, SourceValidators,
};

use super::core::FileCore;

/// Метка источника для кэшей и журнала: адрес страницы сюда не попадает.
const FINGERPRINT: &str = "pushed-range-file";

/// Файл, читаемый запросами диапазонов.
pub(super) struct RangeFileSource {
    /// Общее ядро.
    core: Arc<FileCore>,
    /// Позиция читателя.
    position: u64,
    /// Можно ли перематывать снаружи (размер файла известен).
    seekability: Seekability,
    /// Размер файла, если известен.
    content_length: Option<u64>,
}

impl RangeFileSource {
    /// Источник над ядром; позиция 0.
    pub(super) fn new(core: Arc<FileCore>, content_length: Option<u64>) -> Self {
        let seekability = if content_length.is_some() {
            Seekability::Seekable
        } else {
            Seekability::NotSeekable {
                reason: NotSeekableReason::Unknown,
            }
        };
        Self {
            core,
            position: 0,
            seekability,
            content_length,
        }
    }

    /// Внутренняя перестановка позиции: работает и у файла неизвестного размера.
    ///
    /// Страница умеет отдавать диапазоны всегда, а «не перематываемость» — договор с
    /// демуксером. Источник сам (проверка раскладки MP4 перед открытием) возвращается
    /// к началу, не нарушая этого договора.
    pub(super) const fn move_to(&mut self, offset: u64) {
        self.position = offset;
    }
}

impl ByteSource for RangeFileSource {
    fn read(&mut self, output: &mut [u8], cancellation: &CancellationToken) -> SourceResult<usize> {
        let count = self.core.read_at(self.position, output, cancellation)?;
        self.position += count as u64;
        Ok(count)
    }

    fn seek(&mut self, offset: u64) -> SourceResult<()> {
        if !self.seekability.is_seekable() {
            return Err(SourceError::NotSeekable {
                reason: NotSeekableReason::Unknown,
            });
        }
        self.move_to(offset);
        Ok(())
    }

    fn position(&self) -> u64 {
        self.position
    }

    fn seekability(&self) -> Seekability {
        self.seekability.clone()
    }

    fn validators(&self) -> SourceValidators {
        SourceValidators::default()
    }

    fn content_length(&self) -> Option<u64> {
        self.content_length
    }

    fn fingerprint(&self) -> SourceFingerprint {
        SourceFingerprint::new(FINGERPRINT)
    }
}

impl Drop for RangeFileSource {
    /// Читателя нет: буфер запроса отпускается, поздние рамки — устаревшие.
    fn drop(&mut self) {
        self.core.close_reader();
    }
}
