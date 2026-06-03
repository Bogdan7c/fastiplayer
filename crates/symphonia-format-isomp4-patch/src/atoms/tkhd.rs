// Symphonia
// Copyright (c) 2019-2026 The Project Symphonia Developers.
//
// This Source Code Form is subject to the terms of the Mozilla Public
// License, v. 2.0. If a copy of the MPL was not distributed with this
// file, You can obtain one at https://mozilla.org/MPL/2.0/.

use crate::atoms::{Atom, AtomHeader, AtomIterator, ReadAtom, Result, decode_error};
use crate::fp::FpU8;

/// Единица fixed-point matrix 16.16 для `a/b/c/d` элементов `tkhd`.
const MATRIX_FIXED_ONE: i32 = 0x0001_0000;
const MATRIX_FIXED_NEG_ONE: i32 = -MATRIX_FIXED_ONE;
/// Единица fixed-point matrix 2.30 для perspective divisor `w`.
const MATRIX_PERSPECTIVE_ONE: i32 = 0x4000_0000;

/// Track header atom.
#[allow(dead_code)]
#[derive(Debug)]
pub struct TkhdAtom {
    /// Track header flags.
    pub flags: u32,
    /// Creation time.
    pub ctime: u64,
    /// Modification time.
    pub mtime: u64,
    /// Track identifier.
    pub id: u32,
    /// Track duration in the timescale units specified in the movie header.
    ///
    /// This value is equal to the sum of the durations of all the track's edits. If there are no
    /// edits, then this is the duration of all the track's samples.
    pub duration: u64,
    /// Layer.
    pub layer: u16,
    /// Grouping identifier.
    pub alternate_group: u16,
    /// Preferred volume for track playback.
    pub volume: FpU8,
    /// Display transform matrix из `tkhd`.
    pub display_matrix: TkhdDisplayMatrix,
}

/// Display matrix из MP4/QuickTime `tkhd`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct TkhdDisplayMatrix {
    /// Горизонтальный scale/rotation component.
    pub a: i32,
    /// Горизонтальный shear/rotation component.
    pub b: i32,
    /// Perspective component; для обычного видео должен быть 0.
    pub u: i32,
    /// Вертикальный shear/rotation component.
    pub c: i32,
    /// Вертикальный scale/rotation component.
    pub d: i32,
    /// Perspective component; для обычного видео должен быть 0.
    pub v: i32,
    /// Horizontal translation в 16.16.
    pub x: i32,
    /// Vertical translation в 16.16.
    pub y: i32,
    /// Perspective divisor в 2.30.
    pub w: i32,
}

impl Default for TkhdDisplayMatrix {
    /// Возвращает identity matrix из ISO BMFF default.
    fn default() -> Self {
        Self {
            a: MATRIX_FIXED_ONE,
            b: 0,
            u: 0,
            c: 0,
            d: MATRIX_FIXED_ONE,
            v: 0,
            x: 0,
            y: 0,
            w: MATRIX_PERSPECTIVE_ONE,
        }
    }
}

impl TkhdDisplayMatrix {
    /// Читает 3x3 display matrix в порядке, в котором она лежит в `tkhd`.
    fn read<R: ReadAtom>(it: &mut AtomIterator<R>) -> Result<Self> {
        Ok(Self {
            a: it.read_i32()?,
            b: it.read_i32()?,
            u: it.read_i32()?,
            c: it.read_i32()?,
            d: it.read_i32()?,
            v: it.read_i32()?,
            x: it.read_i32()?,
            y: it.read_i32()?,
            w: it.read_i32()?,
        })
    }

    /// Распознаёт только безопасные quarter-turn transforms без perspective/shear.
    pub fn quarter_turn_clockwise_degrees(self) -> Option<u16> {
        if self.u != 0 || self.v != 0 || self.w != MATRIX_PERSPECTIVE_ONE {
            return None;
        }

        let rotation_terms = (self.a, self.b, self.c, self.d);

        match rotation_terms {
            (MATRIX_FIXED_ONE, 0, 0, MATRIX_FIXED_ONE) => Some(0),
            (0, MATRIX_FIXED_ONE, MATRIX_FIXED_NEG_ONE, 0) => Some(90),
            (MATRIX_FIXED_NEG_ONE, 0, 0, MATRIX_FIXED_NEG_ONE) => Some(180),
            (0, MATRIX_FIXED_NEG_ONE, MATRIX_FIXED_ONE, 0) => Some(270),
            _ => None,
        }
    }
}

impl Atom for TkhdAtom {
    fn read<R: ReadAtom>(it: &mut AtomIterator<R>, _header: &AtomHeader) -> Result<Self> {
        let (version, flags) = it.read_extended_header()?;

        let mut tkhd = TkhdAtom {
            flags,
            ctime: 0,
            mtime: 0,
            id: 0,
            duration: 0,
            layer: 0,
            alternate_group: 0,
            volume: Default::default(),
            display_matrix: TkhdDisplayMatrix::default(),
        };

        // Version 0 uses 32-bit time values, verion 1 used 64-bit values.
        match version {
            0 => {
                tkhd.ctime = u64::from(it.read_u32()?);
                tkhd.mtime = u64::from(it.read_u32()?);
                tkhd.id = it.read_u32()?;
                let _ = it.read_u32()?; // Reserved
                tkhd.duration = u64::from(it.read_u32()?);
            }
            1 => {
                tkhd.ctime = it.read_u64()?;
                tkhd.mtime = it.read_u64()?;
                tkhd.id = it.read_u32()?;
                let _ = it.read_u32()?; // Reserved
                tkhd.duration = it.read_u64()?;
            }
            _ => return decode_error("isomp4 (tkhd): invalid version"),
        }

        // Reserved
        let _ = it.read_u64()?;

        tkhd.layer = it.read_u16()?;
        tkhd.alternate_group = it.read_u16()?;
        tkhd.volume = FpU8::parse_raw(it.read_u16()?);

        let _ = it.read_u16()?; // Reserved
        tkhd.display_matrix = TkhdDisplayMatrix::read(it)?;
        let _ = it.read_u32()?; // Width 16.16 fixed-point
        let _ = it.read_u32()?; // Height 16.16 fixed-point

        Ok(tkhd)
    }
}

#[cfg(test)]
mod tests {
    use super::{MATRIX_FIXED_NEG_ONE, MATRIX_FIXED_ONE, TkhdDisplayMatrix};

    #[test]
    fn display_matrix_recognizes_android_portrait_rotation_like_ffmpeg_autorotate() {
        let matrix = TkhdDisplayMatrix {
            a: 0,
            b: MATRIX_FIXED_ONE,
            c: MATRIX_FIXED_NEG_ONE,
            d: 0,
            ..TkhdDisplayMatrix::default()
        };

        assert_eq!(matrix.quarter_turn_clockwise_degrees(), Some(90));
    }

    #[test]
    fn display_matrix_recognizes_inverse_portrait_rotation() {
        let matrix = TkhdDisplayMatrix {
            a: 0,
            b: MATRIX_FIXED_NEG_ONE,
            c: MATRIX_FIXED_ONE,
            d: 0,
            ..TkhdDisplayMatrix::default()
        };

        assert_eq!(matrix.quarter_turn_clockwise_degrees(), Some(270));
    }

    #[test]
    fn display_matrix_rejects_non_quarter_turn_transform() {
        let matrix = TkhdDisplayMatrix {
            a: MATRIX_FIXED_ONE,
            b: 1024,
            c: 0,
            d: MATRIX_FIXED_ONE,
            ..TkhdDisplayMatrix::default()
        };

        assert_eq!(matrix.quarter_turn_clockwise_degrees(), None);
    }

    #[test]
    fn display_matrix_rejects_perspective_transform() {
        let matrix = TkhdDisplayMatrix {
            u: 1,
            ..TkhdDisplayMatrix::default()
        };

        assert_eq!(matrix.quarter_turn_clockwise_degrees(), None);
    }
}
