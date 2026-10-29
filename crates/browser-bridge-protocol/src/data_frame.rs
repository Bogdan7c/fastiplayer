//! Двоичная рамка куска байтов — двоичная рамка WebSocket сеанса.
//!
//! Байты не кодируются в base64 и не заворачиваются в JSON: WebSocket из
//! background возит `ArrayBuffer` как есть (решение 82). Заголовок фиксированный,
//! 24 байта, все числа little-endian (родной порядок x86 и ARM; `DataView` в
//! JavaScript читает его одним флагом):
//!
//! ```text
//! смещение  размер  поле
//!  0        1       вид рамки: 1 — байты SourceBuffer, 2 — байты файла <video src>
//!  1        1       флаги — пока 0 (резерв для будущих minor)
//!  2        2       резерв — 0
//!
//!  вид 1, байты SourceBuffer:
//!  4        4       поколение (от 1)
//!  8        2       индекс SourceBuffer (0–7)
//! 10        6       резерв — 0
//! 16        8       порядковый номер рамки в SourceBuffer поколения
//!
//!  вид 2, байты файла <video src> (ответ на fetch_range):
//!  4        4       номер запроса
//!  8        8       смещение первого байта в файле (до 2^53 − 1)
//! 16        8       резерв — 0
//!
//! 24        1…1 МиБ полезный груз
//! ```
//!
//! Ненулевой резерв — ошибка (строгая политика): если будущая `minor` займёт
//! резерв, её рамки старой стороне никто не пошлёт (`crate::version`).

use serde::{Deserialize, Serialize};

use crate::limits::{DATA_FRAME_HEADER_BYTES, MAX_DATA_FRAME_PAYLOAD_BYTES};
use crate::stream::{AppendSequence, RangeRequestId, SourceBufferIndex, StreamGeneration};
use crate::time::ByteOffset;

/// Код вида рамки «байты SourceBuffer».
const SOURCE_BUFFER_BYTES_KIND: u8 = 1;
/// Код вида рамки «байты файла `<video src>`».
const RANGE_BYTES_KIND: u8 = 2;

/// Заголовок рамки байтов SourceBuffer.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct SourceBufferBytesHeader {
    /// Поколение потока.
    pub generation: StreamGeneration,
    /// SourceBuffer.
    pub source_buffer: SourceBufferIndex,
    /// Порядковый номер рамки в SourceBuffer поколения.
    pub sequence: AppendSequence,
}

/// Заголовок рамки байтов файла `<video src>`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct RangeBytesHeader {
    /// Номер запроса `fetch_range`.
    pub request_id: RangeRequestId,
    /// Смещение первого байта груза в файле.
    pub offset: ByteOffset,
}

/// Заголовок рамки одного из двух видов.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum DataFrameHeader {
    /// Байты, которые плеер страницы отдал `appendBuffer`.
    SourceBufferBytes(SourceBufferBytesHeader),
    /// Байты файла `<video src>`, скачанные по запросу плеера.
    RangeBytes(RangeBytesHeader),
}

/// Разобранная рамка: заголовок и груз без копирования.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct DataFrame<'frame> {
    /// Заголовок.
    pub header: DataFrameHeader,
    /// Полезный груз (1 байт … 1 МиБ).
    pub payload: &'frame [u8],
}

/// Вид отказа двоичной рамки. Имена — общий словарь фикстур.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize, thiserror::Error)]
#[serde(rename_all = "snake_case")]
pub enum DataFrameError {
    /// Короче заголовка.
    #[error("data frame is shorter than its header")]
    FrameTooShort,
    /// Незнакомый вид рамки.
    #[error("unknown data frame kind")]
    UnknownFrameKind,
    /// Ненулевые флаги или резерв.
    #[error("reserved data frame bits are set")]
    ReservedBitsSet,
    /// Нет полезного груза: пустой кусок ничего не несёт и только тратит номер.
    #[error("data frame payload is empty")]
    PayloadEmpty,
    /// Груз больше лимита.
    #[error("data frame payload exceeds the limit")]
    PayloadTooLarge,
    /// Поле вне диапазона (поколение 0, SourceBuffer ≥ 8, смещение > 2⁵³ − 1).
    #[error("data frame field is out of range")]
    NumberOutOfRange,
}

/// Разбирает двоичную рамку и проверяет её целиком.
///
/// # Errors
/// [`DataFrameError`] с точной причиной.
pub fn decode_data_frame(bytes: &[u8]) -> Result<DataFrame<'_>, DataFrameError> {
    let (header_bytes, payload) = bytes
        .split_first_chunk::<DATA_FRAME_HEADER_BYTES>()
        .ok_or(DataFrameError::FrameTooShort)?;
    check_payload_size(payload.len())?;
    let header = decode_header(header_bytes)?;
    Ok(DataFrame { header, payload })
}

/// Собирает двоичную рамку из заголовка и груза.
///
/// # Errors
/// Пустой или слишком большой груз — те же правила, что при разборе.
pub fn encode_data_frame(
    header: &DataFrameHeader,
    payload: &[u8],
) -> Result<Vec<u8>, DataFrameError> {
    check_payload_size(payload.len())?;
    let mut frame = Vec::with_capacity(DATA_FRAME_HEADER_BYTES + payload.len());
    frame.extend_from_slice(&encode_header(header));
    frame.extend_from_slice(payload);
    Ok(frame)
}

/// Груз от 1 байта до лимита.
fn check_payload_size(payload_len: usize) -> Result<(), DataFrameError> {
    if payload_len == 0 {
        Err(DataFrameError::PayloadEmpty)
    } else if payload_len > MAX_DATA_FRAME_PAYLOAD_BYTES {
        Err(DataFrameError::PayloadTooLarge)
    } else {
        Ok(())
    }
}

/// Разбирает 24 байта заголовка.
fn decode_header(
    header: &[u8; DATA_FRAME_HEADER_BYTES],
) -> Result<DataFrameHeader, DataFrameError> {
    let [kind, flags, reserved_low, reserved_high, ..] = *header;
    if flags != 0 || reserved_low != 0 || reserved_high != 0 {
        return Err(DataFrameError::ReservedBitsSet);
    }
    match kind {
        SOURCE_BUFFER_BYTES_KIND => decode_source_buffer_header(header),
        RANGE_BYTES_KIND => decode_range_header(header),
        _ => Err(DataFrameError::UnknownFrameKind),
    }
}

/// Поля вида 1 (байты SourceBuffer).
fn decode_source_buffer_header(
    header: &[u8; DATA_FRAME_HEADER_BYTES],
) -> Result<DataFrameHeader, DataFrameError> {
    if read_u16(header, 10) != 0 || read_u32(header, 12) != 0 {
        return Err(DataFrameError::ReservedBitsSet);
    }
    let generation =
        StreamGeneration::new(read_u32(header, 4)).map_err(|_| DataFrameError::NumberOutOfRange)?;
    let source_buffer = SourceBufferIndex::new(read_u16(header, 8))
        .map_err(|_| DataFrameError::NumberOutOfRange)?;
    Ok(DataFrameHeader::SourceBufferBytes(
        SourceBufferBytesHeader {
            generation,
            source_buffer,
            sequence: AppendSequence(read_u64(header, 16)),
        },
    ))
}

/// Поля вида 2 (байты файла `<video src>`).
fn decode_range_header(
    header: &[u8; DATA_FRAME_HEADER_BYTES],
) -> Result<DataFrameHeader, DataFrameError> {
    if read_u64(header, 16) != 0 {
        return Err(DataFrameError::ReservedBitsSet);
    }
    let offset =
        ByteOffset::new(read_u64(header, 8)).map_err(|_| DataFrameError::NumberOutOfRange)?;
    Ok(DataFrameHeader::RangeBytes(RangeBytesHeader {
        request_id: RangeRequestId(read_u32(header, 4)),
        offset,
    }))
}

/// Записывает 24 байта заголовка; резервные байты остаются нулями.
fn encode_header(header: &DataFrameHeader) -> [u8; DATA_FRAME_HEADER_BYTES] {
    let mut bytes = [0_u8; DATA_FRAME_HEADER_BYTES];
    match header {
        DataFrameHeader::SourceBufferBytes(fields) => {
            bytes[0] = SOURCE_BUFFER_BYTES_KIND;
            bytes[4..8].copy_from_slice(&fields.generation.get().to_le_bytes());
            bytes[8..10].copy_from_slice(&fields.source_buffer.get().to_le_bytes());
            bytes[16..24].copy_from_slice(&fields.sequence.0.to_le_bytes());
        }
        DataFrameHeader::RangeBytes(fields) => {
            bytes[0] = RANGE_BYTES_KIND;
            bytes[4..8].copy_from_slice(&fields.request_id.0.to_le_bytes());
            bytes[8..16].copy_from_slice(&fields.offset.as_bytes().to_le_bytes());
        }
    }
    bytes
}

/// Читает `N` байт заголовка с позиции `offset`.
///
/// Все смещения — константы этого файла внутри 24 байт, поэтому выход за
/// границу невозможен; на всякий случай он даёт нули, а не панику.
fn read_array<const N: usize>(header: &[u8; DATA_FRAME_HEADER_BYTES], offset: usize) -> [u8; N] {
    header
        .get(offset..offset + N)
        .and_then(|field| <[u8; N]>::try_from(field).ok())
        .unwrap_or([0; N])
}

fn read_u16(header: &[u8; DATA_FRAME_HEADER_BYTES], offset: usize) -> u16 {
    u16::from_le_bytes(read_array(header, offset))
}

fn read_u32(header: &[u8; DATA_FRAME_HEADER_BYTES], offset: usize) -> u32 {
    u32::from_le_bytes(read_array(header, offset))
}

fn read_u64(header: &[u8; DATA_FRAME_HEADER_BYTES], offset: usize) -> u64 {
    u64::from_le_bytes(read_array(header, offset))
}
