//! Проверки планарного кадра до драйвера: каждая ошибка — отдельный вариант, годный кадр
//! проходит и отдаёт ровно то, что в него положили.

use std::fs::File;
use std::os::fd::AsFd;

use super::*;
use crate::planes::ChromaSubsampling;

const LUMA: PlaneExtent = PlaneExtent {
    width: 64,
    height: 32,
};

/// Годный паспорт 4:2:0: Y 64×32 (шаг 128), U и V 32×16 (шаг 64) подряд за яркостью.
fn identity() -> DmaBufPlanarIdentity {
    DmaBufPlanarIdentity {
        resource_id: 7,
        object: DmaBufObjectIdentity {
            device: 1,
            inode: 2,
            size: 128 * 32 + 64 * 16 * 2,
            modifier: 0,
        },
        layout: PlanarLayout {
            chroma: ChromaSubsampling::Yuv420,
            depth: SampleDepth::Bits8,
        },
        luma_extent: LUMA,
        planes: [
            DmaBufPlaneLocation {
                offset: 0,
                pitch: 128,
            },
            DmaBufPlaneLocation {
                offset: 4096,
                pitch: 64,
            },
            DmaBufPlaneLocation {
                offset: 4096 + 1024,
                pitch: 64,
            },
        ],
    }
}

fn build(identity: &DmaBufPlanarIdentity) -> Result<(), DmaBufPlanarFrameError> {
    let file = File::open("/dev/null").expect("/dev/null opens");
    DmaBufPlanarFrame::new(VideoFrameKey(1), identity, file.as_fd()).map(|_| ())
}

#[test]
fn valid_frame_exposes_key_identity_and_derived_extents() {
    let file = File::open("/dev/null").expect("/dev/null opens");
    let passport = identity();
    let frame = DmaBufPlanarFrame::new(VideoFrameKey(9), &passport, file.as_fd()).unwrap();
    assert_eq!(frame.key(), VideoFrameKey(9));
    assert_eq!(frame.identity(), &passport);
    assert_eq!(passport.plane_extent(PlaneRole::Luma), LUMA);
    let chroma = PlaneExtent {
        width: 32,
        height: 16,
    };
    assert_eq!(passport.plane_extent(PlaneRole::ChromaU), chroma);
    assert_eq!(passport.plane_extent(PlaneRole::ChromaV), chroma);
    assert_eq!(passport.plane_location(PlaneRole::ChromaV).offset, 5120);
}

#[test]
fn odd_luma_rounds_chroma_up() {
    let mut passport = identity();
    passport.luma_extent = PlaneExtent {
        width: 63,
        height: 31,
    };
    assert_eq!(
        passport.plane_extent(PlaneRole::ChromaU),
        PlaneExtent {
            width: 32,
            height: 16
        }
    );
    assert_eq!(build(&passport), Ok(()));
}

#[test]
fn every_chroma_subsampling_is_accepted_at_eight_bits() {
    for chroma in [
        ChromaSubsampling::Yuv420,
        ChromaSubsampling::Yuv422,
        ChromaSubsampling::Yuv444,
    ] {
        let mut passport = identity();
        passport.layout.chroma = chroma;
        // Три полноразмерные плоскости в 4:4:4 занимают больше места: объект с запасом.
        passport.object.size = 1 << 20;
        passport.planes[1].pitch = 64;
        passport.planes[2].pitch = 64;
        assert_eq!(build(&passport), Ok(()), "{chroma:?}");
    }
}

/// 10/12 бит — пара байтов на отсчёт (`GR88`): тот же паспорт годен, если шаг вмещает
/// `ширина × 2` байта, а шаг, достаточный для 8 бит, — уже нет.
#[test]
fn high_bit_rows_are_two_bytes_per_sample() {
    for depth in [SampleDepth::Bits10, SampleDepth::Bits12] {
        let mut passport = identity();
        passport.layout.depth = depth;
        // Y 64 отсчёта = 128 байт при шаге 128, U/V 32 отсчёта = 64 байта при шаге 64.
        assert_eq!(build(&passport), Ok(()), "{depth:?}");
        // Шаг 48 вмещает 32 байта строки цветности 8 бит, но не 64 байта 10/12 бит.
        passport.planes[1].pitch = 48;
        assert_eq!(
            build(&passport),
            Err(DmaBufPlanarFrameError::PitchShorterThanRow {
                plane: PlaneRole::ChromaU,
                pitch: 48,
                row_bytes: 64,
            }),
            "{depth:?}"
        );
    }
}

#[test]
fn empty_luma_is_refused() {
    let mut passport = identity();
    passport.luma_extent.height = 0;
    assert_eq!(
        build(&passport),
        Err(DmaBufPlanarFrameError::EmptyExtent {
            extent: passport.luma_extent
        })
    );
}

#[test]
fn non_linear_object_is_refused() {
    let mut passport = identity();
    passport.object.modifier = 0x0200_0000_1040_1b04;
    assert_eq!(
        build(&passport),
        Err(DmaBufPlanarFrameError::UnsupportedModifier {
            modifier: 0x0200_0000_1040_1b04
        })
    );
}

#[test]
fn pitch_shorter_than_the_visible_row_is_refused_per_plane() {
    let mut passport = identity();
    passport.planes[2].pitch = 31;
    assert_eq!(
        build(&passport),
        Err(DmaBufPlanarFrameError::PitchShorterThanRow {
            plane: PlaneRole::ChromaV,
            pitch: 31,
            row_bytes: 32
        })
    );
}

#[test]
fn pitch_equal_to_the_row_is_accepted() {
    let mut passport = identity();
    passport.planes[0].pitch = 64;
    assert_eq!(build(&passport), Ok(()));
}

#[test]
fn offset_or_pitch_beyond_signed_32_bits_is_refused() {
    let mut passport = identity();
    passport.planes[1].offset = 0x8000_0000;
    assert!(matches!(
        build(&passport),
        Err(DmaBufPlanarFrameError::LocationOutOfRange {
            plane: PlaneRole::ChromaU,
            ..
        })
    ));
    let mut passport = identity();
    passport.planes[0].pitch = 0x8000_0000;
    assert!(matches!(
        build(&passport),
        Err(DmaBufPlanarFrameError::LocationOutOfRange {
            plane: PlaneRole::Luma,
            ..
        })
    ));
}

#[test]
fn plane_reaching_past_the_object_is_refused_and_the_exact_end_is_accepted() {
    let mut passport = identity();
    // V кончается ровно на конце объекта: годен.
    passport.object.size = 5120 + 64 * 15 + 32;
    assert_eq!(build(&passport), Ok(()));
    // Объект на байт короче — отказ с точным концом плоскости.
    passport.object.size = 5120 + 64 * 15 + 32 - 1;
    assert_eq!(
        build(&passport),
        Err(DmaBufPlanarFrameError::PlaneOutsideObject {
            plane: PlaneRole::ChromaV,
            end: 5120 + 64 * 15 + 32,
            object_size: 5120 + 64 * 15 + 32 - 1,
        })
    );
}
