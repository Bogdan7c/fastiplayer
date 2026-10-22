//! Память host-planar кадра, которую видеокарта может прочитать без CPU-копии.
//!
//! Зачем (cpu-parity 08b). Обычный программный путь копирует каждую plane-у кадра
//! на CPU в staging-буфер, и только потом GPU переносит её в текстуру. Профиль
//! показал, что эта копия — 43–59 % главного потока на H.264 1080p60 / VP9 4K60.
//! Если декодер сам раскладывает кадр в выровненный по странице блок обычной памяти,
//! renderer может импортировать этот блок в GPU (Vulkan `VK_EXT_external_memory_host`)
//! и копировать прямо из него: CPU-копии нет, GPU-копия остаётся одна.
//!
//! Контракт здесь нейтральный: video-core не знает ни FFmpeg, ни Vulkan. Provider
//! (декодер) отдаёт блок и offset-ы plane-ов, renderer решает, может ли он его
//! импортировать; если нет — идёт обычным путём через staging.
//!
//! Две разные гарантии времени жизни:
//! - `Arc<dyn HostImportableMemoryBlock>` держит **выделенную память** (её можно
//!   импортировать и держать импорт сколько угодно долго);
//! - **содержимое** блока принадлежит кадру, пока жив его `HostPlanarFrameDescriptor`:
//!   после освобождения кадра декодер переиспользует блок под другой кадр. Поэтому
//!   GPU-копия обязана держать descriptor, пока не выполнится.

use std::fmt;
use std::ptr::NonNull;
use std::sync::Arc;
use std::sync::atomic::{AtomicU64, Ordering};

use anyhow::{Context, ensure};

use super::descriptor::{HostPlaneDescriptor, host_plane_block_len, host_plane_visible_row_bytes};

/// Выравнивание начала и длины импортируемого блока: одна страница памяти (4 КиБ).
///
/// Vulkan требует, чтобы адрес и размер импортируемой host-памяти были кратны
/// `minImportedHostPointerAlignment`; у AMD/Intel/NVIDIA это 4 КиБ. Если драйвер
/// потребует больше, renderer откажется от импорта и пойдёт через staging.
pub const HOST_IMPORTABLE_BLOCK_ALIGNMENT: usize = 4096;

/// Шаг строк (stride), при котором GPU копирует plane-у из импортированного блока
/// без перепаковки: 256 байт — требование к `bytes_per_row` копии буфер → текстура
/// в wgpu (`COPY_BYTES_PER_ROW_ALIGNMENT`). Декодер, который хочет отдавать
/// импортируемые кадры, раскладывает строки с таким шагом; renderer проверяет stride
/// каждой plane-ы и при несовпадении идёт через staging.
pub const HOST_IMPORTABLE_ROW_PITCH_ALIGNMENT: usize = 256;

/// Номер блока памяти, уникальный на весь процесс и никогда не повторяемый.
///
/// Renderer ключует кэш импортов по этому номеру, а не по адресу: адрес освобождённого
/// блока может достаться новому, и кэш по адресу перепутал бы импорты.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub struct HostMemoryBlockId(u64);

impl HostMemoryBlockId {
    /// Выдаёт следующий свободный номер (потокобезопасно).
    #[must_use]
    pub fn allocate() -> Self {
        // Relaxed достаточно: нужна только уникальность значения, не порядок с
        // другими операциями памяти.
        static NEXT_BLOCK_ID: AtomicU64 = AtomicU64::new(1);
        Self(NEXT_BLOCK_ID.fetch_add(1, Ordering::Relaxed))
    }

    /// Числовое значение для логов и диагностики.
    #[must_use]
    pub const fn get(self) -> u64 {
        self.0
    }
}

/// Адрес и длина блока памяти процесса.
///
/// Это только описание диапазона для импорта: по нему нельзя читать или писать
/// память из Rust (содержимое может параллельно писать декодер).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct HostMemoryRegion {
    /// Начало блока.
    address: NonNull<u8>,
    /// Длина блока в байтах.
    len: usize,
}

// SAFETY: `HostMemoryRegion` — пара «адрес + длина» без доступа к памяти: safe API не
// разыменовывает адрес. Передача значения между потоками не создаёт гонок данных.
unsafe impl Send for HostMemoryRegion {}
// SAFETY: см. `Send` выше — shared-ссылка тоже даёт только копию адреса и длины.
unsafe impl Sync for HostMemoryRegion {}

impl HostMemoryRegion {
    /// Описывает диапазон `[address, address + len)`.
    #[must_use]
    pub const fn new(address: NonNull<u8>, len: usize) -> Self {
        Self { address, len }
    }

    /// Начало блока (для передачи в GPU-импорт).
    #[must_use]
    pub const fn address(self) -> NonNull<u8> {
        self.address
    }

    /// Длина блока в байтах.
    #[must_use]
    pub const fn len(self) -> usize {
        self.len
    }

    /// `true`, если блок пустой (такой блок импортировать нельзя).
    #[must_use]
    pub const fn is_empty(self) -> bool {
        self.len == 0
    }
}

/// Блок обычной памяти процесса, который GPU может импортировать без CPU-копии.
///
/// # Safety
///
/// Реализация гарантирует:
/// - `region()` возвращает одно и то же значение всё время жизни блока;
/// - адрес и длина кратны [`HOST_IMPORTABLE_BLOCK_ALIGNMENT`], длина больше нуля;
/// - `[address, address + len)` — память, выделенная процессом (anonymous, не
///   отображение файла или устройства), и она остаётся выделенной, пока жива хотя бы
///   одна ссылка на блок: импорт GPU ссылается на эти страницы;
/// - `id()` не меняется и уникален среди всех блоков процесса.
pub unsafe trait HostImportableMemoryBlock: fmt::Debug + Send + Sync {
    /// Номер блока для кэша импортов.
    fn id(&self) -> HostMemoryBlockId;

    /// Диапазон памяти блока.
    fn region(&self) -> HostMemoryRegion;
}

/// Блок памяти кадра плюс offset начала каждой plane-ы от начала блока.
#[derive(Debug, Clone)]
pub struct HostPlanarImportableMemory {
    /// Выделенная память блока (держит её живой, но не содержимое кадра).
    block: Arc<dyn HostImportableMemoryBlock>,
    /// Offset начала plane-ы `i` от начала блока, в порядке `HostPlanarFrameDescriptor::planes`.
    plane_offsets: Vec<usize>,
}

impl HostPlanarImportableMemory {
    /// Собирает описание; проверку против plane-ов делает descriptor.
    #[must_use]
    pub fn new(block: Arc<dyn HostImportableMemoryBlock>, plane_offsets: Vec<usize>) -> Self {
        Self {
            block,
            plane_offsets,
        }
    }

    /// Блок памяти кадра.
    #[must_use]
    pub fn block(&self) -> &Arc<dyn HostImportableMemoryBlock> {
        &self.block
    }

    /// Offset начала plane-ы от начала блока.
    #[must_use]
    pub fn plane_offset(&self, plane_index: usize) -> Option<usize> {
        self.plane_offsets.get(plane_index).copied()
    }
}

/// Проверяет, что блок согласован с plane-ами кадра.
///
/// Renderer передаст offset-ы и stride прямо в GPU-копию, поэтому выход хоть одной
/// видимой строки за блок означал бы чтение чужой памяти — такой блок отвергается.
pub(super) fn validate_importable_memory(
    memory: &HostPlanarImportableMemory,
    planes: &[HostPlaneDescriptor],
) -> anyhow::Result<()> {
    let region = memory.block.region();
    let address = region.address().as_ptr() as usize;
    ensure!(
        !region.is_empty()
            && address.is_multiple_of(HOST_IMPORTABLE_BLOCK_ALIGNMENT)
            && region.len().is_multiple_of(HOST_IMPORTABLE_BLOCK_ALIGNMENT),
        "host-planar importable block {} is not page aligned: address {address:#x}, len {}",
        memory.block.id().get(),
        region.len()
    );
    ensure!(
        memory.plane_offsets.len() == planes.len(),
        "host-planar importable block has {} plane offsets for {} planes",
        memory.plane_offsets.len(),
        planes.len()
    );

    for (plane, &offset) in planes.iter().zip(&memory.plane_offsets) {
        let visible_row_bytes = host_plane_visible_row_bytes(plane)?;
        ensure!(
            plane.stride >= visible_row_bytes,
            "host-planar {:?} stride {} is smaller than visible row bytes {}",
            plane.role,
            plane.stride,
            visible_row_bytes
        );
        let block_len = host_plane_block_len(plane, visible_row_bytes)?;
        let plane_end = offset
            .checked_add(block_len)
            .context("host-planar importable plane end overflow")?;
        ensure!(
            plane_end <= region.len(),
            "host-planar {:?} plane [{offset}, {plane_end}) exceeds importable block of {} bytes",
            plane.role,
            region.len()
        );
    }

    Ok(())
}

#[cfg(test)]
mod tests;
