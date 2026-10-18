//! Типизированные ошибки разбора сегментов PGS.

use crate::segment::SegmentKind;

/// Ошибка разбора содержимого одного сегмента. Вызывающий код пропускает такой сегмент
/// и увеличивает счётчик; дорожка от этого не прерывается.
#[derive(Debug, Clone, Copy, PartialEq, Eq, thiserror::Error)]
pub enum SegmentParseError {
    /// Сегмент короче, чем требует его формат.
    #[error("{segment:?} segment truncated: needs {needed} more bytes, {available} available")]
    Truncated {
        segment: SegmentKind,
        needed: usize,
        available: usize,
    },
    /// PCS объявил нулевой размер видео: система координат не определена.
    #[error("presentation composition declares a zero video size")]
    ZeroVideoSize,
    /// PCS объявил размер кадра больше допустимого для формата (см. `MAX_REFERENCE_WIDTH`).
    #[error("presentation composition declares an oversized video frame {width}x{height}")]
    OversizedVideoSize { width: u16, height: u16 },
    /// Первый фрагмент ODS объявил нулевую ширину или высоту картинки.
    #[error("object definition declares a zero picture size")]
    ZeroObjectSize,
}
