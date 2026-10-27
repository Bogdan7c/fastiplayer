//! Модуль безопасности пути «кадр программного декодера без CPU-копии»: вызовы ядра Linux
//! (`/dev/udmabuf`, `fstat`, `memfd_create`, `mmap`) и сброс кэша CPU. Каждый блок `unsafe` — с
//! `// SAFETY:`; наружу выходят только безопасные типы.
//!
//! Источники истины:
//! - udmabuf — `include/uapi/linux/udmabuf.h` и `drivers/dma-buf/udmabuf.c`: `UDMABUF_CREATE`
//!   превращает диапазон memfd в dma-buf; memfd обязан быть запечатан `F_SEAL_SHRINK` и НЕ
//!   запечатан `F_SEAL_WRITE`, смещение и длина кратны странице;
//! - сброс кэша — `CLFLUSH` (Intel SDM, том 2A): строка кэша, содержащая адрес, записывается в
//!   память и выбрасывается из всех уровней кэша. Нужен там, где GPU читает импортированную
//!   память мимо кэша CPU (Intel без общего LLC, например Ironlake), см. самопроверку в
//!   [`super::availability`].

use std::fs::{File, OpenOptions};
use std::io;
use std::os::fd::{AsFd, AsRawFd, BorrowedFd, FromRawFd, OwnedFd};
use std::os::unix::fs::OpenOptionsExt;
use std::ptr::NonNull;

/// Узел устройства udmabuf.
const UDMABUF_DEVICE_PATH: &str = "/dev/udmabuf";
/// `UDMABUF_FLAGS_CLOEXEC`: новый dma-buf fd закрывается при `exec` (как наши остальные fd).
const UDMABUF_FLAGS_CLOEXEC: u32 = 0x01;

/// `struct udmabuf_create` из `linux/udmabuf.h`.
#[repr(C)]
struct UdmabufCreateRequest {
    memfd: u32,
    flags: u32,
    offset: u64,
    size: u64,
}

/// `UDMABUF_CREATE = _IOW('u', 0x42, struct udmabuf_create)`.
const UDMABUF_CREATE: libc::Ioctl = libc::_IOW::<UdmabufCreateRequest>(b'u' as u32, 0x42);

/// Открытое устройство `/dev/udmabuf`.
#[derive(Debug)]
pub(super) struct UdmabufDevice {
    device: File,
}

impl UdmabufDevice {
    /// Открывает устройство. Нет модуля ядра или прав — ошибка ввода-вывода (путь «без копии»
    /// просто недоступен).
    pub(super) fn open() -> io::Result<Self> {
        let device = OpenOptions::new()
            .read(true)
            .write(true)
            .custom_flags(libc::O_CLOEXEC)
            .open(UDMABUF_DEVICE_PATH)?;
        Ok(Self { device })
    }

    /// Новый dma-buf над диапазоном `[offset, offset + size)` запечатанного memfd.
    ///
    /// dma-buf держит страницы memfd сам: memfd и его отображение можно освобождать независимо.
    pub(super) fn wrap_sealed_memfd(
        &self,
        memfd: BorrowedFd<'_>,
        offset: u64,
        size: u64,
    ) -> io::Result<OwnedFd> {
        let memfd = u32::try_from(memfd.as_raw_fd())
            .map_err(|_| io::Error::new(io::ErrorKind::InvalidInput, "negative memfd"))?;
        let request = UdmabufCreateRequest {
            memfd,
            flags: UDMABUF_FLAGS_CLOEXEC,
            offset,
            size,
        };
        // SAFETY: `request` по раскладке совпадает с `struct udmabuf_create` и живёт весь вызов;
        // fd устройства открыт (`self.device`), memfd заимствован и жив весь вызов.
        let raw = unsafe { libc::ioctl(self.device.as_raw_fd(), UDMABUF_CREATE, &request) };
        if raw < 0 {
            return Err(io::Error::last_os_error());
        }
        // SAFETY: ioctl вернул новый fd, которым больше никто не владеет.
        Ok(unsafe { OwnedFd::from_raw_fd(raw) })
    }
}

/// Паспорт объекта ядра по `fstat` его fd (устройство, inode, размер).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) struct KernelObjectIdentity {
    pub(super) device: u64,
    pub(super) inode: u64,
    pub(super) size: u64,
}

/// `fstat` fd.
pub(super) fn kernel_object_identity(fd: BorrowedFd<'_>) -> io::Result<KernelObjectIdentity> {
    // SAFETY: нулевая `stat` — допустимое начальное значение для заполнения ядром.
    let mut status: libc::stat = unsafe { std::mem::zeroed() };
    // SAFETY: fd заимствован и жив весь вызов; `status` — корректный выходной буфер.
    let result = unsafe { libc::fstat(fd.as_raw_fd(), &mut status) };
    if result != 0 {
        return Err(io::Error::last_os_error());
    }
    let size = u64::try_from(status.st_size)
        .map_err(|_| io::Error::new(io::ErrorKind::InvalidData, "negative object size"))?;
    Ok(KernelObjectIdentity {
        device: status.st_dev,
        inode: status.st_ino,
        size,
    })
}

/// Запечатанный memfd с отображением для записи CPU — буфер самопроверки «видит ли GPU запись
/// CPU» ([`super::probe_buffer`]). Кадров декодера этот тип не касается.
#[derive(Debug)]
pub(super) struct SealedMemfdPages {
    memfd: OwnedFd,
    mapping: NonNull<u8>,
    len: usize,
}

impl SealedMemfdPages {
    /// `len` байт (кратно странице), заполнены нулями, запечатаны от уменьшения и роста.
    pub(super) fn create(len: usize) -> io::Result<Self> {
        let page_size = page_size()?;
        if len == 0 || !len.is_multiple_of(page_size) {
            return Err(io::Error::new(
                io::ErrorKind::InvalidInput,
                "memfd length must be a positive multiple of the page size",
            ));
        }
        // SAFETY: имя — статическая C-строка; результат проверяется и сразу идёт во владение.
        let raw = unsafe {
            libc::memfd_create(
                c"fastiplayer-gles2-probe".as_ptr(),
                libc::MFD_CLOEXEC | libc::MFD_ALLOW_SEALING,
            )
        };
        if raw < 0 {
            return Err(io::Error::last_os_error());
        }
        // SAFETY: fd только что создан и больше ничьим владением не является.
        let memfd = unsafe { OwnedFd::from_raw_fd(raw) };
        let file_len = libc::off_t::try_from(len)
            .map_err(|_| io::Error::new(io::ErrorKind::InvalidInput, "memfd too large"))?;
        // SAFETY: fd открыт; длина неотрицательна и помещается в `off_t`.
        if unsafe { libc::ftruncate(memfd.as_raw_fd(), file_len) } != 0 {
            return Err(io::Error::last_os_error());
        }
        let seals = libc::F_SEAL_SHRINK | libc::F_SEAL_GROW | libc::F_SEAL_SEAL;
        // SAFETY: fd открыт; `fcntl` владение не забирает.
        if unsafe { libc::fcntl(memfd.as_raw_fd(), libc::F_ADD_SEALS, seals) } != 0 {
            return Err(io::Error::last_os_error());
        }
        // SAFETY: отображение shared RW всего memfd; результат проверяется, `munmap` — в Drop.
        let mapping = unsafe {
            libc::mmap(
                std::ptr::null_mut(),
                len,
                libc::PROT_READ | libc::PROT_WRITE,
                libc::MAP_SHARED,
                memfd.as_raw_fd(),
                0,
            )
        };
        if mapping == libc::MAP_FAILED {
            return Err(io::Error::last_os_error());
        }
        let mapping = NonNull::new(mapping.cast::<u8>())
            .ok_or_else(|| io::Error::other("mmap returned a null mapping"))?;
        Ok(Self {
            memfd,
            mapping,
            len,
        })
    }

    /// fd memfd (для `UDMABUF_CREATE`).
    pub(super) fn memfd(&self) -> BorrowedFd<'_> {
        self.memfd.as_fd()
    }

    /// Длина в байтах.
    pub(super) const fn len(&self) -> usize {
        self.len
    }

    /// Заполнить все байты значением (обычная запись CPU, через кэш).
    pub(super) fn fill(&mut self, value: u8) {
        self.bytes_mut().fill(value);
    }

    /// Байты отображения.
    pub(super) fn bytes(&self) -> &[u8] {
        // SAFETY: отображение длиной `len` живо, пока жив `self`; запись идёт только через
        // `&mut self`, поэтому общий срез не пересекается с изменяемым.
        unsafe { std::slice::from_raw_parts(self.mapping.as_ptr(), self.len) }
    }

    pub(super) fn bytes_mut(&mut self) -> &mut [u8] {
        // SAFETY: см. `bytes`; `&mut self` — единственный писатель на стороне CPU.
        unsafe { std::slice::from_raw_parts_mut(self.mapping.as_ptr(), self.len) }
    }
}

impl Drop for SealedMemfdPages {
    fn drop(&mut self) {
        // SAFETY: отображение создано в `create` этой длины и больше не используется.
        let result = unsafe { libc::munmap(self.mapping.as_ptr().cast(), self.len) };
        if result != 0 {
            log::warn!(
                "GLES2: munmap of the probe buffer failed: {}",
                io::Error::last_os_error()
            );
        }
    }
}

fn page_size() -> io::Result<usize> {
    // SAFETY: `sysconf` только читает системную константу.
    let page_size = unsafe { libc::sysconf(libc::_SC_PAGESIZE) };
    usize::try_from(page_size)
        .ok()
        .filter(|size| *size > 0)
        .ok_or_else(|| io::Error::other("page size is unavailable"))
}

/// Умеет ли эта сборка сбрасывать кэш CPU по диапазону.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) enum CpuCacheFlushSupport {
    /// x86_64: `CLFLUSH` (входит в базовый SSE2, есть на любом x86_64).
    Clflush,
    /// Другая архитектура: путь, которому нужен сброс, не включается.
    Unavailable,
}

/// Поддержка сброса кэша CPU в этой сборке.
pub(super) const CPU_CACHE_FLUSH_SUPPORT: CpuCacheFlushSupport = if cfg!(target_arch = "x86_64") {
    CpuCacheFlushSupport::Clflush
} else {
    CpuCacheFlushSupport::Unavailable
};

/// Размер строки кэша x86_64: `CLFLUSH` сбрасывает ровно одну строку, содержащую адрес.
const CACHE_LINE_BYTES: usize = 64;

/// Сбросить из кэша CPU в память все строки кэша, которые пересекают срезы `ranges`.
///
/// Барьер до — запись CPU упорядочена до сброса; барьер после — сброс завершён до того, как
/// вызывающий отдаст GPU команды, читающие эту память. На архитектуре без
/// [`CpuCacheFlushSupport::Clflush`] ничего не делает (туда путь не включается, см.
/// [`CPU_CACHE_FLUSH_SUPPORT`]).
pub(super) fn flush_cpu_cache_ranges<'a>(ranges: impl IntoIterator<Item = &'a [u8]>) {
    #[cfg(target_arch = "x86_64")]
    {
        // SAFETY: `MFENCE` без операндов не трогает память программы.
        unsafe { core::arch::x86_64::_mm_mfence() };
        for range in ranges {
            let start = range.as_ptr() as usize;
            let end = start + range.len();
            let mut line = start / CACHE_LINE_BYTES * CACHE_LINE_BYTES;
            while line < end {
                // SAFETY: `CLFLUSH` не меняет содержимое памяти; адрес строки лежит в той же
                // строке кэша, что и байт среза `range` (живой, читаемый), поэтому обращение
                // допустимо для страниц этого среза.
                unsafe { core::arch::x86_64::_mm_clflush(line as *const u8) };
                line += CACHE_LINE_BYTES;
            }
        }
        // SAFETY: см. первый барьер.
        unsafe { core::arch::x86_64::_mm_mfence() };
    }
    #[cfg(not(target_arch = "x86_64"))]
    {
        let _ = ranges;
    }
}

#[cfg(test)]
#[path = "kernel_tests.rs"]
mod tests;
