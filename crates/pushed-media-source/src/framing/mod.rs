//! Сканер коробок: из кусков любой нарезки — целые init- и медиасегменты.
//!
//! Плеер страницы режет байты как угодно, иногда посреди бокса. Демуксеру же
//! нужны целые сегменты: только тогда `abort()` выбрасывает недособранный кусок,
//! не оставляя мусора в разборщике, а новый init-сегмент открывает новый эпизод
//! ровно на границе. Сканер читает только заголовки контейнера (размер и тип
//! бокса/элемента) и не разбирает кадры — это работа существующих демуксеров.
//!
//! Каждый контейнер сканируется своим модулем; здесь — общий словарь и выбор.

pub(crate) mod ebml;
mod iso_bmff;
mod matroska;
mod mpeg_ts;

use bytes::Bytes;

use crate::container::PushedContainer;

pub(crate) use ebml::{EbmlElementSize, read_element_header};
pub(crate) use iso_bmff::read_box_header;

use self::iso_bmff::IsoBmffFramer;
use self::matroska::MatroskaFramer;
use self::mpeg_ts::MpegTsFramer;

/// Целый сегмент, готовый для демуксера.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum FramedUnit {
    /// Init-сегмент: описание дорожек и кодеков.
    Initialization(Bytes),
    /// Медиасегмент: кадры одного фрагмента или кластера.
    Media(Bytes),
}

/// Пределы размера сегментов: страница — враг, память ограничена заранее.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) struct FramingLimits {
    /// Максимум байтов init-сегмента.
    pub(crate) max_initialization_bytes: usize,
    /// Максимум байтов одного медиасегмента.
    pub(crate) max_media_bytes: usize,
}

/// Какой предел нарушен.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SegmentKind {
    /// Init-сегмент.
    Initialization,
    /// Медиасегмент.
    Media,
}

/// Байты не складываются в сегменты объявленного контейнера.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum FramingError {
    /// Объявленный размер бокса меньше его собственного заголовка.
    #[error("an MP4 box declares {declared} bytes, less than its header")]
    BoxSizeTooSmall {
        /// Объявленный размер.
        declared: u64,
    },
    /// Размер 0 («до конца файла») в потоке без конца недопустим.
    #[error("an MP4 box declares size 0 (until end of file), which a stream cannot have")]
    BoxSizeUntilEndOfFile,
    /// Сегмент больше предела источника.
    #[error("a {kind:?} segment declares {declared} bytes, the limit is {limit}")]
    SegmentTooLarge {
        /// Какой сегмент.
        kind: SegmentKind,
        /// Объявленный (или накопленный) размер.
        declared: u64,
        /// Предел.
        limit: usize,
    },
    /// `mdat` без предшествующего `moof`.
    #[error("an MP4 media data box arrived without a fragment header")]
    MediaDataWithoutFragmentHeader,
    /// Новый `moof` или init пришёл, пока прошлый `moof` ждал свой `mdat`.
    #[error("an MP4 fragment header was not followed by its media data")]
    FragmentHeaderWithoutMediaData,
    /// Неверное число переменной длины в EBML.
    #[error("an invalid EBML variable-length number")]
    InvalidEbmlNumber,
    /// WebM-поток начинается не с EBML-заголовка.
    #[error("a WebM stream must start with an EBML header")]
    MissingEbmlHeader,
    /// После EBML-заголовка нет `Segment`.
    #[error("a WebM EBML header is not followed by a Segment")]
    MissingSegment,
    /// Элемент с неизвестным размером там, где он недопустим.
    #[error("the WebM element 0x{id:X} has an unknown size where it is not allowed")]
    UnknownSizeNotAllowed {
        /// ID элемента.
        id: u32,
    },
    /// Init-сегмент WebM без описания дорожек.
    #[error("a WebM initialization segment has no Tracks element")]
    MissingTracks,
    /// Поток MPEG-TS потерял синхробайт пакета.
    #[error("an MPEG-TS packet does not start with the sync byte")]
    TransportStreamSyncLost,
}

/// Сканер одной полосы; выбирается по контейнеру.
#[derive(Debug)]
pub(crate) enum ContainerFramer {
    /// fMP4.
    IsoBmff(IsoBmffFramer),
    /// WebM.
    Matroska(MatroskaFramer),
    /// MPEG-TS.
    MpegTs(MpegTsFramer),
}

impl ContainerFramer {
    /// Пустой сканер для контейнера.
    pub(crate) fn new(container: PushedContainer, limits: FramingLimits) -> Self {
        match container {
            PushedContainer::FragmentedMp4 => Self::IsoBmff(IsoBmffFramer::new(limits)),
            PushedContainer::WebM => Self::Matroska(MatroskaFramer::new(limits)),
            PushedContainer::MpegTs => Self::MpegTs(MpegTsFramer::new(limits)),
        }
    }

    /// Принимает очередной кусок и дописывает в `completed` все сегменты, ставшие целыми.
    pub(crate) fn push(
        &mut self,
        chunk: &[u8],
        completed: &mut Vec<FramedUnit>,
    ) -> Result<(), FramingError> {
        match self {
            Self::IsoBmff(framer) => framer.push(chunk, completed),
            Self::Matroska(framer) => framer.push(chunk, completed),
            Self::MpegTs(framer) => framer.push(chunk, completed),
        }
    }

    /// `abort()`: забыть недособранный сегмент; следующий кусок начинает сегмент заново.
    pub(crate) fn reset(&mut self) {
        match self {
            Self::IsoBmff(framer) => framer.reset(),
            Self::Matroska(framer) => framer.reset(),
            Self::MpegTs(framer) => framer.reset(),
        }
    }

    /// Конец потока: дописать то, что целым становится только с концом
    /// (кластер WebM неизвестного размера). Возвращает число выброшенных байтов
    /// недособранного сегмента — вызывающий обязан о них сообщить.
    pub(crate) fn finish(&mut self, completed: &mut Vec<FramedUnit>) -> usize {
        match self {
            Self::IsoBmff(framer) => framer.finish(),
            Self::Matroska(framer) => framer.finish(completed),
            Self::MpegTs(framer) => framer.finish(),
        }
    }

    /// Сколько байтов сейчас удерживает сканер (недособранные сегменты).
    pub(crate) fn held_bytes(&self) -> usize {
        match self {
            Self::IsoBmff(framer) => framer.held_bytes(),
            Self::Matroska(framer) => framer.held_bytes(),
            Self::MpegTs(framer) => framer.held_bytes(),
        }
    }
}

/// Проверяет предел размера сегмента.
fn ensure_within_limit(kind: SegmentKind, declared: u64, limit: usize) -> Result<(), FramingError> {
    let fits = usize::try_from(declared).is_ok_and(|declared| declared <= limit);
    if fits {
        Ok(())
    } else {
        Err(FramingError::SegmentTooLarge {
            kind,
            declared,
            limit,
        })
    }
}
