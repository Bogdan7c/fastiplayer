//! Идентичность кадра декодера → паспорт буфера видеослоя.

use gles2_video::{DmaBufObjectIdentity, DmaBufPlaneArrangement, PlaneExtent};

use super::support::{
    HEIGHT, MODIFIER, WIDTH, composed_descriptor, object, p010_descriptor, separate_descriptor,
};
use crate::{Nv12DmaBufError, dma_buf_buffer_identity};

#[test]
fn composed_identity_becomes_a_single_object_passport() {
    let identity = composed_descriptor(11, 500).identity();
    let passport = dma_buf_buffer_identity(&identity).unwrap();
    assert_eq!(passport.resource_id, 11);
    assert_eq!(
        passport.objects,
        vec![DmaBufObjectIdentity {
            device: 7,
            inode: 500,
            size: u64::from(identity.objects[0].size),
            modifier: MODIFIER,
        }]
    );
    assert_eq!(passport.format.modifier, MODIFIER);
    assert_eq!(
        passport.format.arrangement,
        DmaBufPlaneArrangement::ComposedSingleObject
    );
    assert_eq!(
        passport.format.luma_extent,
        PlaneExtent {
            width: WIDTH,
            height: HEIGHT
        }
    );
}

#[test]
fn separate_identity_keeps_both_objects_in_order() {
    let passport = dma_buf_buffer_identity(&separate_descriptor(12, 700).identity()).unwrap();
    assert_eq!(
        passport.format.arrangement,
        DmaBufPlaneArrangement::SeparatePlanes
    );
    let inodes: Vec<u64> = passport.objects.iter().map(|object| object.inode).collect();
    assert_eq!(inodes, [700, 701]);
}

/// Другой буфер ядра под тем же номером поверхности — другой паспорт (кэш не перепутает).
#[test]
fn recreated_buffer_under_the_same_resource_id_has_a_different_passport() {
    let before = dma_buf_buffer_identity(&composed_descriptor(5, 100).identity()).unwrap();
    let after = dma_buf_buffer_identity(&composed_descriptor(5, 101).identity()).unwrap();
    assert_eq!(before.resource_id, after.resource_id);
    assert_ne!(before, after);
}

/// P010 получает свой отказ (решение владельца 33), а не общий «не тот формат».
#[test]
fn p010_identity_is_refused_with_its_own_reason() {
    let error = dma_buf_buffer_identity(&p010_descriptor(1, 1).identity()).unwrap_err();
    assert!(
        matches!(error, Nv12DmaBufError::TenBitHardwareNotDeclared { fourcc } if fourcc == 0x3031_3050),
        "{error:?}"
    );
}

#[test]
fn other_fourcc_is_not_nv12() {
    let mut descriptor = composed_descriptor(1, 1);
    descriptor.fourcc = 0x3432_5559;
    assert_eq!(
        dma_buf_buffer_identity(&descriptor.identity()).unwrap_err(),
        Nv12DmaBufError::NotNv12 {
            fourcc: 0x3432_5559
        }
    );
}

#[test]
fn objects_with_different_modifiers_are_refused() {
    let mut descriptor = separate_descriptor(1, 1);
    descriptor.objects[1] = object(9, 0x42);
    assert_eq!(
        dma_buf_buffer_identity(&descriptor.identity()).unwrap_err(),
        Nv12DmaBufError::ObjectModifiersDiffer {
            first: MODIFIER,
            other: 0x42
        }
    );
}

#[test]
fn identity_without_objects_is_refused() {
    let mut descriptor = composed_descriptor(1, 1);
    descriptor.objects.clear();
    assert_eq!(
        dma_buf_buffer_identity(&descriptor.identity()).unwrap_err(),
        Nv12DmaBufError::NoObjects
    );
}
