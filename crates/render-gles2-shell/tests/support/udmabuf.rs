//! Буфер ядра для тестов: `/dev/udmabuf` оборачивает обычную память `memfd` в dma-buf fd
//! (всегда linear). Без `/dev/udmabuf` тест пропускается с причиной в stderr.

#![allow(
    unsafe_code,
    clippy::unwrap_used,
    clippy::expect_used,
    reason = "тестовый стенд: сырые вызовы libc с SAFETY, громкие падения"
)]

use std::io::{Seek, SeekFrom, Write};
use std::os::fd::{AsRawFd, FromRawFd, OwnedFd};

const UDMABUF_DEVICE_PATH: &str = "/dev/udmabuf";
/// `UDMABUF_CREATE = _IOW('u', 0x42, struct udmabuf_create)`, 24 байта (`linux/udmabuf.h`).
const UDMABUF_CREATE_IOCTL: libc::c_ulong = 0x4018_7542;
const UDMABUF_FLAGS_CLOEXEC: u32 = 0x01;

/// Раскладка `struct udmabuf_create`.
#[repr(C)]
struct UdmabufCreateRequest {
    memfd: u32,
    flags: u32,
    offset: u64,
    size: u64,
}

/// Буфер из памяти memfd + его dma-buf fd. memfd держим открытым ради перезаписи.
pub struct Udmabuf {
    pub dma_buf: OwnedFd,
    memfd: std::fs::File,
}

impl Udmabuf {
    /// Новый буфер с содержимым `contents` (размер округляется до страницы).
    pub fn create(contents: &[u8]) -> Result<Self, String> {
        let device = std::fs::OpenOptions::new()
            .read(true)
            .write(true)
            .open(UDMABUF_DEVICE_PATH)
            .map_err(|error| format!("{UDMABUF_DEVICE_PATH} is not accessible: {error}"))?;
        // SAFETY: `sysconf` только читает системную константу.
        let page_size = u64::try_from(unsafe { libc::sysconf(libc::_SC_PAGESIZE) }).unwrap();
        let buffer_size = (contents.len() as u64).div_ceil(page_size) * page_size;
        // SAFETY: имя — статическая C-строка; результат проверяется и сразу идёт во владение.
        let raw_memfd =
            unsafe { libc::memfd_create(c"gles2-video-test".as_ptr(), libc::MFD_ALLOW_SEALING) };
        assert!(
            raw_memfd >= 0,
            "memfd_create: {}",
            std::io::Error::last_os_error()
        );
        // SAFETY: fd только что создан и больше ничьим владением не является.
        let mut memfd = std::fs::File::from(unsafe { OwnedFd::from_raw_fd(raw_memfd) });
        memfd.set_len(buffer_size).unwrap();
        memfd.write_all(contents).unwrap();
        // udmabuf принимает только memfd, который нельзя уменьшить.
        // SAFETY: fd валиден на время вызова; fcntl владение не забирает.
        let sealed =
            unsafe { libc::fcntl(memfd.as_raw_fd(), libc::F_ADD_SEALS, libc::F_SEAL_SHRINK) };
        assert_eq!(
            sealed,
            0,
            "F_SEAL_SHRINK: {}",
            std::io::Error::last_os_error()
        );
        let request = UdmabufCreateRequest {
            memfd: u32::try_from(memfd.as_raw_fd()).unwrap(),
            flags: UDMABUF_FLAGS_CLOEXEC,
            offset: 0,
            size: buffer_size,
        };
        // SAFETY: `request` совпадает с `struct udmabuf_create` и живёт весь вызов; fd валидны.
        let raw_dma_buf =
            unsafe { libc::ioctl(device.as_raw_fd(), UDMABUF_CREATE_IOCTL, &request) };
        if raw_dma_buf < 0 {
            return Err(format!(
                "UDMABUF_CREATE: {}",
                std::io::Error::last_os_error()
            ));
        }
        // SAFETY: ioctl вернул новый fd, которым больше никто не владеет.
        let dma_buf = unsafe { OwnedFd::from_raw_fd(raw_dma_buf) };
        Ok(Self { dma_buf, memfd })
    }

    /// Перезаписать содержимое с начала ТАК, КАК ОБЯЗАН ПИСАТЬ CPU-ПРОИЗВОДИТЕЛЬ в dma-buf.
    ///
    /// Простая запись в memfd оставляет новые байты в кэше CPU. На GPU с кэшем, общим с CPU
    /// (AMD APU, Intel с LLC), этого достаточно, а на Intel Ironlake (T410, Mesa crocus, i915)
    /// общего LLC нет: GPU читает чужой импортированный объект мимо кэша CPU и видит СТАРЫЕ
    /// байты. Первое рисование верно (ядро сбрасывает кэш при первой привязке страниц), а все
    /// последующие записи ядро не отслеживает. Доказано экспериментом на T410: запись через
    /// `DMA_BUF_IOCTL_SYNC` одна НЕ помогает (на x86 `udmabuf` не сбрасывает кэш: для
    /// когерентного по DMA x86 `dma_sync_*` — пустышка), помогает только явный `clflush`
    /// по записанному диапазону. Поэтому производитель: открывает окно CPU-доступа
    /// (`DMA_BUF_IOCTL_SYNC`, Documentation/driver-api/dma-buf.rst, «CPU Access to DMA Buffer
    /// Objects» — официальный протокол ядра, на других архитектурах он и делает сброс кэша),
    /// пишет, сбрасывает кэш CPU по диапазону (x86_64) и закрывает окно.
    pub fn overwrite_contents_as_cpu_producer(&mut self, contents: &[u8]) {
        let length = self.memfd.metadata().unwrap().len();
        sync_cpu_access(&self.dma_buf, CpuAccessWindow::Begin);
        self.memfd.seek(SeekFrom::Start(0)).unwrap();
        self.memfd.write_all(contents).unwrap();
        self.memfd.sync_all().unwrap();
        flush_cpu_cache(&self.memfd, length);
        sync_cpu_access(&self.dma_buf, CpuAccessWindow::End);
    }

    /// Объект ядра по `fstat` его dma-buf fd (как это делает провайдер ресурсов декодера).
    pub fn kernel_object(&self) -> KernelObject {
        // SAFETY: нулевая `stat` — допустимое начальное значение для заполнения ядром.
        let mut status: libc::stat = unsafe { std::mem::zeroed() };
        // SAFETY: fd валиден, `status` — корректный выходной буфер.
        let result = unsafe { libc::fstat(self.dma_buf.as_raw_fd(), &mut status) };
        assert_eq!(result, 0, "fstat: {}", std::io::Error::last_os_error());
        KernelObject {
            device: status.st_dev,
            inode: status.st_ino,
            size: u32::try_from(status.st_size).unwrap(),
        }
    }
}

/// Паспорт объекта ядра из `fstat`.
#[derive(Debug, Clone, Copy)]
pub struct KernelObject {
    pub device: u64,
    pub inode: u64,
    pub size: u32,
}

/// Начало или конец окна CPU-доступа к dma-buf.
#[derive(Clone, Copy)]
enum CpuAccessWindow {
    Begin,
    End,
}

/// `DMA_BUF_IOCTL_SYNC = _IOW('b', 0, struct dma_buf_sync)`, 8 байт (`linux/dma-buf.h`).
const DMA_BUF_IOCTL_SYNC: libc::c_ulong = 0x4008_6200;
/// `DMA_BUF_SYNC_WRITE`: CPU собирается писать.
const DMA_BUF_SYNC_WRITE: u64 = 2;
/// `DMA_BUF_SYNC_END`: конец окна (начало — нулевой флаг `DMA_BUF_SYNC_START`).
const DMA_BUF_SYNC_END: u64 = 4;
/// Размер строки кэша x86_64: `clflush` сбрасывает ровно одну такую строку.
#[cfg(target_arch = "x86_64")]
const CACHE_LINE_BYTES: usize = 64;

/// `DMA_BUF_IOCTL_SYNC` с флагом записи; ошибка ядра — громкое падение теста.
fn sync_cpu_access(dma_buf: &OwnedFd, window: CpuAccessWindow) {
    let flags = match window {
        CpuAccessWindow::Begin => DMA_BUF_SYNC_WRITE,
        CpuAccessWindow::End => DMA_BUF_SYNC_END | DMA_BUF_SYNC_WRITE,
    };
    // SAFETY: `flags` — `struct dma_buf_sync` из одного u64, живёт весь вызов; fd валиден.
    let result = unsafe { libc::ioctl(dma_buf.as_raw_fd(), DMA_BUF_IOCTL_SYNC, &flags) };
    assert_eq!(
        result,
        0,
        "DMA_BUF_IOCTL_SYNC: {}",
        std::io::Error::last_os_error()
    );
}

/// Сбросить из кэша CPU в память первые `length` байт memfd (`clflush` по каждой строке кэша).
/// Страницы memfd отображаются отдельным mmap: те же физические страницы, что видит GPU.
#[cfg(target_arch = "x86_64")]
fn flush_cpu_cache(memfd: &std::fs::File, length: u64) {
    let length = usize::try_from(length).unwrap();
    // SAFETY: отображение shared RW на весь memfd; результат проверяется, `munmap` ниже.
    let mapping = unsafe {
        libc::mmap(
            std::ptr::null_mut(),
            length,
            libc::PROT_READ | libc::PROT_WRITE,
            libc::MAP_SHARED,
            memfd.as_raw_fd(),
            0,
        )
    };
    assert_ne!(
        mapping,
        libc::MAP_FAILED,
        "mmap: {}",
        std::io::Error::last_os_error()
    );
    let base = mapping.cast::<u8>();
    // SAFETY: все адреса `base + offset` лежат внутри только что созданного отображения;
    // барьеры упорядочивают запись CPU до сброса и сброс до последующего чтения GPU.
    unsafe {
        core::arch::x86_64::_mm_mfence();
        for offset in (0..length).step_by(CACHE_LINE_BYTES) {
            core::arch::x86_64::_mm_clflush(base.add(offset));
        }
        core::arch::x86_64::_mm_mfence();
        libc::munmap(mapping, length);
    }
}

/// Не x86_64: сброс кэша делает `DMA_BUF_IOCTL_SYNC` (там `dma_sync_*` реальный).
#[cfg(not(target_arch = "x86_64"))]
fn flush_cpu_cache(_memfd: &std::fs::File, _length: u64) {}
