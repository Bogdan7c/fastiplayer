//! План загрузки одной картинки в атлас — чистый CPU.
//!
//! В ядре GLES 2.0 шаг строк источника всегда равен ширине вызова. Картинка без хвостов строк
//! грузится одним вызовом; с хвостами — одним вызовом с `GL_UNPACK_ROW_LENGTH`
//! (`GL_EXT_unpack_subimage`) или по строке на вызов. Переупаковки на CPU нет: каждый вызов читает
//! прямо из среза вызывающего.

use crate::gl_calls::overlay::AtlasUploadCall;
use crate::planes::HostRows;

use super::atlas::AtlasSlot;

/// Чем драйвер может задать шаг строк.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) enum RowStepSupport {
    /// Есть `GL_UNPACK_ROW_LENGTH`.
    UnpackRowLength,
    /// Нет: строки с хвостами — по одной.
    RowByRow,
}

/// Вызовы загрузки картинки `rows` (проверенные строки, `bytes_per_texel` байт на тексель) в
/// слот `slot` атласа. Порядок вызовов — сверху вниз.
pub(super) fn atlas_upload_calls(
    rows: &HostRows<'_>,
    bytes_per_texel: usize,
    slot: AtlasSlot,
    support: RowStepSupport,
) -> Option<Vec<AtlasUploadCall>> {
    let width = u32::try_from(rows.visible_row_bytes().checked_div(bytes_per_texel)?).ok()?;
    let tight = rows.stride() == rows.visible_row_bytes();
    // Шаг строки в текселях: годится для `UNPACK_ROW_LENGTH`, только если делится нацело.
    let row_length_texels = rows
        .stride()
        .is_multiple_of(bytes_per_texel)
        .then(|| u32::try_from(rows.stride() / bytes_per_texel).ok())
        .flatten();
    let whole_block = |row_length: Option<u32>| {
        let end = crate::planes::required_host_bytes(
            rows.stride(),
            rows.visible_row_bytes(),
            rows.rows(),
        )?;
        Some(vec![AtlasUploadCall {
            x: slot.x,
            y: slot.y,
            width,
            rows: rows.rows(),
            row_length,
            source_bytes: 0..end,
        }])
    };
    match (tight, support, row_length_texels) {
        (true, _, _) => whole_block(None),
        (false, RowStepSupport::UnpackRowLength, Some(row_length)) => whole_block(Some(row_length)),
        (false, _, _) => (0..rows.rows())
            .map(|row| {
                let start = usize::try_from(row).ok()?.checked_mul(rows.stride())?;
                Some(AtlasUploadCall {
                    x: slot.x,
                    y: slot.y.checked_add(row)?,
                    width,
                    rows: 1,
                    row_length: None,
                    source_bytes: start..start.checked_add(rows.visible_row_bytes())?,
                })
            })
            .collect(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::gl_calls::overlay::{AtlasTextureFormat, atlas_read_size};

    const SLOT: AtlasSlot = AtlasSlot { x: 7, y: 9 };

    fn total_read(calls: &[AtlasUploadCall], format: AtlasTextureFormat) -> usize {
        calls
            .iter()
            .map(|call| atlas_read_size(format, call).expect("small sizes"))
            .sum()
    }

    #[test]
    fn tight_rows_are_one_call_without_row_length() {
        let bytes = vec![1_u8; 12 * 3];
        let rows = HostRows::new(&bytes, 12, 12, 3).expect("tight rows");
        let calls = atlas_upload_calls(&rows, 1, SLOT, RowStepSupport::RowByRow).expect("plan");
        assert_eq!(
            calls,
            vec![AtlasUploadCall {
                x: 7,
                y: 9,
                width: 12,
                rows: 3,
                row_length: None,
                source_bytes: 0..36,
            }]
        );
    }

    #[test]
    fn padded_rows_use_row_length_when_the_driver_has_it() {
        // Окно в общем буфере: шаг 48 байт (12 пикселей RGBA), видимо 8 пикселей.
        let stride = 48;
        let visible = 8 * 4;
        let bytes = vec![0_u8; stride * 3 + visible];
        let rows = HostRows::new(&bytes, stride, visible, 4).expect("window rows");
        let calls =
            atlas_upload_calls(&rows, 4, SLOT, RowStepSupport::UnpackRowLength).expect("plan");
        assert_eq!(calls.len(), 1);
        assert_eq!(calls[0].row_length, Some(12));
        assert_eq!(calls[0].width, 8);
        // Драйвер прочитает ровно видимый блок без хвоста последней строки.
        assert_eq!(
            total_read(&calls, AtlasTextureFormat::PremultipliedRgba),
            calls[0].source_bytes.len()
        );
        assert_eq!(calls[0].source_bytes, 0..stride * 3 + visible);
    }

    #[test]
    fn padded_rows_without_row_length_go_one_row_per_call() {
        let bytes = vec![0_u8; 20 * 2 + 5];
        let rows = HostRows::new(&bytes, 20, 5, 3).expect("padded rows");
        let calls = atlas_upload_calls(&rows, 1, SLOT, RowStepSupport::RowByRow).expect("plan");
        assert_eq!(calls.len(), 3);
        for (index, call) in calls.iter().enumerate() {
            assert_eq!(call.y, 9 + index as u32);
            assert_eq!(call.rows, 1);
            assert_eq!(call.row_length, None);
            assert_eq!(call.source_bytes, index * 20..index * 20 + 5);
        }
        // Каждая строка читает только свои видимые байты.
        assert_eq!(
            total_read(&calls, AtlasTextureFormat::CoverageLuminance),
            15
        );
    }

    #[test]
    fn stride_not_divisible_by_texel_size_falls_back_to_rows() {
        // RGBA со сдвигом окна на байт кратности не даёт: UNPACK_ROW_LENGTH задаётся в текселях.
        let stride = 4 * 3 + 2;
        let bytes = vec![0_u8; stride + 8];
        let rows = HostRows::new(&bytes, stride, 8, 2).expect("rows");
        let calls =
            atlas_upload_calls(&rows, 4, SLOT, RowStepSupport::UnpackRowLength).expect("plan");
        assert_eq!(calls.len(), 2);
        assert!(calls.iter().all(|call| call.row_length.is_none()));
    }
}
