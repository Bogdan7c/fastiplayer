//! Инварианты кэша импортов на fake-значениях: без GL, без драйвера.

use super::*;
use crate::dma_buf_frame::{DmaBufFrameFormat, DmaBufObjectIdentity, DmaBufPlaneArrangement};
use crate::planes::PlaneExtent;

fn format(width: u32) -> DmaBufFrameFormat {
    DmaBufFrameFormat {
        luma_extent: PlaneExtent { width, height: 16 },
        arrangement: DmaBufPlaneArrangement::ComposedSingleObject,
        modifier: 0,
    }
}

fn passport(resource_id: u64, inode: u64, width: u32) -> DmaBufBufferIdentity {
    DmaBufBufferIdentity {
        resource_id,
        objects: vec![DmaBufObjectIdentity {
            device: 1,
            inode,
            size: 4096,
            modifier: 0,
        }],
        format: format(width),
    }
}

fn cache(slots: usize) -> DmaBufImportCache<&'static str> {
    DmaBufImportCache::new(DmaBufImportCacheCapacity::matching_decoder_surface_pool(
        slots,
    ))
}

/// Показ кадра так, как его делает видеослой: сперва кэш, на промахе — импорт.
fn show(cache: &mut DmaBufImportCache<&'static str>, identity: &DmaBufBufferIdentity) -> bool {
    if cache.lookup(identity).is_some() {
        return true;
    }
    cache.insert(identity.clone(), "texture");
    false
}

#[test]
fn zero_slots_become_one() {
    assert_eq!(
        DmaBufImportCacheCapacity::matching_decoder_surface_pool(0).entries(),
        1
    );
    assert_eq!(cache(0).snapshot().capacity, 1);
}

#[test]
fn round_robin_over_capacity_imports_once_per_buffer() {
    let mut cache = cache(3);
    let buffers: Vec<_> = (0..3).map(|n| passport(10 + n, 100 + n, 64)).collect();
    for round in 0..3 {
        for buffer in &buffers {
            let hit = show(&mut cache, buffer);
            assert_eq!(hit, round > 0, "round {round}");
        }
    }
    let snapshot = cache.snapshot();
    assert_eq!(snapshot.imports, 3);
    assert_eq!(snapshot.hits, 6);
    assert_eq!(snapshot.entries, 3);
    assert_eq!(snapshot.evicted_capacity, 0);
}

#[test]
fn pool_larger_than_capacity_misses_every_time() {
    // Урок wgpu: кэш меньше пула на круговом обходе промахивается всегда.
    let mut cache = cache(2);
    let buffers: Vec<_> = (0..3).map(|n| passport(10 + n, 100 + n, 64)).collect();
    for _ in 0..3 {
        for buffer in &buffers {
            assert!(!show(&mut cache, buffer));
        }
    }
    let snapshot = cache.snapshot();
    assert_eq!((snapshot.imports, snapshot.hits), (9, 0));
    assert_eq!(snapshot.entries, 2);
    assert_eq!(snapshot.evicted_capacity, 7);
}

#[test]
fn format_change_removes_every_old_entry_before_the_new_import() {
    let mut cache = cache(3);
    for n in 0..3 {
        show(&mut cache, &passport(n, 100 + n, 64));
    }
    let incoming = passport(7, 200, 128);
    let removed = cache.prepare_for_insert(&incoming);
    assert_eq!(
        removed.len(),
        3,
        "all old-format entries go before the import"
    );
    assert_eq!(cache.snapshot().entries, 0);
    assert_eq!(cache.snapshot().evicted_format_change, 3);
    cache.insert(incoming, "new");
    assert_eq!(cache.snapshot().entries, 1);
}

#[test]
fn same_resource_with_new_kernel_buffer_replaces_the_entry() {
    let mut cache = cache(3);
    let old = passport(5, 100, 64);
    let other = passport(6, 101, 64);
    show(&mut cache, &old);
    show(&mut cache, &other);
    let recreated = passport(5, 999, 64);
    assert!(!show(&mut cache, &recreated), "new inode is a miss");
    let snapshot = cache.snapshot();
    assert_eq!(snapshot.replaced_same_resource, 1);
    assert_eq!(snapshot.entries, 2);
    assert!(
        !cache.contains(&old),
        "the stale texture must not live beside the new one"
    );
    assert!(cache.contains(&recreated));
    assert!(cache.contains(&other));
}

#[test]
fn full_cache_evicts_the_least_recently_used_entry() {
    let mut cache = cache(2);
    let first = passport(1, 101, 64);
    let second = passport(2, 102, 64);
    let third = passport(3, 103, 64);
    show(&mut cache, &first);
    show(&mut cache, &second);
    assert!(
        show(&mut cache, &first),
        "refresh `first`: `second` is now the oldest"
    );
    assert!(!show(&mut cache, &third));
    assert!(cache.contains(&first));
    assert!(cache.contains(&third));
    assert!(!cache.contains(&second));
    assert_eq!(cache.snapshot().evicted_capacity, 1);
}

#[test]
fn peek_does_not_count_hits_or_refresh_recency() {
    let mut cache = cache(2);
    let first = passport(1, 101, 64);
    let second = passport(2, 102, 64);
    show(&mut cache, &first);
    show(&mut cache, &second);
    assert!(cache.peek(&first).is_some());
    assert_eq!(cache.snapshot().hits, 0);
    // `first` остаётся самой давней: peek её не освежил.
    show(&mut cache, &passport(3, 103, 64));
    assert!(!cache.contains(&first));
}

#[test]
fn shrinking_capacity_returns_oldest_entries_and_grows_back_empty_handed() {
    let mut cache = cache(3);
    let buffers: Vec<_> = (0..3).map(|n| passport(n, 100 + n, 64)).collect();
    for buffer in &buffers {
        show(&mut cache, buffer);
    }
    let removed = cache.set_capacity(DmaBufImportCacheCapacity::matching_decoder_surface_pool(1));
    assert_eq!(removed.len(), 2);
    assert!(cache.contains(&buffers[2]));
    assert_eq!(cache.snapshot().capacity, 1);
    let removed = cache.set_capacity(DmaBufImportCacheCapacity::matching_decoder_surface_pool(5));
    assert!(removed.is_empty());
    assert_eq!(cache.snapshot().capacity, 5);
}

#[test]
fn drain_all_hands_back_every_value_and_counts_them() {
    let mut cache = cache(3);
    for n in 0..3 {
        show(&mut cache, &passport(n, 100 + n, 64));
    }
    assert_eq!(cache.drain_all().len(), 3);
    let snapshot = cache.snapshot();
    assert_eq!((snapshot.entries, snapshot.purged), (0, 3));
}

#[test]
fn import_failures_are_counted_separately_from_imports() {
    let mut cache = cache(2);
    cache.record_import_failure();
    let snapshot = cache.snapshot();
    assert_eq!((snapshot.imports, snapshot.import_failures), (0, 1));
}

#[test]
fn any_change_of_modifier_or_arrangement_resets_the_whole_cache() {
    let mut cache = cache(4);
    for n in 0..2 {
        show(&mut cache, &passport(n, 100 + n, 64));
    }
    let mut other_modifier = passport(7, 200, 64);
    other_modifier.format.modifier = 0x0200_0000_1040_1b04;
    assert_eq!(cache.prepare_for_insert(&other_modifier).len(), 2);
    cache.insert(other_modifier, "new");
    let mut other_arrangement = passport(8, 201, 64);
    other_arrangement.format.arrangement = DmaBufPlaneArrangement::SeparatePlanes;
    assert_eq!(cache.prepare_for_insert(&other_arrangement).len(), 1);
    assert_eq!(cache.snapshot().evicted_format_change, 3);
}
