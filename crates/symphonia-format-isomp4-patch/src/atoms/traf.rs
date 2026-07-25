// Symphonia
// Copyright (c) 2019-2026 The Project Symphonia Developers.
//
// This Source Code Form is subject to the terms of the Mozilla Public
// License, v. 2.0. If a copy of the MPL was not distributed with this
// file, You can obtain one at https://mozilla.org/MPL/2.0/.

use crate::atoms::{
    Atom, AtomHeader, AtomIterator, AtomType, ReadAtom, Result, TfdtAtom, TfhdAtom, TrunAtom,
    decode_error,
};

/// Track fragment atom.
#[allow(dead_code)]
#[derive(Debug)]
pub struct TrafAtom {
    /// Track fragment header.
    pub tfhd: TfhdAtom,
    /// Абсолютное decode-время первого sample этого track fragment.
    pub tfdt: TfdtAtom,
    /// Track fragment sample runs.
    pub truns: Vec<TrunAtom>,
    /// The total number of samples in this track fragment.
    pub total_sample_count: u32,
}

impl Atom for TrafAtom {
    fn read<R: ReadAtom>(it: &mut AtomIterator<R>, _header: &AtomHeader) -> Result<Self> {
        let mut tfhd = None;
        let mut tfdt = None;
        let mut truns = Vec::new();

        let mut total_sample_count = 0_u32;

        while let Some(header) = it.next_header()? {
            match header.atom_type {
                AtomType::TrackFragmentHeader => {
                    tfhd = Some(it.read_atom::<TfhdAtom>()?);
                }
                AtomType::TrackFragmentDecodeTime => {
                    if tfdt.is_some() {
                        return decode_error("isomp4 (traf): duplicate tfdt atom");
                    }
                    tfdt = Some(it.read_atom::<TfdtAtom>()?);
                }
                AtomType::TrackFragmentRun => {
                    let trun = it.read_atom::<TrunAtom>()?;

                    // Increment the total sample count.
                    total_sample_count =
                        total_sample_count.checked_add(trun.sample_count).ok_or({
                            crate::atoms::AtomError::Other(
                                symphonia_core::errors::Error::DecodeError(
                                    "isomp4 (traf): total sample count overflow",
                                ),
                            )
                        })?;

                    truns.push(trun);
                }
                _ => (),
            }
        }

        // Tfhd is mandatory.
        if tfhd.is_none() {
            return decode_error("isomp4 (traf): missing tfhd atom");
        }
        // W3C ISO BMFF byte-stream media segment требует ровно один `tfdt` в каждом `traf`.
        if tfdt.is_none() {
            return decode_error("isomp4 (traf): missing tfdt atom");
        }
        let tfhd = tfhd.unwrap();
        if truns.is_empty() && !tfhd.duration_is_empty {
            return decode_error("isomp4 (traf): missing trun atom");
        }
        if !truns.is_empty() && tfhd.duration_is_empty {
            return decode_error("isomp4 (traf): trun present for empty-duration fragment");
        }

        Ok(TrafAtom {
            tfhd,
            tfdt: tfdt.unwrap(),
            truns,
            total_sample_count,
        })
    }
}

#[cfg(test)]
mod tests;
