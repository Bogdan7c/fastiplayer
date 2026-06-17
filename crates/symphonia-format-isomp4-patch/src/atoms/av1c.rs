// Symphonia
// Copyright (c) 2019-2026 The Project Symphonia Developers.
//
// This Source Code Form is subject to the terms of the Mozilla Public
// License, v. 2.0. If a copy of the MPL was not distributed with this
// file, You can obtain one at https://mozilla.org/MPL/2.0/.

use symphonia_core::codecs::CodecProfile;
use symphonia_core::codecs::video::VideoExtraData;
use symphonia_core::codecs::video::well_known::CODEC_ID_AV1;
use symphonia_core::codecs::video::well_known::extra_data::VIDEO_EXTRA_DATA_ID_AV1_DECODER_CONFIG;
use symphonia_core::codecs::video::well_known::profiles::{
    CODEC_PROFILE_AV1_HIGH, CODEC_PROFILE_AV1_MAIN, CODEC_PROFILE_AV1_PROFESSIONAL,
};

use crate::atoms::stsd::VisualSampleEntry;
use crate::atoms::{Atom, AtomHeader, AtomIterator, ReadAtom, Result, decode_error};

#[derive(Debug)]
pub struct Av1CAtom {
    /// AV1 extra data (AV1CodecConfigurationRecord, including config OBUs).
    extra_data: VideoExtraData,
    profile: CodecProfile,
    level: u32,
}

impl Atom for Av1CAtom {
    fn read<R: ReadAtom>(it: &mut AtomIterator<R>, header: &AtomHeader) -> Result<Self> {
        // The av1C atom payload is a single AV1CodecConfigurationRecord (ISOBMFF AV1 binding). It
        // carries the configuration plus optional sequence-header config OBUs and forms the codec
        // extra data passed to the decoder. Cap the size defensively.
        const MAX_AV1C_ATOM_SIZE: u64 = 4 * 1024;

        let len = match header.data_size() {
            Some(len) if len >= 4 && len <= MAX_AV1C_ATOM_SIZE => len as usize,
            Some(len) if len < 4 => {
                return decode_error("isomp4 (av1C): atom is too small");
            }
            Some(_) => {
                return decode_error("isomp4 (av1C): atom size is greater than 4 kb");
            }
            None => {
                return decode_error("isomp4 (av1C): expected atom size to be known");
            }
        };

        let extra_data = VideoExtraData {
            id: VIDEO_EXTRA_DATA_ID_AV1_DECODER_CONFIG,
            data: it.read_boxed_slice_exact(len)?,
        };

        // AV1CodecConfigurationRecord layout:
        //   byte 0: marker(1) | version(7)            -> must be 0b1_0000001 (0x81)
        //   byte 1: seq_profile(3) | seq_level_idx_0(5)
        let marker = extra_data.data[0] >> 7;
        let version = extra_data.data[0] & 0x7f;
        if marker != 1 || version != 1 {
            return decode_error("isomp4 (av1C): unsupported configuration record version");
        }

        let seq_profile = extra_data.data[1] >> 5;
        let level = u32::from(extra_data.data[1] & 0x1f);

        let profile = match seq_profile {
            0 => CODEC_PROFILE_AV1_MAIN,
            1 => CODEC_PROFILE_AV1_HIGH,
            2 => CODEC_PROFILE_AV1_PROFESSIONAL,
            _ => return decode_error("isomp4 (av1C): reserved seq_profile"),
        };

        Ok(Self { extra_data, profile, level })
    }
}

impl Av1CAtom {
    pub fn fill_video_sample_entry(self, entry: &mut VisualSampleEntry) {
        entry.codec_id = CODEC_ID_AV1;
        entry.profile = Some(self.profile);
        entry.level = Some(self.level);
        entry.extra_data.push(self.extra_data);
    }
}
