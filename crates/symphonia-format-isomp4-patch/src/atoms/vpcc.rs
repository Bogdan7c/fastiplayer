// Symphonia
// Copyright (c) 2019-2026 The Project Symphonia Developers.
//
// This Source Code Form is subject to the terms of the Mozilla Public
// License, v. 2.0. If a copy of the MPL was not distributed with this
// file, You can obtain one at https://mozilla.org/MPL/2.0/.

use symphonia_core::codecs::CodecProfile;
use symphonia_core::codecs::video::well_known::CODEC_ID_VP9;
use symphonia_core::codecs::video::well_known::profiles::{
    CODEC_PROFILE_VP9_0, CODEC_PROFILE_VP9_1, CODEC_PROFILE_VP9_2, CODEC_PROFILE_VP9_3,
};

use crate::atoms::stsd::VisualSampleEntry;
use crate::atoms::{Atom, AtomHeader, AtomIterator, ReadAtom, Result, decode_error};

/// `vpcC` (VP Codec Configuration Box) из «VP Codec ISO Media File Format Binding».
///
/// Нам нужны только `profile` и `level`: остальные поля (bit depth, chroma, цветовые
/// параметры) декодер получает из самого VP9 bitstream-а, а container metadata для них
/// приходит из `colr`/`mdcv`/`clli`.
#[derive(Debug)]
pub struct VpcCAtom {
    profile: CodecProfile,
    level: u32,
}

impl Atom for VpcCAtom {
    fn read<R: ReadAtom>(it: &mut AtomIterator<R>, _header: &AtomHeader) -> Result<Self> {
        // FullBox: version (8) + flags (24). Версия 1 — единственная, определённая спецификацией.
        let (version, _flags) = it.read_extended_header()?;
        if version != 1 {
            return decode_error("isomp4 (vpcC): unsupported configuration box version");
        }

        let profile = match it.read_u8()? {
            0 => CODEC_PROFILE_VP9_0,
            1 => CODEC_PROFILE_VP9_1,
            2 => CODEC_PROFILE_VP9_2,
            3 => CODEC_PROFILE_VP9_3,
            _ => return decode_error("isomp4 (vpcC): reserved VP9 profile"),
        };
        let level = u32::from(it.read_u8()?);

        // Хвост (bitDepth/chroma/цвет/codecInitializationData) намеренно не разбирается:
        // AtomIterator сам пропустит остаток атома.
        Ok(Self { profile, level })
    }
}

impl VpcCAtom {
    /// Переносит конфигурацию в sample entry; `vpcC` однозначно говорит, что это VP9.
    pub fn fill_video_sample_entry(self, entry: &mut VisualSampleEntry) {
        entry.codec_id = CODEC_ID_VP9;
        entry.profile = Some(self.profile);
        entry.level = Some(self.level);
    }
}
