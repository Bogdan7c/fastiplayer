//! Поиск начала syncframe-а в произвольном куске байтового потока.

use crate::{SyncframeHeader, parse_syncframe_header};

/// Синхрослово, с которого начинается каждый syncframe AC-3 и E-AC-3.
pub const SYNC_WORD: [u8; 2] = [0x0B, 0x77];

/// Результат поиска syncframe-а.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SyncframeSearch {
    /// Найден разбираемый заголовок. Сам кадр может быть ещё не дочитан:
    /// он целиком доступен, только если `offset + header.frame_length_bytes()`
    /// не больше длины буфера.
    Found {
        /// Смещение синхрослова от начала переданного буфера.
        offset: usize,
        /// Разобранный заголовок.
        header: SyncframeHeader,
    },
    /// Пригодного заголовка в буфере пока нет.
    ///
    /// Первые `discardable_prefix_bytes` байт гарантированно не начинают
    /// syncframe и могут быть выброшены; хвост нужно сохранить и дополнить
    /// (там может лежать половина синхрослова или неполный заголовок).
    NeedMoreData {
        /// Сколько байт с начала буфера можно отбросить.
        discardable_prefix_bytes: usize,
    },
}

/// Ищет первый syncframe с разбираемым заголовком, пропуская мусор и ложные
/// синхрослова (байты `0x0B 0x77` внутри аудиоданных, после которых заголовок
/// не проходит проверку таблиц).
///
/// Функция ничего не запоминает между вызовами: потоковый потребитель
/// отбрасывает `discardable_prefix_bytes`, дочитывает данные и вызывает снова.
#[must_use]
pub fn locate_syncframe(bytes: &[u8]) -> SyncframeSearch {
    let mut candidate_offset = 0_usize;
    while let Some(relative_offset) = find_sync_word(&bytes[candidate_offset..]) {
        let offset = candidate_offset + relative_offset;
        match parse_syncframe_header(&bytes[offset..]) {
            Ok(header) => return SyncframeSearch::Found { offset, header },
            // Заголовок обрезан концом буфера: решение откладывается до дочитывания.
            Err(error) if error.is_truncation() => {
                return SyncframeSearch::NeedMoreData {
                    discardable_prefix_bytes: offset,
                };
            }
            // Ложное синхрослово: продолжаем со следующего байта.
            Err(_) => candidate_offset = offset + 1,
        }
    }
    SyncframeSearch::NeedMoreData {
        discardable_prefix_bytes: discardable_tail_start(bytes),
    }
}

fn find_sync_word(bytes: &[u8]) -> Option<usize> {
    bytes
        .windows(SYNC_WORD.len())
        .position(|window| window == SYNC_WORD)
}

/// Если буфер кончается первым байтом синхрослова, его нельзя выбрасывать.
fn discardable_tail_start(bytes: &[u8]) -> usize {
    if bytes.last() == Some(&SYNC_WORD[0]) {
        bytes.len() - 1
    } else {
        bytes.len()
    }
}
