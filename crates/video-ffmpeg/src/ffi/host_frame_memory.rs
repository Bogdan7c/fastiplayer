//! Блоки памяти, в которые FFmpeg декодирует кадры, чтобы GPU читал их без CPU-копии.
//!
//! Зачем (cpu-parity 08b). По умолчанию libavcodec выделяет plane-ы кадра сам
//! (`avcodec_default_get_buffer2`): адреса выровнены на 64 байта, stride — на 32/64.
//! Такой кадр renderer может только скопировать на CPU в staging (43–59 % главного
//! потока). Если же весь кадр лежит в одном блоке, выровненном по странице, а stride
//! кратен 256, renderer импортирует блок в GPU и копирует прямо из него.
//!
//! Модуль — чистый Rust без FFmpeg: раскладка plane-ов в блоке и пул блоков.
//! Связка с FFmpeg (`get_buffer2`, `av_buffer_create`) живёт в `direct_frame_buffers`.
//!
//! Владение:
//! - блок — `Arc<HostFrameBlock>`; память освобождается, когда отпущена последняя
//!   ссылка (пул, кадр FFmpeg или импорт в renderer-е);
//! - пул помнит выданные блоки (`checked_out`), чтобы по адресу plane-ы кадра найти
//!   его блок, и свободные (`free`) для переиспользования, как `AVBufferPool` FFmpeg.

use std::alloc::{Layout, alloc_zeroed, dealloc};
use std::collections::HashMap;
use std::ptr::NonNull;
use std::sync::{Arc, Mutex, MutexGuard, PoisonError};

#[cfg(target_os = "linux")]
use video_core::SealedMemfdRegion;
use video_core::{
    HOST_IMPORTABLE_BLOCK_ALIGNMENT, HostFrameMemoryKind, HostImportableMemoryBlock,
    HostMemoryBlockId, HostMemoryRegion,
};

#[cfg(target_os = "linux")]
mod sealed_memfd;
#[cfg(target_os = "linux")]
use sealed_memfd::SealedMemfdMapping;

/// Выравнивание начала блока: 2 МиБ — размер «большой страницы» x86-64.
///
/// Замер cpu-parity 08b (VP9 4K60 / H.264 1080p60, Ryzen 7 7840HS, RADV): импорт блока
/// в Vulkan — это `userptr` amdgpu, ядро закрепляет его страницы при импорте и проверяет
/// их на каждом submit. С блоками, выровненными по 4 КиБ, это стоило главному потоку
/// 1,8–2,7 % ядра в system time; с блоками по 2 МиБ (целиком на transparent huge pages)
/// — 1,4–1,65 %, и декодер тоже чуть дешевле (меньше промахов TLB). Длина блока
/// округляется только до страницы: добивать хвост до 2 МиБ значило бы держать в RSS до
/// 2 МиБ лишнего на кадр (блок обнуляется целиком), а выигрыш даёт выравнивание начала.
pub(crate) const HOST_FRAME_BLOCK_ALIGNMENT: usize = 2 * 1024 * 1024;

const _: () = assert!(HOST_FRAME_BLOCK_ALIGNMENT.is_multiple_of(HOST_IMPORTABLE_BLOCK_ALIGNMENT));

/// Выравнивание начала каждой plane-ы внутри блока.
///
/// FFmpeg требует выравнивания plane-ов «по максимуму для CPU» (`av_cpu_max_align`,
/// 64 байта на AVX-512); 256 перекрывает это и совпадает с шагом строк для GPU.
pub(crate) const HOST_FRAME_PLANE_ALIGNMENT: usize = 256;

/// Запас байт после каждой plane-ы.
///
/// Повторяет `16 + STRIDE_ALIGN - 1` из `update_frame_pool` FFmpeg 8.1 (`STRIDE_ALIGN`
/// до 64 на AVX-512): SIMD-код декодеров читает чуть дальше конца plane-ы.
pub(crate) const HOST_FRAME_PLANE_TAIL_SLACK: usize = 16 + 64;

/// Сколько свободных блоков пул держит для переиспользования.
///
/// В обычной работе свободных блоков не больше пика одновременно живых кадров (~25 на
/// 4K при 14 потоках декодера). Потолок только страхует от патологического случая,
/// чтобы пул не держал память, которую уже никто не попросит.
pub(crate) const MAX_FREE_HOST_FRAME_BLOCKS: usize = 64;

/// Раскладка plane-ов кадра в одном блоке.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) struct HostFrameBlockLayout {
    /// Offset начала plane-ы от начала блока; используются первые `plane_count`.
    plane_offsets: [usize; 4],
    /// Сколько plane-ов у кадра (1–4).
    plane_count: usize,
    /// Длина блока, кратная странице.
    block_len: usize,
}

impl HostFrameBlockLayout {
    /// Раскладывает plane-ы заданных размеров (байт) подряд в одном блоке.
    ///
    /// Каждая plane начинается с границы [`HOST_FRAME_PLANE_ALIGNMENT`] и получает
    /// хвостовой запас [`HOST_FRAME_PLANE_TAIL_SLACK`]; длина блока округляется до
    /// страницы ([`HOST_IMPORTABLE_BLOCK_ALIGNMENT`]). `None` — нет plane-ов, больше 4, нулевая plane или переполнение.
    pub(crate) fn for_plane_sizes(plane_sizes: &[usize]) -> Option<Self> {
        if plane_sizes.is_empty() || plane_sizes.len() > 4 {
            return None;
        }
        let mut plane_offsets = [0_usize; 4];
        let mut next_offset = 0_usize;
        for (plane_index, &plane_size) in plane_sizes.iter().enumerate() {
            if plane_size == 0 {
                return None;
            }
            let offset = align_up(next_offset, HOST_FRAME_PLANE_ALIGNMENT)?;
            plane_offsets[plane_index] = offset;
            next_offset = offset
                .checked_add(plane_size)?
                .checked_add(HOST_FRAME_PLANE_TAIL_SLACK)?;
        }
        Some(Self {
            plane_offsets,
            plane_count: plane_sizes.len(),
            block_len: align_up(next_offset, HOST_IMPORTABLE_BLOCK_ALIGNMENT)?,
        })
    }

    /// Offset-ы plane-ов от начала блока.
    pub(crate) fn plane_offsets(&self) -> &[usize] {
        &self.plane_offsets[..self.plane_count]
    }

    /// Длина блока в байтах (кратна странице).
    pub(crate) const fn block_len(&self) -> usize {
        self.block_len
    }
}

/// Округляет `value` вверх до кратного `alignment` (степень двойки не требуется).
fn align_up(value: usize, alignment: usize) -> Option<usize> {
    value.checked_next_multiple_of(alignment)
}

/// Как выделена память блока; определяет, чем её освобождать.
#[derive(Debug)]
enum HostFrameBlockBacking {
    /// Обнулённая анонимная память, выровненная на 2 МиБ.
    Anonymous {
        /// Начало выделения.
        address: NonNull<u8>,
        /// Размер (кратен странице) и выравнивание выделения.
        layout: Layout,
    },
    /// `MAP_SHARED`-отображение запечатанного memfd (для dma-buf GLES2).
    #[cfg(target_os = "linux")]
    SealedMemfd(SealedMemfdMapping),
}

/// Один блок памяти кадра: обнулённая память, анонимная (на 2 МиБ) или из memfd.
#[derive(Debug)]
pub(crate) struct HostFrameBlock {
    /// Номер блока для кэша импортов renderer-а.
    id: HostMemoryBlockId,
    /// Выделение блока.
    backing: HostFrameBlockBacking,
}

// SAFETY: блок эксклюзивно владеет своим выделением и не даёт доступа к памяти через
// safe API (только адрес и длину). Запись в память делает FFmpeg через указатели,
// которые мы отдаём в `get_buffer2`; синхронизацию обеспечивает refcount AVBuffer.
// Отображение memfd (`SealedMemfdMapping`) устроено так же: fd и адрес неизменны.
unsafe impl Send for HostFrameBlock {}
// SAFETY: см. `Send` — через `&HostFrameBlock` доступны только неизменяемые id/адрес/длина/fd.
unsafe impl Sync for HostFrameBlock {}

impl HostFrameBlock {
    /// Выделяет обнулённый блок длиной `block_len` (кратной странице) заданного вида.
    ///
    /// Обнуление — как у FFmpeg (`av_buffer_allocz`): декодер в редких ошибках может
    /// отдать кадр с неинициализированными участками, и там не должно быть чужих данных
    /// (новые страницы memfd ядро тоже отдаёт нулями).
    /// `Err` — длина не кратна странице или система отказала в памяти/memfd.
    fn allocate_zeroed(
        kind: HostFrameMemoryKind,
        block_len: usize,
    ) -> Result<Self, HostFrameBlockAllocationError> {
        if block_len == 0 || !block_len.is_multiple_of(HOST_IMPORTABLE_BLOCK_ALIGNMENT) {
            return Err(HostFrameBlockAllocationError::InvalidLength { block_len });
        }
        let backing = match kind {
            HostFrameMemoryKind::Anonymous => Self::allocate_anonymous(block_len)?,
            HostFrameMemoryKind::SealedMemfd => Self::allocate_sealed_memfd(block_len)?,
        };
        Ok(Self {
            id: HostMemoryBlockId::allocate(),
            backing,
        })
    }

    /// Анонимная память, выровненная на [`HOST_FRAME_BLOCK_ALIGNMENT`].
    fn allocate_anonymous(
        block_len: usize,
    ) -> Result<HostFrameBlockBacking, HostFrameBlockAllocationError> {
        let layout = Layout::from_size_align(block_len, HOST_FRAME_BLOCK_ALIGNMENT)
            .map_err(|_| HostFrameBlockAllocationError::InvalidLength { block_len })?;
        // SAFETY: layout ненулевого размера; результат проверяется на null ниже.
        let raw = unsafe { alloc_zeroed(layout) };
        let address =
            NonNull::new(raw).ok_or(HostFrameBlockAllocationError::OutOfMemory { block_len })?;
        Ok(HostFrameBlockBacking::Anonymous { address, layout })
    }

    /// Запечатанный memfd; вне Linux вида памяти нет.
    #[cfg(target_os = "linux")]
    fn allocate_sealed_memfd(
        block_len: usize,
    ) -> Result<HostFrameBlockBacking, HostFrameBlockAllocationError> {
        SealedMemfdMapping::create(block_len)
            .map(HostFrameBlockBacking::SealedMemfd)
            .map_err(HostFrameBlockAllocationError::SealedMemfd)
    }

    /// Вне Linux memfd нет: пул откатится на обычное выделение FFmpeg.
    #[cfg(not(target_os = "linux"))]
    fn allocate_sealed_memfd(
        _block_len: usize,
    ) -> Result<HostFrameBlockBacking, HostFrameBlockAllocationError> {
        Err(HostFrameBlockAllocationError::SealedMemfdUnsupported)
    }

    /// Начало блока.
    pub(crate) fn address(&self) -> NonNull<u8> {
        match &self.backing {
            HostFrameBlockBacking::Anonymous { address, .. } => *address,
            #[cfg(target_os = "linux")]
            HostFrameBlockBacking::SealedMemfd(mapping) => mapping.address(),
        }
    }

    /// Длина блока в байтах.
    pub(crate) fn len(&self) -> usize {
        match &self.backing {
            HostFrameBlockBacking::Anonymous { layout, .. } => layout.size(),
            #[cfg(target_os = "linux")]
            HostFrameBlockBacking::SealedMemfd(mapping) => mapping.len(),
        }
    }

    /// Адрес начала как число — ключ поиска блока по `AVBufferRef.data`.
    fn address_key(&self) -> usize {
        self.address().as_ptr() as usize
    }
}

/// Почему блок не выделен; пул превращает любую причину в fallback на выделение FFmpeg.
#[derive(Debug, thiserror::Error)]
enum HostFrameBlockAllocationError {
    /// Длина нулевая, не кратна странице или не годится для layout-а.
    #[error("invalid host frame block length {block_len}")]
    InvalidLength {
        /// Запрошенная длина.
        block_len: usize,
    },

    /// Анонимная память не выделилась.
    #[error("out of memory allocating a {block_len} byte host frame block")]
    OutOfMemory {
        /// Запрошенная длина.
        block_len: usize,
    },

    /// Отказ при создании блока memfd.
    #[cfg(target_os = "linux")]
    #[error("sealed memfd host frame block unavailable: {0}")]
    SealedMemfd(#[from] sealed_memfd::SealedMemfdError),

    /// Платформа не умеет memfd.
    #[cfg(not(target_os = "linux"))]
    #[error("sealed memfd host frame blocks are supported only on Linux")]
    SealedMemfdUnsupported,
}

impl Drop for HostFrameBlock {
    fn drop(&mut self) {
        // Память memfd освобождает `Drop` отображения; анонимную освобождаем здесь.
        if let HostFrameBlockBacking::Anonymous { address, layout } = &self.backing {
            // SAFETY: память выделена `alloc_zeroed` с этим layout-ом ровно один раз и
            // освобождается ровно один раз: Drop вызывается, когда отпущена последняя `Arc`.
            unsafe { dealloc(address.as_ptr(), *layout) };
        }
    }
}

// SAFETY: адрес и длина не меняются; выделение — анонимная память, выровненная на 2 МиБ,
// либо `MAP_SHARED`-отображение запечатанного memfd (`SealedMemfdMapping`: печати
// SHRINK|GROW|SEAL без WRITE, отображение с offset 0 на всю длину, fd жив до Drop блока).
// Длина кратна странице и больше нуля (`allocate_zeroed`); память живёт до Drop, то есть
// пока жива любая `Arc` блока; id выдан `HostMemoryBlockId::allocate`.
unsafe impl HostImportableMemoryBlock for HostFrameBlock {
    fn id(&self) -> HostMemoryBlockId {
        self.id
    }

    fn region(&self) -> HostMemoryRegion {
        HostMemoryRegion::new(self.address(), self.len())
    }

    #[cfg(target_os = "linux")]
    fn sealed_memfd(&self) -> Option<SealedMemfdRegion<'_>> {
        match &self.backing {
            HostFrameBlockBacking::Anonymous { .. } => None,
            // Отображение начинается с начала файла.
            HostFrameBlockBacking::SealedMemfd(mapping) => {
                Some(SealedMemfdRegion::new(mapping.memfd(), 0))
            }
        }
    }
}

/// Счётчики пула для тестов.
#[cfg(test)]
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub(crate) struct HostFrameBlockPoolStats {
    /// Сколько блоков выделено заново.
    pub(crate) allocated: u64,
    /// Сколько раз выдан блок из свободных.
    pub(crate) reused: u64,
    /// Сколько выданных блоков сейчас у FFmpeg/кадров.
    pub(crate) checked_out: usize,
    /// Сколько свободных блоков ждёт переиспользования.
    pub(crate) free: usize,
    /// Сколько кадров ушло на обычное выделение FFmpeg (не наш блок).
    pub(crate) fallbacks: u64,
}

/// Какой по счёту откат на выделение FFmpeg записал пул.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[must_use = "первый откат пула надо записать в лог с причиной"]
pub(crate) enum HostFrameFallbackRecord {
    /// Первый откат этого пула: caller пишет причину в лог.
    FirstForPool,
    /// Откат уже был и попал в лог; этот только посчитан.
    Repeated,
}

/// Пул блоков одного декодера (одного `AVCodecContext`).
///
/// Потокобезопасен: FFmpeg вызывает `get_buffer2` из потоков декодирования, а
/// освобождает буфер тот поток, который отпустил последнюю ссылку на кадр (в том
/// числе главный поток после GPU-копии).
#[derive(Debug)]
pub(crate) struct HostFrameBlockPool {
    /// Вид памяти всех блоков пула; задаётся при создании и не меняется.
    memory_kind: HostFrameMemoryKind,
    state: Mutex<HostFrameBlockPoolState>,
}

#[derive(Debug, Default)]
struct HostFrameBlockPoolState {
    /// Длина блоков текущего размера кадра; свободные блоки другой длины выбрасываются.
    block_len: Option<usize>,
    /// Свободные блоки текущей длины.
    free: Vec<Arc<HostFrameBlock>>,
    /// Выданные блоки по адресу начала: так кадр находит свой блок.
    checked_out: HashMap<usize, Arc<HostFrameBlock>>,
    /// Декодер закрыт: возвращённые блоки больше не переиспользуются.
    closed: bool,
    /// Сколько блоков выделено заново.
    allocated: u64,
    /// Сколько блоков выдано повторно.
    reused: u64,
    /// Сколько кадров ушло на обычное выделение FFmpeg.
    fallbacks: u64,
    /// Отказ выделения уже попал в лог (пишем один раз на пул, не на каждый кадр).
    allocation_failure_logged: bool,
}

impl HostFrameBlockPool {
    /// Создаёт пустой пул, выделяющий блоки памяти вида `memory_kind`.
    pub(crate) fn new(memory_kind: HostFrameMemoryKind) -> Arc<Self> {
        Arc::new(Self {
            memory_kind,
            state: Mutex::new(HostFrameBlockPoolState::default()),
        })
    }

    /// Выдаёт блок длиной `block_len`: свободный той же длины или новый.
    ///
    /// Новая длина (кадр сменил размер) выбрасывает свободные блоки старой длины.
    /// `None` — пул закрыт или память не выделилась; caller идёт обычным путём FFmpeg.
    pub(crate) fn checkout(&self, block_len: usize) -> Option<Arc<HostFrameBlock>> {
        let mut state = self.lock_state();
        if state.closed {
            return None;
        }
        if state.block_len != Some(block_len) {
            state.free.clear();
            state.block_len = Some(block_len);
        }
        let block = match state.free.pop() {
            Some(block) => {
                state.reused += 1;
                block
            }
            None => match HostFrameBlock::allocate_zeroed(self.memory_kind, block_len) {
                Ok(block) => {
                    state.allocated += 1;
                    Arc::new(block)
                }
                Err(error) => {
                    // Отказ системы не фатален: caller пойдёт обычным выделением FFmpeg
                    // (и посчитает fallback). В лог пишем один раз на пул.
                    if !state.allocation_failure_logged {
                        state.allocation_failure_logged = true;
                        log::warn!(
                            "host frame block allocation failed, falling back to FFmpeg buffers: {error}"
                        );
                    }
                    return None;
                }
            },
        };
        state
            .checked_out
            .insert(block.address_key(), Arc::clone(&block));
        Some(block)
    }

    /// Принимает блок обратно, когда FFmpeg отпустил последнюю ссылку на его буфер.
    ///
    /// Блок становится свободным, если пул открыт, длина текущая и свободных меньше
    /// потолка; иначе пул просто забывает его (память уйдёт, когда отпустят остальные
    /// ссылки, например импорт renderer-а).
    pub(crate) fn give_back(&self, block_address: NonNull<u8>) {
        let mut state = self.lock_state();
        let Some(block) = state.checked_out.remove(&(block_address.as_ptr() as usize)) else {
            return;
        };
        let keeps_free_block = !state.closed
            && state.block_len == Some(block.len())
            && state.free.len() < MAX_FREE_HOST_FRAME_BLOCKS;
        if keeps_free_block {
            state.free.push(block);
        }
    }

    /// Блок, выданный под буфер с таким адресом начала, если он ещё у FFmpeg/кадров.
    pub(crate) fn checked_out_block(&self, buffer_address: usize) -> Option<Arc<HostFrameBlock>> {
        self.lock_state().checked_out.get(&buffer_address).cloned()
    }

    /// Отмечает кадр, который `get_buffer2` отдал обычному выделению FFmpeg.
    ///
    /// Это не ошибка: такой кадр renderer загрузит копией, как раньше. Счётчик
    /// показывает в тестах, что путь без CPU-копии не сработал, а ответ говорит caller-у,
    /// первый ли это откат пула: причину отката знает caller, и в лог он пишет её
    /// один раз на пул (на каждый кадр лог бы утонул).
    pub(crate) fn record_fallback(&self) -> HostFrameFallbackRecord {
        let mut state = self.lock_state();
        state.fallbacks += 1;
        if state.fallbacks == 1 {
            HostFrameFallbackRecord::FirstForPool
        } else {
            HostFrameFallbackRecord::Repeated
        }
    }

    /// Закрывает пул вместе с декодером: свободные блоки освобождаются, возвращаемые
    /// позже блоки больше не переиспользуются. Выданные кадры остаются валидными.
    pub(crate) fn close(&self) {
        let mut state = self.lock_state();
        state.closed = true;
        state.free.clear();
    }

    /// Счётчики пула.
    #[cfg(test)]
    pub(crate) fn stats(&self) -> HostFrameBlockPoolStats {
        let state = self.lock_state();
        HostFrameBlockPoolStats {
            allocated: state.allocated,
            reused: state.reused,
            checked_out: state.checked_out.len(),
            free: state.free.len(),
            fallbacks: state.fallbacks,
        }
    }

    /// Mutex защищает только списки блоков; паника другого потока не оставляет их в
    /// противоречивом состоянии (каждая операция — одна вставка/удаление), поэтому
    /// отравление не мешает работе.
    fn lock_state(&self) -> MutexGuard<'_, HostFrameBlockPoolState> {
        self.state.lock().unwrap_or_else(PoisonError::into_inner)
    }
}

#[cfg(test)]
mod tests;
