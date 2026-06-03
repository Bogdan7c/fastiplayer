// Symphonia
// Copyright (c) 2019-2026 The Project Symphonia Developers.
//
// This Source Code Form is subject to the terms of the Mozilla Public
// License, v. 2.0. If a copy of the MPL was not distributed with this
// file, You can obtain one at https://mozilla.org/MPL/2.0/.

use symphonia_core::util::bits;

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

fn decode_composition_offset(version: u8, raw_offset: u32) -> Result<i64> {
    match version {
        // Some MP4 muxers write signed two's-complement offsets into a version 0 ctts atom.
        // Treating high-bit values as plain u32 would move B-frames by 2^32 timebase units.
        0 => Ok(i64::from(bits::sign_extend_leq32_to_i32(raw_offset, 32))),
        1 => Ok(i64::from(bits::sign_extend_leq32_to_i32(raw_offset, 32))),
        _ => decode_error("isomp4 (ctts): unsupported version"),
    }
}

impl Atom for CttsAtom {
    fn read<R: ReadAtom>(it: &mut AtomIterator<R>, _header: &AtomHeader) -> Result<Self> {
        let (version, _) = it.read_extended_header()?;

        let entry_count = it.read_u32()?;

        let mut entries = Vec::with_capacity(MAX_TABLE_INITIAL_CAPACITY.min(entry_count as usize));

        for _ in 0..entry_count {
            let sample_count = it.read_u32()?;
            let sample_offset = decode_composition_offset(version, it.read_u32()?)?;

            entries.push(SampleCompositionOffsetEntry {
                sample_count,
                sample_offset,
            });
        }

        Ok(CttsAtom { entries })
    }
}

#[cfg(test)]
mod tests {
    use super::decode_composition_offset;

    #[test]
    fn ctts_v0_accepts_twos_complement_negative_offsets() {
        assert!(matches!(
            decode_composition_offset(0, 0xffff_fc17),
            Ok(-1001)
        ));
    }

    #[test]
    fn ctts_v0_keeps_small_positive_offsets() {
        assert!(matches!(
            decode_composition_offset(0, 0x0000_07d2),
            Ok(2002)
        ));
    }

    #[test]
    fn ctts_v1_reads_signed_offsets() {
        assert!(matches!(
            decode_composition_offset(1, 0xffff_fc17),
            Ok(-1001)
        ));
    }
}
