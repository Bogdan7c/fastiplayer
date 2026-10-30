//! Чтение чисел из тела бокса/элемента с проверкой границ.
//!
//! Каждое чтение либо возвращает число, либо типизированную ошибку
//! `Truncated` с указанием структуры — индексации без проверки нет.

use super::{SegmentTimingError, TimingStructure};

/// Курсор по телу одной структуры; читает числа big-endian.
#[derive(Debug, Clone, Copy)]
pub(super) struct ByteCursor<'a> {
    /// Ещё не прочитанные байты.
    remaining: &'a [u8],
    /// Какая структура читается — попадает в ошибку.
    structure: TimingStructure,
}

impl<'a> ByteCursor<'a> {
    /// Курсор в начале тела.
    pub(super) fn new(body: &'a [u8], structure: TimingStructure) -> Self {
        Self {
            remaining: body,
            structure,
        }
    }

    /// Сколько байтов осталось.
    pub(super) fn remaining_len(&self) -> usize {
        self.remaining.len()
    }

    /// Ошибка «структура кончилась» для этой структуры.
    fn truncated(&self) -> SegmentTimingError {
        SegmentTimingError::Truncated {
            structure: self.structure,
        }
    }

    /// Забирает ровно `N` байтов.
    fn take_array<const N: usize>(&mut self) -> Result<[u8; N], SegmentTimingError> {
        let Some((head, tail)) = self.remaining.split_first_chunk::<N>() else {
            return Err(self.truncated());
        };
        self.remaining = tail;
        Ok(*head)
    }

    /// Пропускает `count` байтов.
    pub(super) fn skip(&mut self, count: usize) -> Result<(), SegmentTimingError> {
        let tail = self
            .remaining
            .get(count..)
            .ok_or_else(|| self.truncated())?;
        self.remaining = tail;
        Ok(())
    }

    /// Один байт.
    pub(super) fn read_u8(&mut self) -> Result<u8, SegmentTimingError> {
        self.take_array::<1>().map(|[byte]| byte)
    }

    /// Знаковое 16-битное число.
    pub(super) fn read_i16(&mut self) -> Result<i16, SegmentTimingError> {
        self.take_array().map(i16::from_be_bytes)
    }

    /// Беззнаковое 32-битное число.
    pub(super) fn read_u32(&mut self) -> Result<u32, SegmentTimingError> {
        self.take_array().map(u32::from_be_bytes)
    }

    /// Знаковое 32-битное число.
    pub(super) fn read_i32(&mut self) -> Result<i32, SegmentTimingError> {
        self.take_array().map(i32::from_be_bytes)
    }

    /// Беззнаковое 64-битное число.
    pub(super) fn read_u64(&mut self) -> Result<u64, SegmentTimingError> {
        self.take_array().map(u64::from_be_bytes)
    }

    /// Заголовок полного бокса ISO BMFF: версия (1 байт) и флаги (24 бита).
    pub(super) fn read_full_box_header(&mut self) -> Result<FullBoxHeader, SegmentTimingError> {
        let [version, flags_high, flags_mid, flags_low] = self.take_array::<4>()?;
        Ok(FullBoxHeader {
            version,
            flags: u32::from_be_bytes([0, flags_high, flags_mid, flags_low]),
        })
    }
}

/// Версия и флаги полного бокса.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) struct FullBoxHeader {
    /// Версия формата бокса.
    pub(super) version: u8,
    /// 24 бита флагов.
    pub(super) flags: u32,
}

impl FullBoxHeader {
    /// Установлен ли флаг.
    pub(super) fn has_flag(self, flag: u32) -> bool {
        self.flags & flag != 0
    }
}
