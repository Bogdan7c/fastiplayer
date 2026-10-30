//! Проверка раскладки обычного MP4 до открытия демуксером.
//!
//! Зачем. Обычный (не фрагментированный) MP4 хранит оглавление `moov` либо в
//! начале (faststart), либо в конце файла. Демуксер без перемотки конец файла не
//! достанет и зависнет, читая весь фильм подряд; а зашифрованный (CENC) MP4 надо
//! отвергнуть честно, а не показать мусор. Оба вопроса решает обход коробок
//! верхнего уровня: заголовки читаются запросами диапазонов (страница отдаёт
//! диапазоны всегда), `moov` читается целиком и проверяется на `encv`/`enca`.
//!
//! Не MP4 (WebM и прочее) обход пропускает: решает демуксер.

use source_core::{ByteSource, CancellationToken, SourceError};

use super::error::{PushedFileError, UnsupportedFileLayout};
use super::source::RangeFileSource;
use super::state::ResourceSize;
use crate::container::PushedContainer;
use crate::protection::{ProtectionVerdict, scan_initialization};

/// Заголовок коробки без расширенного размера.
const BOX_HEADER_BYTES: usize = 8;
/// Заголовок с 64-битным размером.
const LARGE_BOX_HEADER_BYTES: usize = 16;
/// Типы коробок, с которых начинается обычный MP4 или QuickTime-файл.
const MP4_FIRST_BOX_TYPES: [&[u8; 4]; 6] = [b"ftyp", b"moov", b"mdat", b"free", b"skip", b"wide"];

/// Сколько коробок верхнего уровня обход смотрит до `moov`. У настоящих файлов их
/// единицы (`ftyp`, `free`, `mdat`, `moov`); каждая коробка — запрос к странице, и
/// бесконечная цепочка 8-байтовых `free` не должна давать запрос на коробку вечно.
const MAX_TOP_LEVEL_BOXES: usize = 256;

/// Почему проверка не дошла до вердикта.
#[derive(Debug)]
pub(super) enum LayoutProblem {
    /// Чтение файла не удалось (отмена, ошибка файла): причину знает ядро.
    Read(SourceError),
    /// Файл кончился посреди коробки и страница ещё могла его принести — либо
    /// страница ушла: решает открывальщик по состоянию ядра.
    EndedEarly,
    /// Вердикт по раскладке или содержимому.
    Verdict(PushedFileError),
}

/// Обходит коробки верхнего уровня MP4 и ищет `moov`.
///
/// `size` — размер файла, который сообщила страница: с неизвестным размером `mdat`
/// перед `moov` означает «оглавление в конце» — отказ.
pub(super) fn check_movie_layout(
    source: &mut RangeFileSource,
    size: ResourceSize,
    max_movie_bytes: u32,
    cancellation: &CancellationToken,
) -> Result<(), LayoutProblem> {
    let mut offset = 0_u64;
    for _ in 0..MAX_TOP_LEVEL_BOXES {
        if let ResourceSize::Known(size) = size
            && offset >= size
        {
            // Дошли до конца без `moov`: пусть скажет демуксер.
            return Ok(());
        }
        let header = read_box_header(source, offset, size, cancellation)?;
        if offset == 0 && !MP4_FIRST_BOX_TYPES.contains(&&header.box_type) {
            return Ok(());
        }
        match &header.box_type {
            b"moov" => {
                return check_movie_box(
                    source,
                    offset,
                    &header,
                    size,
                    max_movie_bytes,
                    cancellation,
                );
            }
            b"mdat" if matches!(size, ResourceSize::Unknown) => {
                return Err(LayoutProblem::Verdict(PushedFileError::UnsupportedLayout {
                    layout: UnsupportedFileLayout::MovieAtEndWithoutKnownSize,
                }));
            }
            _ => {}
        }
        // Коробка «до конца файла» (`size == 0`) — последняя.
        let Some(length) = header.length else {
            return Ok(());
        };
        // Переполнение — не «дошли до конца»: такая длина коробки бессмысленна.
        offset = offset.checked_add(length).ok_or_else(|| {
            LayoutProblem::Verdict(PushedFileError::UnparsableMedia {
                reason: "an MP4 box length overflows the file offset".to_owned(),
            })
        })?;
    }
    Err(LayoutProblem::Verdict(PushedFileError::UnparsableMedia {
        reason: format!("more than {MAX_TOP_LEVEL_BOXES} top-level MP4 boxes before the movie box"),
    }))
}

/// Заголовок коробки.
struct BoxHeader {
    /// Четыре буквы типа.
    box_type: [u8; 4],
    /// Полная длина вместе с заголовком; `None` — «до конца файла».
    length: Option<u64>,
}

/// Читает заголовок коробки по смещению.
fn read_box_header(
    source: &mut RangeFileSource,
    offset: u64,
    size: ResourceSize,
    cancellation: &CancellationToken,
) -> Result<BoxHeader, LayoutProblem> {
    let mut short = [0_u8; BOX_HEADER_BYTES];
    read_exact_at(source, offset, &mut short, cancellation)?;
    let declared = u32::from_be_bytes([short[0], short[1], short[2], short[3]]);
    let box_type = [short[4], short[5], short[6], short[7]];
    let (length, header_bytes) = match declared {
        0 => (None, BOX_HEADER_BYTES as u64),
        1 => {
            let mut large = [0_u8; LARGE_BOX_HEADER_BYTES - BOX_HEADER_BYTES];
            read_exact_at(
                source,
                offset + BOX_HEADER_BYTES as u64,
                &mut large,
                cancellation,
            )?;
            (
                Some(u64::from_be_bytes(large)),
                LARGE_BOX_HEADER_BYTES as u64,
            )
        }
        declared => (Some(u64::from(declared)), BOX_HEADER_BYTES as u64),
    };
    if let Some(length) = length
        && length < header_bytes
    {
        return Err(LayoutProblem::Verdict(PushedFileError::UnparsableMedia {
            reason: "an MP4 box is shorter than its own header".to_owned(),
        }));
    }
    // Коробка «до конца файла» при известном размере — длина известна.
    let length = length.or(match size {
        ResourceSize::Known(size) => Some(size.saturating_sub(offset)),
        ResourceSize::Unset | ResourceSize::Unknown => None,
    });
    Ok(BoxHeader { box_type, length })
}

/// Читает `moov` целиком и проверяет его на шифрование.
fn check_movie_box(
    source: &mut RangeFileSource,
    offset: u64,
    header: &BoxHeader,
    size: ResourceSize,
    max_movie_bytes: u32,
    cancellation: &CancellationToken,
) -> Result<(), LayoutProblem> {
    let Some(length) = header.length else {
        // `moov` «до конца файла» при неизвестном размере: границ нет — пусть решает демуксер.
        return Ok(());
    };
    if let ResourceSize::Known(size) = size
        && length > size.saturating_sub(offset)
    {
        // Заявленный `moov` длиннее остатка файла: не выделяем под него память.
        return Err(LayoutProblem::Verdict(PushedFileError::UnparsableMedia {
            reason: "the MP4 movie box is declared beyond the end of the file".to_owned(),
        }));
    }
    if length > u64::from(max_movie_bytes) {
        return Err(LayoutProblem::Verdict(PushedFileError::UnsupportedLayout {
            layout: UnsupportedFileLayout::MovieTooLarge {
                limit: u64::from(max_movie_bytes),
            },
        }));
    }
    let length = usize::try_from(length).map_err(|_| {
        LayoutProblem::Verdict(PushedFileError::UnsupportedLayout {
            layout: UnsupportedFileLayout::MovieTooLarge {
                limit: u64::from(max_movie_bytes),
            },
        })
    })?;
    let mut movie = vec![0_u8; length];
    read_exact_at(source, offset, &mut movie, cancellation)?;
    match scan_initialization(PushedContainer::FragmentedMp4, &movie) {
        Ok(ProtectionVerdict::Clear) => Ok(()),
        Ok(ProtectionVerdict::Protected) => {
            Err(LayoutProblem::Verdict(PushedFileError::ProtectedContent))
        }
        Err(_) => Err(LayoutProblem::Verdict(PushedFileError::UnparsableMedia {
            reason: "the MP4 movie box structure is inconsistent".to_owned(),
        })),
    }
}

/// Читает ровно `buffer.len()` байтов с `offset`.
fn read_exact_at(
    source: &mut RangeFileSource,
    offset: u64,
    buffer: &mut [u8],
    cancellation: &CancellationToken,
) -> Result<(), LayoutProblem> {
    source.move_to(offset);
    let mut filled = 0;
    while filled < buffer.len() {
        match source.read(&mut buffer[filled..], cancellation) {
            Ok(0) => return Err(LayoutProblem::EndedEarly),
            Ok(count) => filled += count,
            Err(error) => return Err(LayoutProblem::Read(error)),
        }
    }
    Ok(())
}
