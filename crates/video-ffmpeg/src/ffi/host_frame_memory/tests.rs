use std::sync::Arc;

use video_core::{HOST_IMPORTABLE_BLOCK_ALIGNMENT, HostFrameMemoryKind, HostImportableMemoryBlock};

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
    let pool = HostFrameBlockPool::new(HostFrameMemoryKind::Anonymous);

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
    let pool = HostFrameBlockPool::new(HostFrameMemoryKind::Anonymous);

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
    let pool = HostFrameBlockPool::new(HostFrameMemoryKind::Anonymous);
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
    let pool = HostFrameBlockPool::new(HostFrameMemoryKind::Anonymous);
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
    let pool = HostFrameBlockPool::new(HostFrameMemoryKind::Anonymous);
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
    let pool = HostFrameBlockPool::new(HostFrameMemoryKind::Anonymous);
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
    let pool = HostFrameBlockPool::new(HostFrameMemoryKind::Anonymous);
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

#[cfg(target_os = "linux")]
mod memfd_pool {
    use std::os::fd::AsRawFd;

    use super::*;

    fn memfd_pool() -> Arc<HostFrameBlockPool> {
        HostFrameBlockPool::new(HostFrameMemoryKind::SealedMemfd)
    }

    /// Читает `len` байт из memfd блока по `offset` обычным `pread` (мимо отображения).
    fn pread_from_block(block: &HostFrameBlock, offset: usize, len: usize) -> Vec<u8> {
        let region = block
            .sealed_memfd()
            .unwrap_or_else(|| unreachable!("memfd block reports its fd"));
        let mut bytes = vec![0_u8; len];
        // SAFETY: fd открыт и принадлежит блоку, буфер — наш Vec нужной длины.
        let read = unsafe {
            libc::pread(
                region.memfd().as_raw_fd(),
                bytes.as_mut_ptr().cast(),
                len,
                libc::off_t::try_from(offset).unwrap_or_else(|_| unreachable!("small test offset")),
            )
        };
        assert_eq!(
            usize::try_from(read).ok(),
            Some(len),
            "pread must return all bytes"
        );
        bytes
    }

    #[test]
    fn memfd_block_has_fd_zeroed_pages_and_required_seals() {
        let pool = memfd_pool();

        let block = pool
            .checkout(BLOCK)
            .unwrap_or_else(|| unreachable!("memfd allocation"));

        let region = block
            .sealed_memfd()
            .unwrap_or_else(|| unreachable!("memfd block must report its fd"));
        assert_eq!(region.offset(), 0);
        // SAFETY: fd открыт и принадлежит блоку.
        let seals = unsafe { libc::fcntl(region.memfd().as_raw_fd(), libc::F_GET_SEALS) };
        let required = libc::F_SEAL_SHRINK | libc::F_SEAL_GROW | libc::F_SEAL_SEAL;
        assert!(seals >= 0, "F_GET_SEALS failed");
        assert_eq!(seals & required, required, "seals {seals:#x}");
        assert_eq!(
            seals & libc::F_SEAL_WRITE,
            0,
            "decoder must still be able to write"
        );
        let mapped = block.region();
        assert_eq!(mapped.len(), BLOCK);
        assert!(
            (mapped.address().as_ptr() as usize).is_multiple_of(HOST_IMPORTABLE_BLOCK_ALIGNMENT)
        );
        assert!(
            pread_from_block(&block, 0, 4096)
                .iter()
                .all(|&byte| byte == 0)
        );
    }

    #[test]
    fn write_through_mapping_is_visible_through_memfd_read() {
        let pool = memfd_pool();
        let block = pool
            .checkout(BLOCK)
            .unwrap_or_else(|| unreachable!("memfd allocation"));

        // SAFETY: блок выдан этому тесту, FFmpeg его не держит; пишем внутри блока.
        unsafe {
            let base = block.address().as_ptr();
            base.add(5000).write(0xAB);
            base.add(BLOCK - 1).write(0xCD);
        }

        assert_eq!(pread_from_block(&block, 5000, 1), [0xAB]);
        assert_eq!(pread_from_block(&block, BLOCK - 1, 1), [0xCD]);
    }

    #[test]
    fn memfd_block_is_reused_and_dropping_everything_does_not_fail() {
        let pool = memfd_pool();
        let first = pool
            .checkout(BLOCK)
            .unwrap_or_else(|| unreachable!("memfd allocation"));
        let first_id = first.id();
        pool.give_back(first.address());
        drop(first);

        let second = pool
            .checkout(BLOCK)
            .unwrap_or_else(|| unreachable!("memfd allocation"));

        assert_eq!(second.id(), first_id, "memfd block must be reused");
        assert!(second.sealed_memfd().is_some());
        let stats = pool.stats();
        assert_eq!((stats.allocated, stats.reused), (1, 1));
        pool.give_back(second.address());
        pool.close();
        drop(second);
        drop(pool);
    }

    #[test]
    fn invalid_length_gives_no_memfd_block_and_no_accounting() {
        let pool = memfd_pool();

        assert!(pool.checkout(HOST_IMPORTABLE_BLOCK_ALIGNMENT + 1).is_none());
        assert!(pool.checkout(0).is_none());

        let stats = pool.stats();
        assert_eq!((stats.allocated, stats.checked_out), (0, 0));
    }

    #[test]
    fn system_refusal_returns_none_instead_of_panicking() {
        // Длина больше любого допустимого файла: ftruncate/mmap обязаны отказать.
        // Выровнена по странице, поэтому дойдёт именно до системы.
        let huge = (usize::MAX / 2) & !(HOST_IMPORTABLE_BLOCK_ALIGNMENT - 1);
        let pool = memfd_pool();

        assert!(pool.checkout(huge).is_none());

        let stats = pool.stats();
        assert_eq!((stats.allocated, stats.checked_out), (0, 0));
        // Отказ не ломает пул: нормальный блок после него выдаётся.
        assert!(pool.checkout(BLOCK).is_some());
    }

    #[test]
    fn anonymous_block_has_no_memfd() {
        let pool = HostFrameBlockPool::new(HostFrameMemoryKind::Anonymous);
        let block = pool
            .checkout(BLOCK)
            .unwrap_or_else(|| unreachable!("allocation"));

        assert!(block.sealed_memfd().is_none());
    }
}
