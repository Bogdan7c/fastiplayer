use std::sync::Arc;

use video_core::{HOST_IMPORTABLE_BLOCK_ALIGNMENT, HostImportableMemoryBlock};

use super::*;

/// Длина тестового блока (одна большая страница).
const BLOCK: usize = HOST_FRAME_BLOCK_ALIGNMENT;

/// Plane-ы 4K YUV 4:2:0 8-bit с шагом строк 3840/2304 (как даёт наш `get_buffer2`).
fn yuv420_4k_plane_sizes() -> [usize; 3] {
    [3840 * 2176, 2304 * 1088, 2304 * 1088]
}

#[test]
fn layout_aligns_planes_and_keeps_tail_slack() {
    let layout = HostFrameBlockLayout::for_plane_sizes(&yuv420_4k_plane_sizes())
        .unwrap_or_else(|| unreachable!("valid plane sizes"));

    let offsets = layout.plane_offsets();
    assert_eq!(offsets.len(), 3);
    assert_eq!(offsets[0], 0);
    for (index, &offset) in offsets.iter().enumerate() {
        assert!(
            offset.is_multiple_of(HOST_FRAME_PLANE_ALIGNMENT),
            "plane {index} at {offset}"
        );
    }
    // Между концом plane-ы и началом следующей — минимум запас для SIMD-чтения.
    let sizes = yuv420_4k_plane_sizes();
    assert!(offsets[1] >= sizes[0] + HOST_FRAME_PLANE_TAIL_SLACK);
    assert!(offsets[2] >= offsets[1] + sizes[1] + HOST_FRAME_PLANE_TAIL_SLACK);
    let used_len = offsets[2] + sizes[2] + HOST_FRAME_PLANE_TAIL_SLACK;
    assert!(layout.block_len() >= used_len);
    assert!(
        layout
            .block_len()
            .is_multiple_of(HOST_IMPORTABLE_BLOCK_ALIGNMENT)
    );
    // Округление только до страницы: хвост до 2 МиБ не добивается (лишний RSS).
    assert!(layout.block_len() - used_len < HOST_IMPORTABLE_BLOCK_ALIGNMENT);
}

#[test]
fn layout_rejects_empty_zero_and_too_many_planes() {
    assert!(HostFrameBlockLayout::for_plane_sizes(&[]).is_none());
    assert!(HostFrameBlockLayout::for_plane_sizes(&[256, 0, 64]).is_none());
    assert!(HostFrameBlockLayout::for_plane_sizes(&[1, 1, 1, 1, 1]).is_none());
    assert!(HostFrameBlockLayout::for_plane_sizes(&[usize::MAX]).is_none());
}

#[test]
fn checked_out_block_is_huge_page_aligned_zeroed_and_findable_by_address() {
    let pool = HostFrameBlockPool::new();

    let block = pool
        .checkout(BLOCK)
        .unwrap_or_else(|| unreachable!("allocation of one block"));

    let region = block.region();
    let address = region.address().as_ptr() as usize;
    assert_eq!(region.len(), BLOCK);
    assert!(address.is_multiple_of(HOST_FRAME_BLOCK_ALIGNMENT));
    // SAFETY: блок выдан этому тесту, FFmpeg его не держит; читаем всё выделение.
    let bytes = unsafe { std::slice::from_raw_parts(region.address().as_ptr(), region.len()) };
    assert!(bytes.iter().all(|&byte| byte == 0), "block must be zeroed");
    let found = pool
        .checked_out_block(address)
        .unwrap_or_else(|| unreachable!("checked-out block must be findable"));
    assert!(Arc::ptr_eq(&found, &block));
    assert!(pool.checked_out_block(address + 64).is_none());
}

#[test]
fn block_length_must_be_whole_pages() {
    let pool = HostFrameBlockPool::new();

    assert!(pool.checkout(HOST_IMPORTABLE_BLOCK_ALIGNMENT + 1).is_none());
    assert!(pool.checkout(0).is_none());
    let one_page = pool
        .checkout(HOST_IMPORTABLE_BLOCK_ALIGNMENT)
        .unwrap_or_else(|| unreachable!("one page block"));
    // Начало блока — на границе большой страницы даже у маленького блока.
    let address = one_page.region().address().as_ptr() as usize;
    assert!(address.is_multiple_of(HOST_FRAME_BLOCK_ALIGNMENT));
}

#[test]
fn returned_block_is_reused_for_same_size() {
    let pool = HostFrameBlockPool::new();
    let first = pool
        .checkout(BLOCK)
        .unwrap_or_else(|| unreachable!("allocation"));
    let first_id = first.id();
    pool.give_back(first.address());
    drop(first);

    let second = pool
        .checkout(BLOCK)
        .unwrap_or_else(|| unreachable!("allocation"));

    assert_eq!(second.id(), first_id, "same-size block must be reused");
    let stats = pool.stats();
    assert_eq!(
        (stats.allocated, stats.reused, stats.checked_out, stats.free),
        (1, 1, 1, 0)
    );
}

#[test]
fn new_frame_size_drops_free_blocks_of_old_size() {
    let pool = HostFrameBlockPool::new();
    let small = pool
        .checkout(BLOCK)
        .unwrap_or_else(|| unreachable!("allocation"));
    pool.give_back(small.address());
    assert_eq!(pool.stats().free, 1);

    let large = pool
        .checkout(2 * BLOCK)
        .unwrap_or_else(|| unreachable!("allocation"));

    assert_ne!(large.id(), small.id());
    let stats = pool.stats();
    assert_eq!((stats.allocated, stats.reused, stats.free), (2, 0, 0));
    // Блок текущего размера, вернувшийся после смены размера, становится свободным.
    let current = pool
        .checkout(2 * BLOCK)
        .unwrap_or_else(|| unreachable!("allocation"));
    pool.give_back(current.address());
    assert_eq!(pool.stats().free, 1);
}

#[test]
fn closed_pool_hands_out_nothing_and_forgets_returned_blocks() {
    let pool = HostFrameBlockPool::new();
    let block = pool
        .checkout(BLOCK)
        .unwrap_or_else(|| unreachable!("allocation"));

    pool.close();
    pool.give_back(block.address());

    assert!(pool.checkout(BLOCK).is_none());
    let stats = pool.stats();
    assert_eq!((stats.checked_out, stats.free), (0, 0));
    // Кадр, который ещё держит блок, остаётся валидным: память жива, пока жива `Arc`.
    assert_eq!(block.region().len(), BLOCK);
}

#[test]
fn free_list_is_capped() {
    let pool = HostFrameBlockPool::new();
    let blocks: Vec<_> = (0..MAX_FREE_HOST_FRAME_BLOCKS + 3)
        .map(|_| {
            pool.checkout(BLOCK)
                .unwrap_or_else(|| unreachable!("allocation"))
        })
        .collect();

    for block in &blocks {
        pool.give_back(block.address());
    }

    assert_eq!(pool.stats().free, MAX_FREE_HOST_FRAME_BLOCKS);
    assert_eq!(pool.stats().checked_out, 0);
}

#[test]
fn unknown_address_return_is_ignored() {
    let pool = HostFrameBlockPool::new();
    let block = pool
        .checkout(BLOCK)
        .unwrap_or_else(|| unreachable!("allocation"));
    let foreign_byte = 0_u8;

    pool.give_back(std::ptr::NonNull::from(&foreign_byte));

    assert_eq!(
        pool.stats().checked_out,
        1,
        "foreign address must not release our block"
    );
    drop(block);
}
