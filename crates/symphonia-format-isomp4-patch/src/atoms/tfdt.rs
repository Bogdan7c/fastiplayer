// Symphonia
// Copyright (c) 2019-2026 The Project Symphonia Developers.
//
// This Source Code Form is subject to the terms of the Mozilla Public
// License, v. 2.0. If a copy of the MPL was not distributed with this
// file, You can obtain one at https://mozilla.org/MPL/2.0/.

use crate::atoms::{Atom, AtomHeader, AtomIterator, ReadAtom, Result, decode_error};

/// Абсолютное decode-время первого sample в track fragment.
#[derive(Debug)]
pub struct TfdtAtom {
    /// Время в media timescale соответствующего track-а.
    pub base_media_decode_time: u64,
}

impl Atom for TfdtAtom {
    fn read<R: ReadAtom>(it: &mut AtomIterator<R>, _header: &AtomHeader) -> Result<Self> {
        let (version, _) = it.read_extended_header()?;

        let base_media_decode_time = match version {
            0 => u64::from(it.read_u32()?),
            1 => it.read_u64()?,
            _ => return decode_error("isomp4 (tfdt): unsupported version"),
        };

        Ok(TfdtAtom {
            base_media_decode_time,
        })
    }
}

#[cfg(test)]
mod tests;
