use super::*;

fn call(width: u32, rows: u32, row_length: Option<u32>) -> UploadCall {
    UploadCall {
        first_row: 0,
        width,
        rows,
        row_length,
        source_bytes: 0..0,
    }
}

#[test]
fn gl_read_size_counts_last_row_without_tail() {
    // 3 строки шириной 5, шаг 8: 2 × 8 + 5.
    assert_eq!(
        gl_read_size(PlaneTextureFormat::Luminance, &call(5, 3, Some(8))),
        Ok(21)
    );
    // Без ROW_LENGTH шаг = ширине вызова.
    assert_eq!(
        gl_read_size(PlaneTextureFormat::RedExt, &call(8, 2, None)),
        Ok(16)
    );
    assert_eq!(
        gl_read_size(PlaneTextureFormat::Luminance, &call(5, 1, None)),
        Ok(5)
    );
}
