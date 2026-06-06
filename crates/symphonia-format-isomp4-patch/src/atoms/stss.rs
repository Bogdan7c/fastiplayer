// Symphonia
// Copyright (c) 2019-2026 The Project Symphonia Developers.
//
// This Source Code Form is subject to the terms of the Mozilla Public
// License, v. 2.0. If a copy of the MPL was not distributed with this
// file, You can obtain one at https://mozilla.org/MPL/2.0/.

use crate::atoms::limits::*;
use crate::atoms::{Atom, AtomHeader, AtomIterator, ReadAtom, Result};

#[allow(dead_code)]
#[derive(Debug)]
pub struct StssAtom {
    /// 1-based номера sync samples ровно в формате MP4 `stss`.
    pub sample_numbers: Vec<u32>,
}

impl StssAtom {
    /// Возвращает zero-based sync sample не позже `sample_num`.
    pub fn find_sync_sample_for_sample(&self, sample_num: u32) -> Option<u32> {
        // MP4 хранит sample numbers как 1-based, а внутренние таблицы Symphonia используют
        // zero-based индексы. Держим конвертацию внутри владельца `stss`, чтобы callers не
        // копировали это знание и не ошибались на границе форматов.
        let target_sample_number = sample_num.checked_add(1)?;

        self.sample_numbers
            .iter()
            .copied()
            .filter(|&sample_number| sample_number <= target_sample_number)
            .filter_map(|sample_number| sample_number.checked_sub(1))
            .max()
    }
}

impl Atom for StssAtom {
    fn read<R: ReadAtom>(it: &mut AtomIterator<R>, _header: &AtomHeader) -> Result<Self> {
        let (_, _) = it.read_extended_header()?;

        let entry_count = it.read_u32()?;
        let mut sample_numbers =
            Vec::with_capacity(MAX_TABLE_INITIAL_CAPACITY.min(entry_count as usize));

        for _ in 0..entry_count {
            sample_numbers.push(it.read_u32()?);
        }

        Ok(StssAtom { sample_numbers })
    }
}

#[cfg(test)]
mod tests {
    use super::StssAtom;

    fn sync_samples(sample_numbers: &[u32]) -> StssAtom {
        StssAtom {
            sample_numbers: sample_numbers.to_vec(),
        }
    }

    #[test]
    fn finds_exact_sync_sample() {
        let stss = sync_samples(&[1, 29, 57]);

        assert_eq!(stss.find_sync_sample_for_sample(28), Some(28));
    }

    #[test]
    fn falls_back_to_previous_sync_sample() {
        let stss = sync_samples(&[1, 29, 57]);

        assert_eq!(stss.find_sync_sample_for_sample(42), Some(28));
    }

    #[test]
    fn returns_none_before_first_sync_sample() {
        let stss = sync_samples(&[29, 57]);

        assert_eq!(stss.find_sync_sample_for_sample(10), None);
    }

    #[test]
    fn ignores_invalid_zero_sample_number() {
        let stss = sync_samples(&[0, 29, 57]);

        assert_eq!(stss.find_sync_sample_for_sample(42), Some(28));
    }
}
