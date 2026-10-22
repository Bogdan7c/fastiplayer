//! Копия плоскости в mapped staging без GPU: проверяются только байты.

use super::copy_plane_block_into_staging;

/// Байт, которым заранее залит staging: копия не должна трогать выравнивающий хвост строк.
const UNTOUCHED_STAGING_BYTE: u8 = 0xEE;

/// Байт padding-а строк источника: в staging он попасть не должен.
const SOURCE_PADDING_BYTE: u8 = 0xAB;

/// Детерминированное значение видимого байта строки, разное для соседних строк и столбцов.
fn visible_byte(row: usize, column: usize) -> u8 {
    ((row * 31 + column * 7) % 251) as u8
}

/// Блок плоскости в раскладке FFmpeg: строки через `stride`, у последней строки нет padding-а.
fn plane_block(visible_row_bytes: usize, visible_height: usize, stride: usize) -> Vec<u8> {
    let block_len = stride * (visible_height - 1) + visible_row_bytes;
    let mut block = vec![SOURCE_PADDING_BYTE; block_len];
    for row in 0..visible_height {
        for column in 0..visible_row_bytes {
            block[row * stride + column] = visible_byte(row, column);
        }
    }
    block
}

/// Копирует блок в staging с заданным stride-ом и возвращает staging целиком.
fn copy_into_fresh_staging(
    block: &[u8],
    src_stride: usize,
    dst_stride: usize,
    visible_row_bytes: usize,
    visible_height: usize,
) -> Vec<u8> {
    let mut staging = vec![UNTOUCHED_STAGING_BYTE; dst_stride * visible_height];
    copy_plane_block_into_staging(
        block,
        wgpu::WriteOnly::from_mut(staging.as_mut_slice()),
        src_stride,
        dst_stride,
        visible_row_bytes,
        visible_height,
    );
    staging
}

#[test]
fn padded_source_rows_land_in_aligned_staging_rows_without_padding() {
    let (visible_row_bytes, visible_height) = (5, 3);
    let (src_stride, dst_stride) = (8, 256);
    let block = plane_block(visible_row_bytes, visible_height, src_stride);

    let staging = copy_into_fresh_staging(
        &block,
        src_stride,
        dst_stride,
        visible_row_bytes,
        visible_height,
    );

    for (row, staging_row) in staging.chunks_exact(dst_stride).enumerate() {
        let expected_visible: Vec<u8> = (0..visible_row_bytes)
            .map(|column| visible_byte(row, column))
            .collect();
        assert_eq!(
            &staging_row[..visible_row_bytes],
            expected_visible.as_slice()
        );
        assert!(
            staging_row[visible_row_bytes..]
                .iter()
                .all(|&byte| byte == UNTOUCHED_STAGING_BYTE),
            "строка {row}: хвост выравнивания staging не должен перезаписываться"
        );
    }
}

/// 4K luma (8 МиБ) — размер, который раньше делился на полосы между потоками:
/// однопоточная копия обязана покрыть все строки целиком, без швов на границах полос.
#[test]
fn whole_4k_luma_plane_is_copied_row_for_row() {
    let (visible_row_bytes, visible_height) = (3840, 2160);
    let (src_stride, dst_stride) = (3904, 3840);
    let block = plane_block(visible_row_bytes, visible_height, src_stride);

    let staging = copy_into_fresh_staging(
        &block,
        src_stride,
        dst_stride,
        visible_row_bytes,
        visible_height,
    );

    let mismatched_rows: Vec<usize> = staging
        .chunks_exact(dst_stride)
        .enumerate()
        .filter(|(row, staging_row)| {
            let source_row = &block[row * src_stride..row * src_stride + visible_row_bytes];
            *staging_row != source_row
        })
        .map(|(row, _)| row)
        .collect();
    assert!(
        mismatched_rows.is_empty(),
        "строки скопированы не так, как в источнике: {:?}",
        &mismatched_rows[..mismatched_rows.len().min(8)]
    );
}

#[test]
fn zero_height_plane_leaves_staging_untouched() {
    let mut staging = vec![UNTOUCHED_STAGING_BYTE; 256];

    copy_plane_block_into_staging(
        &[],
        wgpu::WriteOnly::from_mut(staging.as_mut_slice()),
        4,
        256,
        4,
        0,
    );

    assert!(staging.iter().all(|&byte| byte == UNTOUCHED_STAGING_BYTE));
}
