// Symphonia
// Copyright (c) 2019-2026 The Project Symphonia Developers.
//
// This Source Code Form is subject to the terms of the Mozilla Public
// License, v. 2.0. If a copy of the MPL was not distributed with this
// file, You can obtain one at https://mozilla.org/MPL/2.0/.

use crate::atoms::limits::*;
use crate::atoms::{Atom, AtomHeader, AtomIterator, ReadAtom, Result, decode_error};

#[derive(Debug)]
pub struct SampleCompositionOffsetEntry {
    pub sample_count: u32,
    pub sample_offset: i64,
}

/// Composition time atom.
#[allow(dead_code)]
#[derive(Debug)]
pub struct CttsAtom {
    pub entries: Vec<SampleCompositionOffsetEntry>,
}

impl CttsAtom {
    /// Get the composition offset for the sample indicated by `sample_num`.
    pub fn find_offset_for_sample(&self, sample_num: u32) -> Option<i64> {
        let mut next_entry_first_sample = 0;

        for entry in &self.entries {
            next_entry_first_sample += entry.sample_count;

            if sample_num < next_entry_first_sample {
                return Some(entry.sample_offset);
            }
        }

        None
    }
}

impl Atom for CttsAtom {
    fn read<R: ReadAtom>(it: &mut AtomIterator<R>, _header: &AtomHeader) -> Result<Self> {
        let (version, _) = it.read_extended_header()?;

        let entry_count = it.read_u32()?;

        let mut entries = Vec::with_capacity(MAX_TABLE_INITIAL_CAPACITY.min(entry_count as usize));

        for _ in 0..entry_count {
            let sample_count = it.read_u32()?;
            let sample_offset = match version {
                0 => i64::from(it.read_u32()?),
                1 => i64::from(it.read_i32()?),
                _ => return decode_error("isomp4 (ctts): unsupported version"),
            };

            entries.push(SampleCompositionOffsetEntry {
                sample_count,
                sample_offset,
            });
        }

        Ok(CttsAtom { entries })
    }
}
