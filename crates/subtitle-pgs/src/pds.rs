//! PDS (0x14): палитра в YCbCr с прозрачностью.

use subtitle_core::{PaletteColor, PicturePalette};

use crate::color::{PgsColorMatrix, ycbcr_to_rgb};
use crate::error::SegmentParseError;
use crate::reader::ByteReader;
use crate::segment::SegmentKind;

/// Запись палитры в исходном виде (YCbCr, как в потоке).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct PaletteEntryYcbcr {
    pub index: u8,
    pub luma: u8,
    pub cb: u8,
    pub cr: u8,
    pub alpha: u8,
}

/// Разобранный PDS.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PaletteDefinition {
    pub palette_id: u8,
    pub version: u8,
    pub entries: Vec<PaletteEntryYcbcr>,
    /// Байт хвоста, не составивших целую запись (0..=4). «Мягкая» проблема: целые записи
    /// использованы, вызывающий лишь считает отклонение.
    pub trailing_partial_bytes: usize,
}

impl PaletteDefinition {
    /// Переводит в RGBA-палитру выбранной матрицей. Записи, которых нет в сегменте,
    /// остаются полностью прозрачными. Альфа переносится как есть (straight).
    pub fn to_palette(&self, matrix: PgsColorMatrix) -> PicturePalette {
        let mut colors = [PaletteColor::TRANSPARENT; PicturePalette::LEN];
        for entry in &self.entries {
            let [red, green, blue] = ycbcr_to_rgb(matrix, entry.luma, entry.cb, entry.cr);
            colors[usize::from(entry.index)] = PaletteColor {
                red,
                green,
                blue,
                alpha: entry.alpha,
            };
        }
        PicturePalette::new(colors)
    }
}

/// Размер одной записи: индекс, Y, Cr, Cb, альфа.
const ENTRY_LEN: usize = 5;

/// Разбирает payload PDS. ВАЖНО: в потоке канал Cr идёт РАНЬШЕ Cb.
/// Хвост, не кратный размеру записи, НЕ ошибка: целые записи разбираются, остаток
/// возвращается в `trailing_partial_bytes` (декодер считает его в статистике).
pub fn parse_pds(payload: &[u8]) -> Result<PaletteDefinition, SegmentParseError> {
    let mut reader = ByteReader::new(SegmentKind::PaletteDefinition, payload);
    let palette_id = reader.u8()?;
    let version = reader.u8()?;
    let mut entries = Vec::with_capacity(reader.remaining() / ENTRY_LEN);
    while reader.remaining() >= ENTRY_LEN {
        let raw = reader.take(ENTRY_LEN)?;
        entries.push(PaletteEntryYcbcr {
            index: raw[0],
            luma: raw[1],
            cr: raw[2],
            cb: raw[3],
            alpha: raw[4],
        });
    }
    Ok(PaletteDefinition {
        palette_id,
        version,
        entries,
        trailing_partial_bytes: reader.remaining(),
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn reads_cr_before_cb() {
        let pds = parse_pds(&[5, 2, 7, 100, 200, 50, 255, 9, 16, 128, 128, 0]).expect("valid");
        assert_eq!(pds.palette_id, 5);
        assert_eq!(pds.version, 2);
        assert_eq!(
            pds.entries[0],
            PaletteEntryYcbcr {
                index: 7,
                luma: 100,
                cr: 200,
                cb: 50,
                alpha: 255
            }
        );
        assert_eq!(pds.entries.len(), 2);
    }

    #[test]
    fn unlisted_entries_are_transparent_and_alpha_is_straight() {
        let pds = parse_pds(&[0, 0, 3, 235, 128, 128, 128]).expect("valid");
        let palette = pds.to_palette(PgsColorMatrix::Bt709);
        assert_eq!(
            palette.color(3),
            PaletteColor {
                red: 255,
                green: 255,
                blue: 255,
                alpha: 128
            }
        );
        assert_eq!(palette.color(4), PaletteColor::TRANSPARENT);
    }

    #[test]
    fn partial_trailing_entry_is_tolerated_and_reported() {
        let pds = parse_pds(&[4, 1, 7, 100, 200, 50, 255, 1, 2, 3]).expect("tolerated");
        assert_eq!(pds.entries.len(), 1);
        assert_eq!(pds.entries[0].index, 7);
        assert_eq!(pds.trailing_partial_bytes, 3);
        let only_partial = parse_pds(&[0, 0, 1, 2, 3]).expect("tolerated");
        assert!(only_partial.entries.is_empty());
        assert_eq!(only_partial.trailing_partial_bytes, 3);
        assert!(matches!(
            parse_pds(&[0]),
            Err(SegmentParseError::Truncated { .. })
        ));
        assert!(
            parse_pds(&[0, 0])
                .expect("empty palette")
                .entries
                .is_empty()
        );
    }
}
