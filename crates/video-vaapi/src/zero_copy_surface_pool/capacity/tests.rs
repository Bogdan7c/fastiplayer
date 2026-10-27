//! Рост ёмкости слотов `ZeroCopySurfacePool` без затрагивания занятых слотов.

use super::*;
use crate::zero_copy_surface_pool::ZeroCopyImportReuseContract;

fn pool(capacity: usize) -> ZeroCopySurfacePool {
    ZeroCopySurfacePool::new(
        capacity,
        ZeroCopyImportReuseContract::vaapi_vulkan_dma_buf(),
    )
}

#[test]
fn raise_grows_capacity_and_reports_previous_value() {
    let mut surface_pool = pool(12);

    let change = surface_pool.raise_capacity_to_follow_surfaces(14);

    assert_eq!(
        change,
        ZeroCopyCapacityChange::Raised {
            previous: 12,
            current: 14
        }
    );
    assert_eq!(surface_pool.stats().capacity, 14);
}

#[test]
fn raise_never_shrinks_and_equal_value_is_unchanged() {
    let mut surface_pool = pool(12);

    assert_eq!(
        surface_pool.raise_capacity_to_follow_surfaces(12),
        ZeroCopyCapacityChange::Unchanged { capacity: 12 }
    );
    assert_eq!(
        surface_pool.raise_capacity_to_follow_surfaces(4),
        ZeroCopyCapacityChange::Unchanged { capacity: 12 }
    );
    assert_eq!(surface_pool.stats().capacity, 12);
}
