// Symphonia
// Copyright (c) 2019-2026 The Project Symphonia Developers.
//
// This Source Code Form is subject to the terms of the Mozilla Public
// License, v. 2.0. If a copy of the MPL was not distributed with this
// file, You can obtain one at https://mozilla.org/MPL/2.0/.

use symphonia_core::util::bits;

use crate::atoms::{Atom, AtomHeader, AtomIterator, ReadAtom, Result, decode_error};

/// Track fragment run atom.
#[allow(dead_code)]
#[derive(Debug)]
pub struct TrunAtom {
    /// Extended header flags.
    flags: u32,
    /// Data offset of this run.
    pub data_offset: Option<i32>,
    /// Number of samples in this run.
    pub sample_count: u32,
    /// Sample flags for the first sample only.
    pub first_sample_flags: Option<u32>,
    /// Sample duration for each sample in this run.
    pub sample_duration: Vec<u32>,
    /// Sample size for each sample in this run.
    pub sample_size: Vec<u32>,
    /// Sample flags for each sample in this run.
    pub sample_flags: Vec<u32>,
    /// Sample composition offsets for each sample in this run.
    pub sample_composition_time_offset: Vec<i64>,
    /// The total size of all samples in this run. 0 if the sample size flag is not set.
    total_sample_size: u64,
    /// The total duration of all samples in this run. 0 if the sample duration flag is not set.
    total_sample_duration: u64,
}

impl TrunAtom {
    // Track fragment run atom flags.
    const DATA_OFFSET_PRESENT: u32 = 0x1;
    const FIRST_SAMPLE_FLAGS_PRESENT: u32 = 0x4;
    const SAMPLE_DURATION_PRESENT: u32 = 0x100;
    const SAMPLE_SIZE_PRESENT: u32 = 0x200;
    const SAMPLE_FLAGS_PRESENT: u32 = 0x400;
    const SAMPLE_COMPOSITION_TIME_OFFSETS_PRESENT: u32 = 0x800;

    /// Indicates if sample durations are provided.
    pub fn is_sample_duration_present(&self) -> bool {
        self.flags & TrunAtom::SAMPLE_DURATION_PRESENT != 0
    }

    /// Indicates if sample sizes are provided.
    pub fn is_sample_size_present(&self) -> bool {
        self.flags & TrunAtom::SAMPLE_SIZE_PRESENT != 0
    }

    /// Indicates if sample flags are provided.
    #[allow(dead_code)]
    pub fn are_sample_flags_present(&self) -> bool {
        self.flags & TrunAtom::SAMPLE_FLAGS_PRESENT != 0
    }

    /// Indicates if sample composition time offsets are provided.
    #[allow(dead_code)]
    pub fn are_sample_composition_time_offsets_present(&self) -> bool {
        self.flags & TrunAtom::SAMPLE_COMPOSITION_TIME_OFFSETS_PRESENT != 0
    }

    /// Gets the total duration of all samples.
    pub fn total_duration(&self, default_dur: u32) -> u64 {
        if self.is_sample_duration_present() {
            self.total_sample_duration
        } else {
            // Без per-sample duration все samples используют единый default из tfhd/trex.
            u64::from(self.sample_count) * u64::from(default_dur)
        }
    }

    /// Gets the total size of all samples.
    pub fn total_size(&self, default_size: u32) -> u64 {
        if self.is_sample_size_present() {
            self.total_sample_size
        } else {
            u64::from(self.sample_count) * u64::from(default_size)
        }
    }

    /// Get the timestamp and duration of a sample. The desired sample is specified by the
    /// trun-relative sample number, `sample_num_rel`.
    pub fn sample_timing(&self, sample_num_rel: u32, default_dur: u32) -> (u64, u32) {
        debug_assert!(sample_num_rel < self.sample_count);

        if self.is_sample_duration_present() {
            // All sample durations are unique.
            let ts = if sample_num_rel > 0 {
                self.sample_duration[..sample_num_rel as usize]
                    .iter()
                    .map(|&s| u64::from(s))
                    .sum::<u64>()
            } else {
                0
            };

            let dur = self.sample_duration[sample_num_rel as usize];

            (ts, dur)
        } else {
            // The duration of all samples in the track fragment are not unique.
            let ts = u64::from(sample_num_rel) * u64::from(default_dur);

            (ts, default_dur)
        }
    }

    /// Get the composition offset of a sample.
    pub fn sample_composition_offset(&self, sample_num_rel: u32) -> i64 {
        debug_assert!(sample_num_rel < self.sample_count);

        if self.are_sample_composition_time_offsets_present() {
            self.sample_composition_time_offset[sample_num_rel as usize]
        } else {
            0
        }
    }

    /// Возвращает effective ISO sample flags с приоритетом `trun` -> first -> default.
    pub fn effective_sample_flags(&self, sample_num_rel: u32, default_flags: u32) -> u32 {
        debug_assert!(sample_num_rel < self.sample_count);

        if self.are_sample_flags_present() {
            self.sample_flags[sample_num_rel as usize]
        } else if sample_num_rel == 0 {
            self.first_sample_flags.unwrap_or(default_flags)
        } else {
            default_flags
        }
    }

    /// Проверяет, доказывают ли effective flags random-access sample.
    pub fn is_proven_sync_sample(&self, sample_num_rel: u32, default_flags: u32) -> bool {
        const SAMPLE_IS_NON_SYNC_SAMPLE: u32 = 0x0001_0000;
        const SAMPLE_DEPENDS_ON_MASK: u32 = 0x0300_0000;
        const SAMPLE_DEPENDS_ON_NO_OTHERS: u32 = 0x0200_0000;

        let flags = self.effective_sample_flags(sample_num_rel, default_flags);
        flags & SAMPLE_IS_NON_SYNC_SAMPLE == 0
            && flags & SAMPLE_DEPENDS_ON_MASK == SAMPLE_DEPENDS_ON_NO_OTHERS
    }

    /// Находит ближайший доказанный sync sample не позднее указанного sample этого run-а.
    pub fn sync_sample_at_or_before(
        &self,
        sample_num_rel: u32,
        default_flags: u32,
    ) -> Option<u32> {
        if self.sample_count == 0 {
            return None;
        }

        let last_candidate = sample_num_rel.min(self.sample_count - 1);
        (0..=last_candidate)
            .rev()
            .find(|&candidate| self.is_proven_sync_sample(candidate, default_flags))
    }

    /// Get the size of a sample. The desired sample is specified by the trun-relative sample
    /// number, `sample_num_rel`.
    pub fn sample_size(&self, sample_num_rel: u32, default_size: u32) -> u32 {
        debug_assert!(sample_num_rel < self.sample_count);

        if self.is_sample_size_present() {
            self.sample_size[sample_num_rel as usize]
        } else {
            default_size
        }
    }

    /// Get the byte offset and size of a sample. The desired sample is specified by the
    /// trun-relative sample number, `sample_num_rel`.
    pub fn sample_offset(&self, sample_num_rel: u32, default_size: u32) -> (u64, u32) {
        debug_assert!(sample_num_rel < self.sample_count);

        if self.is_sample_size_present() {
            // All sample sizes are unique.
            let offset = if sample_num_rel > 0 {
                self.sample_size[..sample_num_rel as usize]
                    .iter()
                    .map(|&s| u64::from(s))
                    .sum::<u64>()
            } else {
                0
            };

            (offset, self.sample_size[sample_num_rel as usize])
        } else {
            // The size of all samples in the track are not unique.
            let offset = u64::from(sample_num_rel) * u64::from(default_size);

            (offset, default_size)
        }
    }

    /// Get the sample number (relative to the trun) of the sample that contains timestamp `ts`.
    pub fn ts_sample(&self, ts_rel: u64, default_dur: u32) -> u32 {
        let mut sample_num = 0;
        let mut ts_delta = ts_rel;

        if self.is_sample_duration_present() {
            // If the sample durations are present, then each sample duration is independently
            // stored. Sum sample durations until the delta is reached.
            for &dur in &self.sample_duration {
                if u64::from(dur) > ts_delta {
                    break;
                }

                ts_delta -= u64::from(dur);
                sample_num += 1;
            }
        } else {
            sample_num += ts_delta.checked_div(u64::from(default_dur)).unwrap_or(0) as u32;
        }

        sample_num
    }
}

#[cfg(test)]
mod tests;

impl Atom for TrunAtom {
    fn read<R: ReadAtom>(it: &mut AtomIterator<R>, _header: &AtomHeader) -> Result<Self> {
        let (version, flags) = it.read_extended_header()?;

        let sample_count = it.read_u32()?;

        let data_offset = match flags & TrunAtom::DATA_OFFSET_PRESENT {
            0 => None,
            _ => Some(bits::sign_extend_leq32_to_i32(it.read_u32()?, 32)),
        };

        let first_sample_flags = match flags & TrunAtom::FIRST_SAMPLE_FLAGS_PRESENT {
            0 => None,
            _ => Some(it.read_u32()?),
        };

        // If the first-sample-flags-present flag is set, then the sample-flags-present flag should
        // not be set. The samples after the first shall use the default sample flags defined in the
        // tfhd or mvex atoms.
        if first_sample_flags.is_some() && (flags & TrunAtom::SAMPLE_FLAGS_PRESENT != 0) {
            return decode_error(
                "isomp4: sample-flag-present and first-sample-flags-present flags are set",
            );
        }

        let mut sample_duration = Vec::new();
        let mut sample_size = Vec::new();
        let mut sample_flags = Vec::new();
        let mut sample_composition_time_offset = Vec::new();

        let mut total_sample_size = 0;
        let mut total_sample_duration = 0;

        for _ in 0..sample_count {
            if (flags & TrunAtom::SAMPLE_DURATION_PRESENT) != 0 {
                let duration = it.read_u32()?;
                total_sample_duration += u64::from(duration);
                sample_duration.push(duration);
            }

            if (flags & TrunAtom::SAMPLE_SIZE_PRESENT) != 0 {
                let size = it.read_u32()?;
                total_sample_size += u64::from(size);
                sample_size.push(size);
            }

            if (flags & TrunAtom::SAMPLE_FLAGS_PRESENT) != 0 {
                sample_flags.push(it.read_u32()?);
            }

            if (flags & TrunAtom::SAMPLE_COMPOSITION_TIME_OFFSETS_PRESENT) != 0 {
                let sample_offset = match version {
                    0 => i64::from(it.read_u32()?),
                    1 => i64::from(it.read_i32()?),
                    _ => return decode_error("isomp4 (trun): unsupported version"),
                };

                sample_composition_time_offset.push(sample_offset);
            }
        }

        Ok(TrunAtom {
            flags,
            data_offset,
            sample_count,
            first_sample_flags,
            sample_duration,
            sample_size,
            sample_flags,
            sample_composition_time_offset,
            total_sample_size,
            total_sample_duration,
        })
    }
}
