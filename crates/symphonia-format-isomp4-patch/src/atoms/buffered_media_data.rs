// Fastiplayer patch: `mdat` фрагмента целиком в памяти у источника без перемотки.
//
// This Source Code Form is subject to the terms of the Mozilla Public
// License, v. 2.0. If a copy of the MPL was not distributed with this
// file, You can obtain one at https://mozilla.org/MPL/2.0/.

//! Payload одного `mdat`, прочитанный целиком, — для источника, который читается только вперёд.
//!
//! **Зачем.** Читатель отдаёт пакеты по времени декодирования вперемешку между дорожками
//! (кадр видео, кадр звука, кадр видео, …) — так их ждёт плеер. А внутри `mdat`
//! фрагмента дорожки обычно лежат длинными прогонами: ffmpeg (и MSE-страницы с одним
//! SourceBuffer «видео + звук») кладут сначала все кадры видео фрагмента, потом весь звук.
//! Значит, после первого кадра звука из конца `mdat` читателю надо вернуться назад к
//! второму кадру видео. У перематываемого источника это обычная перемотка, а у источника
//! «только вперёд» (труба, ответ HTTP 200, упорядоченные сегменты) назад можно вернуться
//! лишь в пределах кольцевого буфера `MediaSourceStream` (64 КиБ по умолчанию). Прогон
//! видео длиннее — и чтение падает «out-of-bounds seek for a non-seekable stream».
//!
//! **Решение.** Для такого фрагмента (источник без перемотки и кадры больше чем одной
//! дорожки в фрагменте) payload `mdat` читается в память один раз, по порядку байтов, а
//! кадры вырезаются из памяти в прежнем порядке времени. Порядок пакетов тот же, что у
//! перематываемого источника; память — один `mdat`, она отпускается на следующем атоме.
//! Перематываемых источников (локальные файлы, HTTP Range) это не касается.
//!
//! **Обрыв.** Если поток кончился внутри `mdat`, в памяти остаётся то, что успело прийти:
//! кадры из этой части отдаются, кадр за обрывом — прежняя структурная ошибка
//! «unexpected end of atom» (как и при чтении без буфера), а не тихий конец потока.

use std::io::{self, Read};

/// Наибольший `mdat`, который читатель без перемотки держит целиком в памяти.
///
/// Фрагмент MSE/CMAF — это 1–10 секунд медиа: 4K60 H.264 на 40 Мбит/с за 10 с весит
/// ~50 МБ, обычные фрагменты — сотни КиБ…единицы МиБ. 64 МиБ — с запасом выше реальных
/// фрагментов, но лживый заголовок «`mdat` на 4 ГиБ» не заставит держать гигабайты: такой
/// `mdat` читается по-старому (без буфера), и если дорожки в нём лежат далеко друг от
/// друга, чтение честно падает прежней ошибкой.
pub(crate) const FORWARD_ONLY_MEDIA_DATA_BUFFER_LIMIT: u64 = 64 * 1024 * 1024;

/// Payload одного `mdat` в памяти.
#[derive(Debug)]
pub(crate) struct BufferedMediaData {
    /// Абсолютная позиция первого байта payload-а во входе читателя.
    payload_start: u64,
    /// Пришедшие байты payload-а (короче объявленного — при обрыве потока).
    payload: Vec<u8>,
    /// Объявленный заголовком размер payload-а.
    declared_len: u64,
}

/// Где лежит запрошенный диапазон байтов относительно буфера.
#[derive(Debug, PartialEq, Eq)]
pub(crate) enum BufferedSlice {
    /// Диапазон целиком в пришедших байтах — копия.
    Available(Box<[u8]>),
    /// Диапазон начинается внутри `mdat`, но заходит за обрыв потока.
    CutShort,
    /// Диапазон не целиком внутри этого `mdat` (читается обычным путём).
    Elsewhere,
}

impl BufferedMediaData {
    /// Читает до `declared_len` байтов payload-а, начинающегося в `payload_start`.
    ///
    /// Читатель должен стоять ровно на начале payload-а. Чтение растёт по мере прихода
    /// байтов (память не выделяется заранее под объявленный размер). Конец потока раньше
    /// объявленного размера — не ошибка здесь: пришедшая часть остаётся доступной, а кадр
    /// за обрывом получит структурную ошибку в [`Self::slice`]. Любая другая ошибка
    /// ввода-вывода (отмена, сбой источника) возвращается как есть.
    pub(crate) fn read_payload<R: Read>(
        reader: &mut R,
        payload_start: u64,
        declared_len: u64,
    ) -> io::Result<Self> {
        let mut payload = Vec::new();
        reader.take(declared_len).read_to_end(&mut payload)?;
        if (payload.len() as u64) < declared_len {
            log::debug!(
                "isomp4: forward-only mdat cut short: {} of {declared_len} bytes arrived",
                payload.len()
            );
        }
        Ok(Self {
            payload_start,
            payload,
            declared_len,
        })
    }

    /// Копия `len` байтов с абсолютной позиции `pos`.
    ///
    /// Диапазон, выходящий за объявленные границы `mdat`, — не наш (`Elsewhere`): его
    /// читает обычный путь, как без буфера. Внутри объявленных границ, но за пришедшими
    /// байтами — обрыв потока (`CutShort`).
    pub(crate) fn slice(&self, pos: u64, len: usize) -> BufferedSlice {
        let Some(offset) = pos.checked_sub(self.payload_start) else {
            return BufferedSlice::Elsewhere;
        };
        let Some(end) = u64::try_from(len)
            .ok()
            .and_then(|len| offset.checked_add(len))
        else {
            return BufferedSlice::Elsewhere;
        };
        if end > self.declared_len {
            return BufferedSlice::Elsewhere;
        }
        // `end` ≤ объявленного размера ≤ лимита буфера, поэтому помещается в `usize`.
        match (usize::try_from(offset), usize::try_from(end)) {
            (Ok(offset), Ok(end)) if end <= self.payload.len() => {
                BufferedSlice::Available(self.payload[offset..end].into())
            }
            _ => BufferedSlice::CutShort,
        }
    }
}

#[cfg(test)]
mod tests {
    use std::io::Cursor;

    use super::*;

    /// Payload из `len` байтов со значением «номер байта по модулю 251».
    fn numbered_payload(len: usize) -> Vec<u8> {
        (0..len).map(|index| (index % 251) as u8).collect()
    }

    #[test]
    fn slices_come_from_memory_in_any_order() {
        let payload = numbered_payload(1_000);
        let mut reader = Cursor::new(payload.clone());
        let buffered = BufferedMediaData::read_payload(&mut reader, 5_000, 1_000).unwrap();

        // Сначала конец (звук), потом начало (видео) — назад без перемотки источника.
        assert_eq!(
            buffered.slice(5_900, 50),
            BufferedSlice::Available(payload[900..950].into())
        );
        assert_eq!(
            buffered.slice(5_010, 20),
            BufferedSlice::Available(payload[10..30].into())
        );
        assert_eq!(
            reader.position(),
            1_000,
            "the reader stands at the mdat end"
        );
    }

    #[test]
    fn reads_only_the_declared_payload() {
        let mut reader = Cursor::new(numbered_payload(300));
        let buffered = BufferedMediaData::read_payload(&mut reader, 0, 200).unwrap();

        assert_eq!(reader.position(), 200, "the next atom stays unread");
        // Кадр, вылезающий за объявленный `mdat`, — не из памяти (как без буфера).
        assert_eq!(buffered.slice(150, 51), BufferedSlice::Elsewhere);
        assert!(matches!(
            buffered.slice(150, 50),
            BufferedSlice::Available(_)
        ));
    }

    #[test]
    fn a_stream_cut_inside_the_mdat_keeps_what_arrived() {
        let payload = numbered_payload(120);
        let mut reader = Cursor::new(payload.clone());
        let buffered = BufferedMediaData::read_payload(&mut reader, 100, 400).unwrap();

        assert_eq!(
            buffered.slice(100, 120),
            BufferedSlice::Available(payload.into())
        );
        assert_eq!(buffered.slice(200, 40), BufferedSlice::CutShort);
    }

    #[test]
    fn ranges_outside_the_mdat_are_not_served_from_memory() {
        let mut reader = Cursor::new(numbered_payload(100));
        let buffered = BufferedMediaData::read_payload(&mut reader, 1_000, 100).unwrap();

        assert_eq!(buffered.slice(999, 1), BufferedSlice::Elsewhere);
        assert_eq!(buffered.slice(1_100, 1), BufferedSlice::Elsewhere);
        assert_eq!(buffered.slice(1_101, 1), BufferedSlice::Elsewhere);
        assert_eq!(buffered.slice(u64::MAX, 1), BufferedSlice::Elsewhere);
    }

    #[test]
    fn source_failures_are_returned_not_swallowed() {
        struct FailingSource;
        impl Read for FailingSource {
            fn read(&mut self, _destination: &mut [u8]) -> io::Result<usize> {
                Err(io::Error::other("source went away"))
            }
        }

        let error = BufferedMediaData::read_payload(&mut FailingSource, 0, 10).unwrap_err();
        assert_eq!(error.to_string(), "source went away");
    }
}
