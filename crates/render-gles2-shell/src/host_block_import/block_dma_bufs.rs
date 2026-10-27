//! dma-buf блоков памяти декодера: один `UDMABUF_CREATE` на блок, а не на кадр.
//!
//! Блоков мало (их переиспользует пул декодера), а импорт на кадр на T410 стоил +0,7 мс
//! system-времени (стенд сессии 12). Поэтому dma-buf блока живёт в кэше, пока блок в ходу.
//!
//! Кэш ограничен: dma-buf держит страницы memfd сам, поэтому запись о блоке, который декодер уже
//! выбросил (смена размера кадра, закрытие декодера), удерживала бы его память. Ёмкость равна
//! пулу поверхностей плана импорта, старые записи вытесняются по LRU, а смена декодера очищает всё
//! ([`BlockDmaBufs::purge`]).

use std::collections::VecDeque;
use std::io;
use std::os::fd::{AsFd, BorrowedFd, OwnedFd};

use video_core::{HostMemoryBlockId, SealedMemfdRegion};

use super::kernel::{KernelObjectIdentity, UdmabufDevice, kernel_object_identity};

/// Сколько dma-buf блоков держим одновременно (не меньше одного).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) struct BlockDmaBufCapacity(usize);

impl BlockDmaBufCapacity {
    /// До установки импорта: столько же, сколько у кэша импортов `gles2-video` по умолчанию.
    pub(crate) const DEFAULT: Self = Self(8);

    /// Столько же, сколько поверхностей в пуле плана импорта.
    pub(crate) const fn matching_surface_pool(slots: usize) -> Self {
        Self(if slots == 0 { 1 } else { slots })
    }

    pub(crate) const fn get(self) -> usize {
        self.0
    }
}

/// dma-buf одного блока и его паспорт по `fstat`.
#[derive(Debug)]
pub(super) struct BlockDmaBuf {
    dma_buf: OwnedFd,
    object: KernelObjectIdentity,
}

impl BlockDmaBuf {
    pub(super) fn fd(&self) -> BorrowedFd<'_> {
        self.dma_buf.as_fd()
    }

    pub(super) const fn object(&self) -> KernelObjectIdentity {
        self.object
    }
}

/// Что сделал запрос dma-buf блока.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) enum BlockDmaBufLookup {
    /// dma-buf уже был.
    Cached,
    /// Создан новый (`UDMABUF_CREATE`).
    Created,
}

/// LRU блок → dma-buf (в конце очереди — самый свежий).
#[derive(Debug)]
pub(super) struct BlockDmaBufs {
    entries: VecDeque<(HostMemoryBlockId, BlockDmaBuf)>,
    capacity: BlockDmaBufCapacity,
}

impl BlockDmaBufs {
    pub(super) const fn new() -> Self {
        Self {
            entries: VecDeque::new(),
            capacity: BlockDmaBufCapacity::DEFAULT,
        }
    }

    #[cfg(test)]
    pub(super) fn len(&self) -> usize {
        self.entries.len()
    }

    /// Новая ёмкость; лишние старые записи закрываются сразу.
    pub(super) fn set_capacity(&mut self, capacity: BlockDmaBufCapacity) {
        self.capacity = capacity;
        self.evict_over_capacity(0);
    }

    /// Закрыть все dma-buf (смена декодера: блоков прошлого декодера больше нет).
    pub(super) fn purge(&mut self) {
        self.entries.clear();
    }

    /// dma-buf блока `block`: из кэша или новый над `region_len` байтами memfd.
    pub(super) fn get_or_create(
        &mut self,
        device: &UdmabufDevice,
        block: HostMemoryBlockId,
        memfd: SealedMemfdRegion<'_>,
        region_len: usize,
    ) -> io::Result<(&BlockDmaBuf, BlockDmaBufLookup)> {
        if let Some(position) = self.entries.iter().position(|(id, _)| *id == block) {
            // Освежить: перенести в конец очереди.
            if let Some(entry) = self.entries.remove(position) {
                self.entries.push_back(entry);
            }
            return self
                .newest()
                .map(|entry| (entry, BlockDmaBufLookup::Cached))
                .ok_or_else(|| io::Error::other("block dma-buf cache lost an entry"));
        }
        let size = u64::try_from(region_len)
            .map_err(|_| io::Error::new(io::ErrorKind::InvalidInput, "block too large"))?;
        let dma_buf = device.wrap_sealed_memfd(memfd.memfd(), memfd.offset(), size)?;
        let object = kernel_object_identity(dma_buf.as_fd())?;
        // Место освобождается ДО вставки: ёмкость не превышается даже на миг.
        self.evict_over_capacity(1);
        self.entries
            .push_back((block, BlockDmaBuf { dma_buf, object }));
        self.newest()
            .map(|entry| (entry, BlockDmaBufLookup::Created))
            .ok_or_else(|| io::Error::other("block dma-buf cache lost an entry"))
    }

    fn newest(&self) -> Option<&BlockDmaBuf> {
        self.entries.back().map(|(_, entry)| entry)
    }

    /// Вытеснить старые записи так, чтобы осталось место под `reserve` новых.
    fn evict_over_capacity(&mut self, reserve: usize) {
        let limit = self.capacity.get().saturating_sub(reserve);
        while self.entries.len() > limit {
            self.entries.pop_front();
        }
    }
}
