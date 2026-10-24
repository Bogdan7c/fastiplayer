use super::*;

const LUMA_1080P: PlaneExtent = PlaneExtent {
    width: 1920,
    height: 1080,
};

const ODD_LUMA: PlaneExtent = PlaneExtent {
    width: 5,
    height: 3,
};

fn frame_with(
    layout: ChromaSubsampling,
    luma: PlaneExtent,
    rows: HostRows<'_>,
) -> PlanarFrameUpload<'_> {
    PlanarFrameUpload {
        key: VideoFrameKey(1),
        layout: PlanarLayout {
            chroma: layout,
            depth: SampleDepth::Bits8,
        },
        luma_extent: luma,
        luma: PlaneSource::HostRows(rows),
        chroma_u: PlaneSource::HostRows(rows),
        chroma_v: PlaneSource::HostRows(rows),
    }
}

#[test]
fn chroma_extent_rounds_odd_sides_up_like_ffmpeg() {
    assert_eq!(
        ChromaSubsampling::Yuv420.chroma_extent(ODD_LUMA),
        PlaneExtent {
            width: 3,
            height: 2
        }
    );
    assert_eq!(
        ChromaSubsampling::Yuv422.chroma_extent(ODD_LUMA),
        PlaneExtent {
            width: 3,
            height: 3
        }
    );
    assert_eq!(ChromaSubsampling::Yuv444.chroma_extent(ODD_LUMA), ODD_LUMA);
    assert_eq!(
        ChromaSubsampling::Yuv420.chroma_extent(LUMA_1080P),
        PlaneExtent {
            width: 960,
            height: 540
        }
    );
}

#[test]
fn host_rows_accept_block_without_tail_of_last_row() {
    // 3 строки по 5 байт, stride 8: нужно 2 × 8 + 5 = 21 байт, хвост последней строки не нужен.
    let bytes = [0_u8; 21];
    let rows = HostRows::new(&bytes, 8, 5, 3);
    assert!(rows.is_ok(), "{rows:?}");
}

#[test]
fn host_rows_reject_truncated_block() {
    let bytes = [0_u8; 20];
    assert_eq!(
        HostRows::new(&bytes, 8, 5, 3).err(),
        Some(PlaneSourceError::TruncatedRows {
            required: 21,
            actual: 20
        })
    );
}

#[test]
fn host_rows_reject_overlapping_and_empty_rows() {
    let bytes = [0_u8; 64];
    assert_eq!(
        HostRows::new(&bytes, 4, 5, 3).err(),
        Some(PlaneSourceError::StrideShorterThanRow {
            stride: 4,
            visible_row_bytes: 5
        })
    );
    assert_eq!(
        HostRows::new(&bytes, 8, 0, 3).err(),
        Some(PlaneSourceError::EmptyPlane)
    );
    assert_eq!(
        HostRows::new(&bytes, 8, 5, 0).err(),
        Some(PlaneSourceError::EmptyPlane)
    );
}

#[test]
fn host_rows_reject_overflowing_geometry() {
    let bytes = [0_u8; 8];
    assert_eq!(
        HostRows::new(&bytes, usize::MAX, 5, 3).err(),
        Some(PlaneSourceError::SizeOverflow)
    );
}

#[test]
fn plane_validation_checks_rows_and_row_bytes_against_layout() {
    let bytes = [0_u8; 64];
    // Цветность 4:2:0 для яркости 5×3 — 3×2.
    let chroma_rows = HostRows::new(&bytes, 8, 3, 2).unwrap_or_else(|error| panic!("{error}"));
    let frame = frame_with(ChromaSubsampling::Yuv420, ODD_LUMA, chroma_rows);
    assert_eq!(
        validate_host_plane(&frame, PlaneRole::ChromaU, &chroma_rows),
        Ok(())
    );
    assert_eq!(
        validate_host_plane(&frame, PlaneRole::Luma, &chroma_rows),
        Err(FrameLayoutError::RowBytesMismatch {
            role: PlaneRole::Luma,
            expected: 5,
            actual: 3
        })
    );

    let wrong_height = HostRows::new(&bytes, 8, 3, 3).unwrap_or_else(|error| panic!("{error}"));
    assert_eq!(
        validate_host_plane(&frame, PlaneRole::ChromaV, &wrong_height),
        Err(FrameLayoutError::RowCountMismatch {
            role: PlaneRole::ChromaV,
            expected: 2,
            actual: 3
        })
    );
}

#[test]
fn frame_planes_come_in_luma_u_v_order() {
    let bytes = [0_u8; 64];
    let rows = HostRows::new(&bytes, 8, 5, 3).unwrap_or_else(|error| panic!("{error}"));
    let frame = frame_with(ChromaSubsampling::Yuv444, ODD_LUMA, rows);
    let roles = frame.planes().map(|(role, _)| role);
    assert_eq!(
        roles,
        [PlaneRole::Luma, PlaneRole::ChromaU, PlaneRole::ChromaV]
    );
}
