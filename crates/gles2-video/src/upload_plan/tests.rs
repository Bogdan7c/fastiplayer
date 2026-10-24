use super::*;

fn rows(bytes: &[u8], stride: usize, visible_row_bytes: usize, row_count: u32) -> HostRows<'_> {
    HostRows::new(bytes, stride, visible_row_bytes, row_count).expect("valid host rows")
}

const VISIBLE_5X3: PlaneExtent = PlaneExtent {
    width: 5,
    height: 3,
};

#[test]
fn tight_rows_upload_in_one_call_with_either_support() {
    let bytes = [0_u8; 15];
    let host_rows = rows(&bytes, 5, 5, 3);
    for support in [
        RowLengthSupport::UnpackRowLength,
        RowLengthSupport::StrideWideTexture,
    ] {
        let plan = plan_plane_upload(&host_rows, VISIBLE_5X3, 1, support).expect("plan");
        assert_eq!(
            plan.texture,
            TextureExtent {
                width: 5,
                height: 3
            }
        );
        assert_eq!(plan.calls().count(), 1);
        assert_eq!(plan.first_call.row_length, None);
        assert_eq!(plan.copied_bytes(), 15);
    }
}

#[test]
fn row_length_extension_uploads_padded_rows_in_one_call() {
    // stride 8, видимо 5: нужно 2 × 8 + 5 = 21 байт.
    let bytes = [0_u8; 21];
    let plan = plan_plane_upload(
        &rows(&bytes, 8, 5, 3),
        VISIBLE_5X3,
        1,
        RowLengthSupport::UnpackRowLength,
    )
    .expect("plan");
    assert_eq!(
        plan.texture,
        TextureExtent {
            width: 5,
            height: 3
        }
    );
    assert_eq!(
        plan.first_call,
        UploadCall {
            first_row: 0,
            width: 5,
            rows: 3,
            row_length: Some(8),
            source_bytes: 0..21,
        }
    );
    assert!(plan.last_row_call.is_none());
    // В текстуру уходят только видимые байты: хвосты строк драйвер пропускает.
    assert_eq!(plan.copied_bytes(), 15);
}

#[test]
fn stride_wide_texture_never_reads_past_the_visible_block() {
    let bytes = [0_u8; 21];
    let plan = plan_plane_upload(
        &rows(&bytes, 8, 5, 3),
        VISIBLE_5X3,
        1,
        RowLengthSupport::StrideWideTexture,
    )
    .expect("plan");
    assert_eq!(
        plan.texture,
        TextureExtent {
            width: 8,
            height: 3
        }
    );
    assert_eq!(
        plan.first_call,
        UploadCall {
            first_row: 0,
            width: 8,
            rows: 2,
            row_length: None,
            source_bytes: 0..16,
        }
    );
    assert_eq!(
        plan.last_row_call,
        Some(UploadCall {
            first_row: 2,
            width: 5,
            rows: 1,
            row_length: None,
            source_bytes: 16..21,
        })
    );
    // Хвосты первых строк копируются вместе со строками, хвост последней не читается.
    assert_eq!(plan.copied_bytes(), 21);
}

#[test]
fn single_padded_row_uploads_only_its_visible_part() {
    let bytes = [0_u8; 5];
    let plan = plan_plane_upload(
        &rows(&bytes, 8, 5, 1),
        PlaneExtent {
            width: 5,
            height: 1,
        },
        1,
        RowLengthSupport::StrideWideTexture,
    )
    .expect("plan");
    assert_eq!(
        plan.texture,
        TextureExtent {
            width: 8,
            height: 1
        }
    );
    assert_eq!(plan.calls().count(), 1);
    assert_eq!(plan.first_call.source_bytes, 0..5);
    assert_eq!(plan.first_call.width, 5);
}

#[test]
fn stride_not_multiple_of_sample_size_is_rejected() {
    let bytes = [0_u8; 32];
    assert_eq!(
        plan_plane_upload(
            &rows(&bytes, 9, 4, 2),
            PlaneExtent {
                width: 2,
                height: 2
            },
            2,
            RowLengthSupport::UnpackRowLength,
        ),
        Err(UnalignedPlaneRows)
    );
}
