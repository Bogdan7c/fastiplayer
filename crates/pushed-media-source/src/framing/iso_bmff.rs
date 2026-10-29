//! Сканер fMP4 (ISO BMFF, W3C «ISO BMFF Byte Stream Format»).
//!
//! Верхний уровень потока — последовательность боксов `[размер][тип][тело]`.
//! Init-сегмент — `ftyp` (необязателен) + `moov`. Медиасегмент — `moof` + `mdat`.
//! Служебные боксы между ними (`styp`, `sidx`, `emsg`, `prft`, `free` …) демуксеру
//! не нужны и выбрасываются, не попадая в память целиком.

use bytes::{Buf, Bytes, BytesMut};

use super::{FramedUnit, FramingError, FramingLimits, SegmentKind, ensure_within_limit};

/// Длина обычного заголовка бокса: 4 байта размера + 4 байта типа.
const COMPACT_HEADER_BYTES: usize = 8;
/// Длина заголовка с 64-битным размером (`size == 1`): ещё 8 байтов.
const LARGE_HEADER_BYTES: usize = 16;
/// Значение 32-битного размера, означающее «размер в следующих 8 байтах».
const LARGE_SIZE_MARKER: u32 = 1;
/// Значение 32-битного размера, означающее «до конца файла».
const UNTIL_END_OF_FILE_MARKER: u32 = 0;

/// Заголовок одного бокса.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) struct IsoBoxHeader {
    /// Четырёхсимвольный тип (`moov`, `moof`, `mdat` …).
    pub(crate) box_type: [u8; 4],
    /// Длина заголовка: 8 или 16 байтов.
    pub(crate) header_len: usize,
    /// Полный размер бокса вместе с заголовком.
    pub(crate) total_len: u64,
}

/// Читает заголовок бокса в начале `bytes`; `None` — байтов на заголовок ещё мало.
pub(crate) fn read_box_header(bytes: &[u8]) -> Result<Option<IsoBoxHeader>, FramingError> {
    let Some(compact) = bytes.get(..COMPACT_HEADER_BYTES) else {
        return Ok(None);
    };
    let size_field = u32::from_be_bytes([compact[0], compact[1], compact[2], compact[3]]);
    let box_type = [compact[4], compact[5], compact[6], compact[7]];
    let (header_len, total_len) = match size_field {
        UNTIL_END_OF_FILE_MARKER => return Err(FramingError::BoxSizeUntilEndOfFile),
        LARGE_SIZE_MARKER => {
            let Some(large) = bytes.get(COMPACT_HEADER_BYTES..LARGE_HEADER_BYTES) else {
                return Ok(None);
            };
            let mut size_bytes = [0_u8; 8];
            size_bytes.copy_from_slice(large);
            (LARGE_HEADER_BYTES, u64::from_be_bytes(size_bytes))
        }
        compact_size => (COMPACT_HEADER_BYTES, u64::from(compact_size)),
    };
    if total_len < header_len as u64 {
        return Err(FramingError::BoxSizeTooSmall {
            declared: total_len,
        });
    }
    Ok(Some(IsoBoxHeader {
        box_type,
        header_len,
        total_len,
    }))
}

/// Роль бокса верхнего уровня в потоке MSE.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum TopLevelRole {
    /// `ftyp`: начало init-сегмента.
    FileType,
    /// `moov`: конец init-сегмента.
    Movie,
    /// `moof`: заголовок фрагмента.
    FragmentHeader,
    /// `mdat`: данные фрагмента.
    MediaData,
    /// Всё остальное: демуксеру не нужно.
    Ignored,
}

impl TopLevelRole {
    /// Роль по типу бокса.
    fn of(box_type: [u8; 4]) -> Self {
        match &box_type {
            b"ftyp" => Self::FileType,
            b"moov" => Self::Movie,
            b"moof" => Self::FragmentHeader,
            b"mdat" => Self::MediaData,
            _ => Self::Ignored,
        }
    }
}

/// Сканер одной полосы fMP4.
#[derive(Debug)]
pub(crate) struct IsoBmffFramer {
    /// Пределы размера сегментов.
    limits: FramingLimits,
    /// Принятые, но ещё не разобранные байты (начинаются с заголовка бокса).
    input: BytesMut,
    /// Сколько байтов ненужного бокса ещё пропустить, не сохраняя.
    skip_remaining: u64,
    /// `ftyp`, ждущий свой `moov`.
    pending_file_type: Option<Bytes>,
    /// `moof`, ждущий свой `mdat`.
    pending_fragment_header: Option<Bytes>,
}

impl IsoBmffFramer {
    /// Пустой сканер.
    pub(crate) fn new(limits: FramingLimits) -> Self {
        Self {
            limits,
            input: BytesMut::new(),
            skip_remaining: 0,
            pending_file_type: None,
            pending_fragment_header: None,
        }
    }

    /// Принимает кусок и выдаёт ставшие целыми сегменты.
    pub(crate) fn push(
        &mut self,
        chunk: &[u8],
        completed: &mut Vec<FramedUnit>,
    ) -> Result<(), FramingError> {
        self.input.extend_from_slice(chunk);
        loop {
            if !self.consume_skipped_bytes() {
                return Ok(());
            }
            let Some(header) = read_box_header(&self.input)? else {
                return Ok(());
            };
            let role = TopLevelRole::of(header.box_type);
            if role == TopLevelRole::Ignored {
                self.skip_box(header.total_len);
                continue;
            }
            self.ensure_segment_fits(role, header.total_len)?;
            let Some(whole_box) = self.take_whole_box(header.total_len) else {
                return Ok(());
            };
            self.accept_box(role, whole_box, completed)?;
        }
    }

    /// `abort()`: недособранный сегмент выбрасывается.
    pub(crate) fn reset(&mut self) {
        self.input.clear();
        self.skip_remaining = 0;
        self.pending_file_type = None;
        self.pending_fragment_header = None;
    }

    /// Конец потока: всё недособранное выбрасывается, возвращается его объём.
    pub(crate) fn finish(&mut self) -> usize {
        let dropped = self.held_bytes();
        self.reset();
        dropped
    }

    /// Сколько байтов удерживается до целого сегмента.
    pub(crate) fn held_bytes(&self) -> usize {
        self.input.len()
            + self.pending_file_type.as_ref().map_or(0, Bytes::len)
            + self.pending_fragment_header.as_ref().map_or(0, Bytes::len)
    }

    /// Пропускает уже пришедшие байты ненужного бокса. `false` — пропуск ещё не окончен.
    fn consume_skipped_bytes(&mut self) -> bool {
        if self.skip_remaining == 0 {
            return true;
        }
        let available = self.input.len() as u64;
        let skipped = self.skip_remaining.min(available);
        // `skipped` не больше длины буфера, значит помещается в usize.
        self.input
            .advance(usize::try_from(skipped).unwrap_or(self.input.len()));
        self.skip_remaining -= skipped;
        self.skip_remaining == 0
    }

    /// Начинает пропуск бокса целиком (он может прийти позже по частям).
    fn skip_box(&mut self, total_len: u64) {
        self.skip_remaining = total_len;
    }

    /// Проверяет предел сегмента ещё по заголовку — до того, как бокс накопится в памяти.
    fn ensure_segment_fits(&self, role: TopLevelRole, box_len: u64) -> Result<(), FramingError> {
        let pending_len = |pending: &Option<Bytes>| pending.as_ref().map_or(0, Bytes::len) as u64;
        match role {
            TopLevelRole::FileType => ensure_within_limit(
                SegmentKind::Initialization,
                box_len,
                self.limits.max_initialization_bytes,
            ),
            TopLevelRole::Movie => ensure_within_limit(
                SegmentKind::Initialization,
                box_len.saturating_add(pending_len(&self.pending_file_type)),
                self.limits.max_initialization_bytes,
            ),
            TopLevelRole::FragmentHeader => {
                ensure_within_limit(SegmentKind::Media, box_len, self.limits.max_media_bytes)
            }
            TopLevelRole::MediaData => ensure_within_limit(
                SegmentKind::Media,
                box_len.saturating_add(pending_len(&self.pending_fragment_header)),
                self.limits.max_media_bytes,
            ),
            TopLevelRole::Ignored => Ok(()),
        }
    }

    /// Забирает бокс из буфера, если он пришёл целиком.
    fn take_whole_box(&mut self, total_len: u64) -> Option<Bytes> {
        // Предел уже проверен, поэтому размер помещается в usize.
        let total_len = usize::try_from(total_len).ok()?;
        (self.input.len() >= total_len).then(|| self.input.split_to(total_len).freeze())
    }

    /// Раскладывает целый бокс по сегментам и проверяет порядок MSE.
    fn accept_box(
        &mut self,
        role: TopLevelRole,
        whole_box: Bytes,
        completed: &mut Vec<FramedUnit>,
    ) -> Result<(), FramingError> {
        let fragment_header_waits = self.pending_fragment_header.is_some();
        match role {
            TopLevelRole::FileType if fragment_header_waits => {
                Err(FramingError::FragmentHeaderWithoutMediaData)
            }
            TopLevelRole::FileType => {
                self.pending_file_type = Some(whole_box);
                Ok(())
            }
            TopLevelRole::Movie if fragment_header_waits => {
                Err(FramingError::FragmentHeaderWithoutMediaData)
            }
            TopLevelRole::Movie => {
                let initialization = concat(self.pending_file_type.take(), whole_box);
                completed.push(FramedUnit::Initialization(initialization));
                Ok(())
            }
            TopLevelRole::FragmentHeader if fragment_header_waits => {
                Err(FramingError::FragmentHeaderWithoutMediaData)
            }
            TopLevelRole::FragmentHeader => {
                self.pending_fragment_header = Some(whole_box);
                Ok(())
            }
            TopLevelRole::MediaData => {
                let fragment_header = self
                    .pending_fragment_header
                    .take()
                    .ok_or(FramingError::MediaDataWithoutFragmentHeader)?;
                completed.push(FramedUnit::Media(concat(Some(fragment_header), whole_box)));
                Ok(())
            }
            TopLevelRole::Ignored => Ok(()),
        }
    }
}

/// Склеивает необязательный префикс и бокс в один непрерывный сегмент.
fn concat(prefix: Option<Bytes>, tail: Bytes) -> Bytes {
    let Some(prefix) = prefix else {
        return tail;
    };
    let mut joined = BytesMut::with_capacity(prefix.len() + tail.len());
    joined.extend_from_slice(&prefix);
    joined.extend_from_slice(&tail);
    joined.freeze()
}

#[cfg(test)]
#[path = "iso_bmff_tests.rs"]
mod tests;
