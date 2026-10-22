use std::alloc::{Layout, alloc_zeroed, dealloc};
use std::ptr::NonNull;
use std::sync::Arc;

use super::*;
use crate::resource::{HostPlanarFrameDescriptor, HostPlanarFrameOwner, HostPlaneRole};

/// Тестовый блок: выровненная по странице обнулённая память процесса.
#[derive(Debug)]
struct TestPageBlock {
    id: HostMemoryBlockId,
    address: NonNull<u8>,
    layout: Layout,
    /// Сдвиг адреса, который отдаёт `region()` — для проверки отказа на невыровненном блоке.
    reported_shift: usize,
}

impl TestPageBlock {
    fn new(len: usize) -> Self {
        Self::with_reported_shift(len, 0)
    }

    fn with_reported_shift(len: usize, reported_shift: usize) -> Self {
        let layout = Layout::from_size_align(len, HOST_IMPORTABLE_BLOCK_ALIGNMENT)
            .unwrap_or_else(|error| unreachable!("valid test layout: {error}"));
        // SAFETY: layout ненулевого размера.
        let raw = unsafe { alloc_zeroed(layout) };
        let address = NonNull::new(raw).unwrap_or_else(|| unreachable!("test allocation failed"));
        Self {
            id: HostMemoryBlockId::allocate(),
            address,
            layout,
            reported_shift,
        }
    }
}

// SAFETY: блок владеет своей allocation-ой эксклюзивно, а через safe API наружу уходит
// только адрес (без доступа к памяти); тесты не пишут в память блока.
unsafe impl Send for TestPageBlock {}
// SAFETY: см. `Send` — shared-ссылка даёт только id и диапазон.
unsafe impl Sync for TestPageBlock {}

impl Drop for TestPageBlock {
    fn drop(&mut self) {
        // SAFETY: память выделена `alloc_zeroed` с этим layout-ом и ещё не освобождена.
        unsafe { dealloc(self.address.as_ptr(), self.layout) };
    }
}

// SAFETY: адрес/длина постоянны, память живёт до Drop; `reported_shift` используется
// только в тесте отказа, где descriptor обязан отвергнуть блок до любого импорта.
unsafe impl HostImportableMemoryBlock for TestPageBlock {
    fn id(&self) -> HostMemoryBlockId {
        self.id
    }

    fn region(&self) -> HostMemoryRegion {
        // SAFETY (теста): сдвиг меньше длины выделения, адрес остаётся ненулевым.
        let address = unsafe { self.address.add(self.reported_shift) };
        HostMemoryRegion::new(address, self.layout.size())
    }
}

/// Owner, который отдаёт заданное описание импортируемой памяти.
#[derive(Debug)]
struct ImportableOwner {
    memory: HostPlanarImportableMemory,
}

impl HostPlanarFrameOwner for ImportableOwner {
    fn visible_row_bytes(
        &self,
        _plane_index: usize,
        _plane: &HostPlaneDescriptor,
        _row_index: u32,
        _visible_row_bytes: usize,
    ) -> anyhow::Result<&[u8]> {
        anyhow::bail!("rows are not read in host import tests")
    }

    fn visible_plane_block(
        &self,
        _plane_index: usize,
        _plane: &HostPlaneDescriptor,
        _block_len: usize,
    ) -> anyhow::Result<&[u8]> {
        anyhow::bail!("plane blocks are not read in host import tests")
    }

    fn importable_memory(
        &self,
        _planes: &[HostPlaneDescriptor],
    ) -> Option<HostPlanarImportableMemory> {
        Some(self.memory.clone())
    }
}

/// Plane-ы кадра 4×4 YUV 4:2:0 8-bit со stride 256 (как у нашего FFmpeg allocator-а).
fn yuv420_planes() -> Vec<HostPlaneDescriptor> {
    let plane = |role, size| HostPlaneDescriptor {
        role,
        offset: 0,
        stride: 256,
        visible_width: size,
        visible_height: size,
        bytes_per_sample: 1,
    };
    vec![
        plane(HostPlaneRole::Luma, 4),
        plane(HostPlaneRole::ChromaU, 2),
        plane(HostPlaneRole::ChromaV, 2),
    ]
}

fn descriptor_with(block: TestPageBlock, plane_offsets: Vec<usize>) -> HostPlanarFrameDescriptor {
    let memory = HostPlanarImportableMemory::new(Arc::new(block), plane_offsets);
    HostPlanarFrameDescriptor::new(Arc::new(ImportableOwner { memory }), yuv420_planes())
}

#[test]
fn owner_without_importable_memory_keeps_staging_path() {
    let descriptor =
        HostPlanarFrameDescriptor::from_owned_buffer(vec![0_u8; 4096], yuv420_planes());

    let memory = descriptor
        .importable_memory()
        .unwrap_or_else(|error| unreachable!("default owner must not fail: {error}"));

    assert!(
        memory.is_none(),
        "owned buffer must not claim importable memory"
    );
}

#[test]
fn consistent_block_is_returned_with_plane_offsets() {
    let block = TestPageBlock::new(4096);
    let block_id = block.id;
    let descriptor = descriptor_with(block, vec![0, 1024, 2048]);

    let memory = descriptor
        .importable_memory()
        .unwrap_or_else(|error| unreachable!("consistent block must validate: {error}"))
        .unwrap_or_else(|| unreachable!("owner provides memory"));

    assert_eq!(memory.block().id(), block_id);
    assert_eq!(memory.plane_offset(0), Some(0));
    assert_eq!(memory.plane_offset(1), Some(1024));
    assert_eq!(memory.plane_offset(2), Some(2048));
    assert_eq!(memory.plane_offset(3), None);
}

#[test]
fn plane_rows_outside_block_are_rejected() {
    // Последняя chroma-plane начинается за 200 байт до конца блока, а её видимые строки
    // занимают 256 + 2 = 258 байт: GPU-копия прочитала бы память за блоком.
    let descriptor = descriptor_with(TestPageBlock::new(4096), vec![0, 1024, 4096 - 200]);

    let error = descriptor
        .importable_memory()
        .err()
        .unwrap_or_else(|| unreachable!("plane outside block must be rejected"));

    assert!(
        error.to_string().contains("exceeds importable block"),
        "unexpected error: {error}"
    );
}

#[test]
fn plane_offset_count_must_match_planes() {
    let descriptor = descriptor_with(TestPageBlock::new(4096), vec![0, 1024]);

    let error = descriptor
        .importable_memory()
        .err()
        .unwrap_or_else(|| unreachable!("missing plane offset must be rejected"));

    assert!(
        error.to_string().contains("2 plane offsets for 3 planes"),
        "{error}"
    );
}

#[test]
fn unaligned_block_is_rejected() {
    let descriptor = descriptor_with(
        TestPageBlock::with_reported_shift(8192, 64),
        vec![0, 1024, 2048],
    );

    let error = descriptor
        .importable_memory()
        .err()
        .unwrap_or_else(|| unreachable!("unaligned block must be rejected"));

    assert!(error.to_string().contains("not page aligned"), "{error}");
}

#[test]
fn block_ids_are_unique() {
    let first = HostMemoryBlockId::allocate();
    let second = HostMemoryBlockId::allocate();

    assert_ne!(first, second);
    assert!(second.get() > first.get());
}
