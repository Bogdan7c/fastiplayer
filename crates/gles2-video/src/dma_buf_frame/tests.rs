//! Проверки кадра NV12 до драйвера: каждая ошибка — отдельный вариант, годный кадр проходит.

use std::fs::File;
use std::os::fd::AsFd;

use super::*;

const LUMA: PlaneExtent = PlaneExtent {
    width: 32,
    height: 16,
};

fn plane(fd: &File, drm_format: u32, pitch: u32, offset: u64) -> DmaBufPlane<'_> {
    DmaBufPlane {
        fd: fd.as_fd(),
        offset,
        pitch,
        modifier: 0,
        drm_format,
    }
}

fn passport() -> DmaBufBufferIdentity {
    DmaBufBufferIdentity {
        resource_id: 1,
        objects: vec![DmaBufObjectIdentity {
            device: 1,
            inode: 2,
            size: 8192,
            modifier: 0,
        }],
        format: DmaBufFrameFormat {
            luma_extent: LUMA,
            arrangement: DmaBufPlaneArrangement::ComposedSingleObject,
            modifier: 0,
        },
    }
}

fn build(
    luma_format: u32,
    luma_pitch: u32,
    chroma_format: u32,
    chroma_pitch: u32,
    chroma_offset: u64,
) -> Result<(), DmaBufFrameError> {
    let file = File::open("/dev/null").expect("/dev/null opens");
    let luma = plane(&file, luma_format, luma_pitch, 0);
    let chroma = plane(&file, chroma_format, chroma_pitch, chroma_offset);
    let identity = passport();
    DmaBufNv12Frame::new(VideoFrameKey(1), &identity, LUMA, luma, chroma).map(|_| ())
}

#[test]
fn valid_frame_exposes_derived_chroma_extent_and_offsets() {
    let file = File::open("/dev/null").expect("/dev/null opens");
    let luma = plane(&file, DrmFourcc::R8.0, 64, 0);
    let chroma = plane(&file, DrmFourcc::GR88.0, 64, 4096);
    let identity = passport();
    let odd = PlaneExtent {
        width: 33,
        height: 17,
    };
    // Нечётный размер яркости: цветность округляется вверх, 17 × 9 текселей.
    let mut odd_identity = identity.clone();
    odd_identity.format.luma_extent = odd;
    let frame = DmaBufNv12Frame::new(VideoFrameKey(7), &odd_identity, odd, luma, chroma)
        .expect("odd-sized frame is valid");
    assert_eq!(
        frame.chroma_extent(),
        PlaneExtent {
            width: 17,
            height: 9
        }
    );
    assert_eq!((frame.luma_offset(), frame.chroma_offset()), (0, 4096));
    assert_eq!(frame.key(), VideoFrameKey(7));
}

#[test]
fn good_frame_passes() {
    assert_eq!(
        build(DrmFourcc::R8.0, 64, DrmFourcc::GR88.0, 64, 4096),
        Ok(())
    );
}

#[test]
fn wrong_plane_formats_are_named_per_plane() {
    assert_eq!(
        build(DrmFourcc::GR88.0, 64, DrmFourcc::GR88.0, 64, 0),
        Err(DmaBufFrameError::WrongPlaneFormat {
            plane: DmaBufPlaneRole::Luma,
            expected: DrmFourcc::R8.0,
            actual: DrmFourcc::GR88.0
        })
    );
    assert_eq!(
        build(DrmFourcc::R8.0, 64, DrmFourcc::R8.0, 64, 0),
        Err(DmaBufFrameError::WrongPlaneFormat {
            plane: DmaBufPlaneRole::Chroma,
            expected: DrmFourcc::GR88.0,
            actual: DrmFourcc::R8.0
        })
    );
}

#[test]
fn pitch_must_cover_the_visible_row_of_each_plane() {
    assert_eq!(
        build(DrmFourcc::R8.0, 31, DrmFourcc::GR88.0, 64, 0),
        Err(DmaBufFrameError::PitchShorterThanRow {
            plane: DmaBufPlaneRole::Luma,
            pitch: 31,
            row_bytes: 32
        })
    );
    // 16 текселей цветности по 2 байта — 32 байта в строке.
    assert_eq!(
        build(DrmFourcc::R8.0, 64, DrmFourcc::GR88.0, 31, 0),
        Err(DmaBufFrameError::PitchShorterThanRow {
            plane: DmaBufPlaneRole::Chroma,
            pitch: 31,
            row_bytes: 32
        })
    );
    assert_eq!(build(DrmFourcc::R8.0, 32, DrmFourcc::GR88.0, 32, 0), Ok(()));
}

#[test]
fn offset_wider_than_u32_is_rejected() {
    let too_far = u64::from(u32::MAX) + 1;
    assert_eq!(
        build(DrmFourcc::R8.0, 64, DrmFourcc::GR88.0, 64, too_far),
        Err(DmaBufFrameError::OffsetTooLarge {
            plane: DmaBufPlaneRole::Chroma,
            offset: too_far
        })
    );
}

#[test]
fn empty_extent_is_rejected() {
    let file = File::open("/dev/null").expect("/dev/null opens");
    let luma = plane(&file, DrmFourcc::R8.0, 64, 0);
    let chroma = plane(&file, DrmFourcc::GR88.0, 64, 0);
    let identity = passport();
    let empty = PlaneExtent {
        width: 0,
        height: 16,
    };
    assert_eq!(
        DmaBufNv12Frame::new(VideoFrameKey(1), &identity, empty, luma, chroma).map(|_| ()),
        Err(DmaBufFrameError::EmptyExtent { extent: empty })
    );
}

#[test]
fn identity_with_another_luma_extent_is_rejected() {
    let file = File::open("/dev/null").expect("/dev/null opens");
    let luma = plane(&file, DrmFourcc::R8.0, 64, 0);
    let chroma = plane(&file, DrmFourcc::GR88.0, 64, 4096);
    let mut identity = passport();
    identity.format.luma_extent = PlaneExtent {
        width: 64,
        height: 16,
    };
    assert_eq!(
        DmaBufNv12Frame::new(VideoFrameKey(1), &identity, LUMA, luma, chroma).map(|_| ()),
        Err(DmaBufFrameError::LumaExtentDiffersFromIdentity {
            passport: identity.format.luma_extent,
            frame: LUMA
        })
    );
}

#[test]
fn plane_modifier_must_match_the_identity_modifier() {
    let file = File::open("/dev/null").expect("/dev/null opens");
    let luma = plane(&file, DrmFourcc::R8.0, 64, 0);
    let mut chroma = plane(&file, DrmFourcc::GR88.0, 64, 4096);
    chroma.modifier = 0x1234;
    assert_eq!(
        DmaBufNv12Frame::new(VideoFrameKey(1), &passport(), LUMA, luma, chroma).map(|_| ()),
        Err(DmaBufFrameError::PlaneModifierDiffersFromIdentity {
            plane: DmaBufPlaneRole::Chroma,
            passport: 0,
            actual: 0x1234
        })
    );
}

#[test]
fn offsets_and_pitches_are_not_part_of_the_identity() {
    // Один паспорт годится кадрам с разными смещениями и шагами: они свойство экспорта.
    assert_eq!(
        build(DrmFourcc::R8.0, 64, DrmFourcc::GR88.0, 64, 4096),
        Ok(())
    );
    assert_eq!(
        build(DrmFourcc::R8.0, 128, DrmFourcc::GR88.0, 128, 8192),
        Ok(())
    );
}
