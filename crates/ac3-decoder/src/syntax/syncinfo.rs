//! `syncinfo` — синхрослово, `crc1`, частота и размер кадра (A/52 §5.3.1, §5.4.1).
//!
//! Кроме самих полей здесь решается вопрос «какой синтаксис дальше»: `bsid`
//! лежит сразу после `syncinfo` и в AC-3, и в E-AC-3 (Annex E §2.1), поэтому
//! его можно подсмотреть до разбора BSI.

use crate::error::{Ac3DecodeError, UnsupportedFeature};
use crate::tables::frame_size::{SampleRateCode, frame_size_row};

/// Синхрослово syncframe-а (A/52 §5.4.1.1).
pub(crate) const SYNC_WORD: u16 = 0x0B77;

/// Байт, в старших 5 битах которого лежит `bsid` (после 16+16+2+6 бит syncinfo).
const BSID_BYTE_OFFSET: usize = 5;

/// Сколько байт нужно, чтобы прочитать `syncinfo` и `bsid`.
pub(crate) const MINIMUM_HEADER_BYTES: usize = BSID_BYTE_OFFSET + 1;

/// Последний `bsid`, который читается синтаксисом AC-3 в полной частоте.
const LAST_FULL_RATE_BSID: u8 = 8;
/// Последний `bsid` синтаксиса AC-3 вообще: 9 и 10 — пониженная частота.
const LAST_AC3_SYNTAX_BSID: u8 = 10;
/// Диапазон `bsid` E-AC-3 (Annex E §2.3.1.6).
const EAC3_BSID_RANGE: std::ops::RangeInclusive<u8> = 11..=16;

/// Поля `syncinfo` и производная длина кадра.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) struct SyncInfo {
    /// `fscod` (Таблица 5.6).
    pub(crate) sample_rate_code: SampleRateCode,
    /// `frmsizecod` (Таблица 5.18).
    pub(crate) frame_size_code: u8,
    /// Номинальный битрейт по Таблице 5.18, кбит/с.
    pub(crate) nominal_bitrate_kbps: u16,
    /// Длина кадра в 16-битных словах.
    pub(crate) frame_words: usize,
}

impl SyncInfo {
    /// Длина кадра в байтах.
    pub(crate) const fn frame_bytes(self) -> usize {
        self.frame_words * 2
    }
}

/// Какой синтаксис следует за `syncinfo`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum BitstreamFamily {
    /// AC-3 (`bsid` 0–10).
    ///
    /// Сдвиг частоты: 0 для `bsid` ≤ 8, 1 для 9 (половинная), 2 для 10
    /// (четвертная). Спецификации A/52:2012 и ETSI TS 102 366 значения 9/10
    /// не определяют (A/52 §5.4.2.1 велит глушить `bsid > 8`); принято то же
    /// толкование, что у проектного `ac3-syncframe`: синтаксис и таблица длин
    /// AC-3, частота `fs >> (bsid − 8)`.
    Ac3 { sample_rate_shift: u32 },
    /// E-AC-3 (`bsid` 11–16, Annex E).
    EnhancedAc3,
}

/// Разбирает `syncinfo` и классифицирует `bsid`; буфер должен содержать кадр
/// целиком, иначе `Truncated` с объявленной длиной кадра.
pub(crate) fn parse_syncinfo(frame: &[u8]) -> Result<(SyncInfo, BitstreamFamily), Ac3DecodeError> {
    let Some(header) = frame.first_chunk::<MINIMUM_HEADER_BYTES>() else {
        return Err(Ac3DecodeError::Truncated {
            available_bytes: frame.len(),
            required_bytes: MINIMUM_HEADER_BYTES,
        });
    };
    if u16::from_be_bytes([header[0], header[1]]) != SYNC_WORD {
        return Err(Ac3DecodeError::MissingSyncWord);
    }
    let bitstream_id = header[BSID_BYTE_OFFSET] >> 3;
    let family = classify_bitstream_id(bitstream_id)?;
    if family == BitstreamFamily::EnhancedAc3 {
        // Длина E-AC-3 кадра считается иначе (Annex E `frmsiz`); это следующие пакеты.
        return Err(Ac3DecodeError::Unsupported(UnsupportedFeature::EnhancedAc3));
    }
    // Байт 4: fscod (2 бита) + frmsizecod (6 бит); байты 2–3 — crc1.
    let sample_rate_code =
        SampleRateCode::from_code(header[4] >> 6).ok_or(Ac3DecodeError::ReservedSampleRateCode)?;
    let frame_size_code = header[4] & 0x3F;
    let row = frame_size_row(frame_size_code)
        .ok_or(Ac3DecodeError::ReservedFrameSizeCode { frame_size_code })?;
    let syncinfo = SyncInfo {
        sample_rate_code,
        frame_size_code,
        nominal_bitrate_kbps: row.nominal_kbps,
        frame_words: usize::from(row.words(sample_rate_code)),
    };
    if frame.len() < syncinfo.frame_bytes() {
        return Err(Ac3DecodeError::Truncated {
            available_bytes: frame.len(),
            required_bytes: syncinfo.frame_bytes(),
        });
    }
    Ok((syncinfo, family))
}

/// Классифицирует `bsid` (A/52 §5.4.2.1, Annex E §2.3.1.6).
fn classify_bitstream_id(bitstream_id: u8) -> Result<BitstreamFamily, Ac3DecodeError> {
    if bitstream_id <= LAST_FULL_RATE_BSID {
        Ok(BitstreamFamily::Ac3 {
            sample_rate_shift: 0,
        })
    } else if bitstream_id <= LAST_AC3_SYNTAX_BSID {
        Ok(BitstreamFamily::Ac3 {
            sample_rate_shift: u32::from(bitstream_id - LAST_FULL_RATE_BSID),
        })
    } else if EAC3_BSID_RANGE.contains(&bitstream_id) {
        Ok(BitstreamFamily::EnhancedAc3)
    } else {
        Err(Ac3DecodeError::UnsupportedBitstreamId { bitstream_id })
    }
}

#[cfg(test)]
mod tests {
    use super::{BitstreamFamily, classify_bitstream_id, parse_syncinfo};
    use crate::error::{Ac3DecodeError, UnsupportedFeature};
    use crate::tables::frame_size::SampleRateCode;

    fn header(fscod: u8, frmsizecod: u8, bsid: u8) -> Vec<u8> {
        vec![0x0B, 0x77, 0x00, 0x00, (fscod << 6) | frmsizecod, bsid << 3]
    }

    #[test]
    fn bitstream_id_ranges_follow_spec_and_reduced_rate_convention() {
        assert_eq!(
            classify_bitstream_id(0),
            Ok(BitstreamFamily::Ac3 {
                sample_rate_shift: 0
            })
        );
        assert_eq!(
            classify_bitstream_id(6),
            Ok(BitstreamFamily::Ac3 {
                sample_rate_shift: 0
            })
        );
        assert_eq!(
            classify_bitstream_id(9),
            Ok(BitstreamFamily::Ac3 {
                sample_rate_shift: 1
            })
        );
        assert_eq!(
            classify_bitstream_id(10),
            Ok(BitstreamFamily::Ac3 {
                sample_rate_shift: 2
            })
        );
        assert_eq!(classify_bitstream_id(16), Ok(BitstreamFamily::EnhancedAc3));
        assert_eq!(
            classify_bitstream_id(17),
            Err(Ac3DecodeError::UnsupportedBitstreamId { bitstream_id: 17 })
        );
    }

    #[test]
    fn short_buffer_reports_required_frame_length() {
        // 48 кГц, frmsizecod 8 → 128 слов = 256 байт.
        let bytes = header(0, 8, 8);
        assert_eq!(
            parse_syncinfo(&bytes),
            Err(Ac3DecodeError::Truncated {
                available_bytes: 6,
                required_bytes: 256
            })
        );
        assert_eq!(
            parse_syncinfo(&bytes[..3]),
            Err(Ac3DecodeError::Truncated {
                available_bytes: 3,
                required_bytes: 6
            })
        );
    }

    #[test]
    fn full_frame_yields_table_values() {
        let mut bytes = header(1, 9, 8);
        bytes.resize(280, 0);
        let (syncinfo, family) = parse_syncinfo(&bytes).unwrap();
        assert_eq!(syncinfo.sample_rate_code, SampleRateCode::Rate44100);
        assert_eq!(syncinfo.frame_words, 140);
        assert_eq!(syncinfo.nominal_bitrate_kbps, 64);
        assert_eq!(
            family,
            BitstreamFamily::Ac3 {
                sample_rate_shift: 0
            }
        );
    }

    #[test]
    fn reserved_codes_and_foreign_syntax_are_typed() {
        assert_eq!(
            parse_syncinfo(&header(3, 0, 8)),
            Err(Ac3DecodeError::ReservedSampleRateCode)
        );
        assert_eq!(
            parse_syncinfo(&header(0, 38, 8)),
            Err(Ac3DecodeError::ReservedFrameSizeCode {
                frame_size_code: 38
            })
        );
        assert_eq!(
            parse_syncinfo(&header(0, 0, 16)),
            Err(Ac3DecodeError::Unsupported(UnsupportedFeature::EnhancedAc3))
        );
        let mut no_sync = header(0, 0, 8);
        no_sync[1] = 0x78;
        assert_eq!(
            parse_syncinfo(&no_sync),
            Err(Ac3DecodeError::MissingSyncWord)
        );
    }
}
