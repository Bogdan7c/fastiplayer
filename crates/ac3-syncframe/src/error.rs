//! Типизированные причины, по которым байты не являются пригодным syncframe-ом.

use std::fmt;

/// Почему заголовок syncframe-а нельзя разобрать.
///
/// `Truncated` — единственный «не окончательный» вариант: данных просто ещё
/// не хватает, и потоковый потребитель должен дочитать байты. Все остальные
/// варианты означают, что в этом месте настоящего syncframe-а нет (ложное
/// синхрослово или повреждение) либо поток использует неподдерживаемое
/// расширение.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SyncframeHeaderError {
    /// Байтов меньше, чем нужно для разбора заголовка или всего кадра.
    Truncated {
        /// Сколько байт было доступно.
        available_bytes: usize,
        /// Сколько байт нужно, чтобы продолжить разбор.
        required_bytes: usize,
    },
    /// Первые два байта не равны синхрослову `0x0B77`.
    MissingSyncWord,
    /// `bsid` вне поддерживаемых диапазонов: 0–10 (AC-3, где 9–10 — пониженная
    /// частота) и 11–16 (E-AC-3); 17–31 зарезервированы спецификацией.
    UnsupportedBitstreamId {
        /// Прочитанное значение `bsid`.
        bitstream_id: u8,
    },
    /// Зарезервированный код частоты: `fscod == 3` у AC-3 или `fscod2 == 3` у E-AC-3.
    ReservedSampleRateCode,
    /// `frmsizecod` AC-3 вне таблицы 5.18 (значения 38–63 зарезервированы).
    ReservedFrameSizeCode {
        /// Прочитанное значение `frmsizecod`.
        frame_size_code: u8,
    },
    /// `strmtyp == 3` у E-AC-3 зарезервирован.
    ReservedStreamType,
    /// Заявленная длина кадра меньше, чем уже прочитанный заголовок.
    FrameShorterThanHeader {
        /// Длина кадра из заголовка.
        frame_length_bytes: usize,
        /// Сколько байт занял разобранный заголовок.
        header_bytes: usize,
    },
    /// В одном access unit-е слишком много syncframe-ов подряд без нового
    /// независимого substream-а 0 — поток повреждён или враждебен.
    AccessUnitTooLong {
        /// Предел, который был превышен.
        maximum_syncframes: usize,
    },
}

impl SyncframeHeaderError {
    /// Можно ли продолжить, дочитав ещё байты (в отличие от «здесь не syncframe»).
    #[must_use]
    pub const fn is_truncation(self) -> bool {
        matches!(self, Self::Truncated { .. })
    }
}

impl fmt::Display for SyncframeHeaderError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Truncated {
                available_bytes,
                required_bytes,
            } => write!(
                formatter,
                "AC-3 syncframe is truncated: {available_bytes} bytes available, {required_bytes} required"
            ),
            Self::MissingSyncWord => write!(formatter, "AC-3 sync word 0x0B77 is missing"),
            Self::UnsupportedBitstreamId { bitstream_id } => write!(
                formatter,
                "AC-3 bitstream id {bitstream_id} is reserved or unsupported"
            ),
            Self::ReservedSampleRateCode => {
                write!(formatter, "AC-3 sample rate code is reserved")
            }
            Self::ReservedFrameSizeCode { frame_size_code } => write!(
                formatter,
                "AC-3 frame size code {frame_size_code} is reserved"
            ),
            Self::ReservedStreamType => write!(formatter, "E-AC-3 stream type 3 is reserved"),
            Self::FrameShorterThanHeader {
                frame_length_bytes,
                header_bytes,
            } => write!(
                formatter,
                "AC-3 frame length {frame_length_bytes} is shorter than its {header_bytes}-byte header"
            ),
            Self::AccessUnitTooLong { maximum_syncframes } => write!(
                formatter,
                "E-AC-3 access unit exceeds {maximum_syncframes} syncframes without a new independent substream"
            ),
        }
    }
}

impl std::error::Error for SyncframeHeaderError {}
