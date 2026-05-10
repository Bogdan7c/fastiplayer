use std::fs::{File, OpenOptions};
use std::os::fd::{AsRawFd, FromRawFd};

use nix::ioctl_readwrite;
use nix::libc;

/// ioctl magic для dma-buf heap (из linux/dma-heap.h).
const DMA_HEAP_IOC_MAGIC: u8 = b'H';

/// Структура данных для ioctl DMA_HEAP_IOCTL_ALLOC.
///
/// Соответствует C-структуре из `linux/dma-heap.h`.
/// Ядро Linux заполняет поле `fd` файловым дескриптором выделенного DMA-BUF.
#[repr(C)]
#[derive(Debug)]
struct DmaHeapAllocationData {
    /// Запрашиваемый размер буфера в байтах.
    len: u64,
    /// Возвращаемый файловый дескриптор (ядро записывает сюда).
    fd: u32,
    /// Флаги для создаваемого fd (O_RDWR | O_CLOEXEC).
    fd_flags: u32,
    /// Флаги кучи (0 для стандартного поведения).
    heap_flags: u64,
}

// Генерируем ioctl функцию для выделения DMA-BUF.
// `ioctl_readwrite!` создаёт безопасную обёртку вокруг ioctl системного вызова.
ioctl_readwrite!(
    dma_heap_ioctl_alloc,
    DMA_HEAP_IOC_MAGIC,
    0,
    DmaHeapAllocationData
);

/// Путь к системной DMA-куче.
///
/// `/dev/dma_heap/system` доступен на Linux 5.6+ и выделяет буферы
/// из системной памяти с поддержкой DMA.
const DMA_HEAP_PATH: &str = "/dev/dma_heap/system";

/// Выделяет DMA-BUF заданного размера через системную dma-кучу.
///
/// # Аргументы
/// * `size` — размер буфера в байтах.
///
/// # Возвращаемое значение
/// `File`, владеющий файловым дескриптором DMA-BUF. Буфер zero-initialized
/// ядром и подходит для CPU mmap + GPU import.
///
/// # Ошибки
/// Возвращает ошибку если:
/// - `/dev/dma_heap/system` недоступен (требуется Linux 5.6+)
/// - ioctl выделения не удался
/// - ядро вернуло невалидный fd
///
/// # Безопасность
/// Эта функция использует `unsafe` для:
/// 1. Вызова ioctl — `allocation_request` корректно инициализирован, `heap` валидный fd.
/// 2. `File::from_raw_fd` — ядро гарантирует что возвращённый fd валиден.
pub fn allocate_dma_buffer(size: usize) -> anyhow::Result<File> {
    // Открываем устройство dma-heap на чтение и запись.
    let heap = OpenOptions::new()
        .read(true)
        .write(true)
        .open(DMA_HEAP_PATH)
        .map_err(|e| anyhow::anyhow!("Failed to open {}: {}", DMA_HEAP_PATH, e))?;

    // Формируем запрос на выделение.
    let mut allocation_request = DmaHeapAllocationData {
        len: size as u64,
        fd: 0,
        fd_flags: (libc::O_RDWR | libc::O_CLOEXEC) as u32,
        heap_flags: 0,
    };

    // SAFETY: `allocation_request` — корректно размещённая структура, `heap` — валидный fd.
    unsafe {
        dma_heap_ioctl_alloc(heap.as_raw_fd(), &mut allocation_request)
            .map_err(|e| anyhow::anyhow!("DMA_HEAP_IOCTL_ALLOC failed: {}", e))?;
    }

    // Проверяем что ядро вернуло валидный fd.
    if allocation_request.fd == 0 {
        return Err(anyhow::anyhow!(
            "DMA_HEAP_IOCTL_ALLOC returned invalid fd (0)"
        ));
    }

    // SAFETY: ядро вернуло валидный fd, мы берём на него владение.
    let file = unsafe { File::from_raw_fd(allocation_request.fd as i32) };
    Ok(file)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::Path;

    /// Проверяем доступность `/dev/dma_heap/system`.
    ///
    /// В sandbox-средах (CI, Docker) устройство может быть недоступно
    /// из-за отсутствия прав или неподдерживаемого ядра.
    fn dma_heap_available() -> bool {
        Path::new(DMA_HEAP_PATH).exists()
            && std::fs::OpenOptions::new()
                .read(true)
                .write(true)
                .open(DMA_HEAP_PATH)
                .is_ok()
    }

    /// Тест: выделение DMA-буфера размером 4KB.
    ///
    /// Требует `/dev/dma_heap/system` (Linux 5.6+) и права на доступ.
    /// Пропускается если устройство недоступно (sandbox/CI).
    #[test]
    fn test_allocate_dma_buffer() {
        if !dma_heap_available() {
            eprintln!("Skipping test: {} not available", DMA_HEAP_PATH);
            return;
        }
        let file = allocate_dma_buffer(4096).expect("dma-heap alloc failed");
        let metadata = file.metadata().expect("metadata failed");
        assert!(
            metadata.len() >= 4096,
            "allocated buffer too small: {} < 4096",
            metadata.len()
        );
    }

    /// Тест: выделение большого DMA-буфера (1MB).
    ///
    /// Пропускается если dma-heap недоступен.
    #[test]
    fn test_allocate_dma_buffer_1mb() {
        if !dma_heap_available() {
            eprintln!("Skipping test: {} not available", DMA_HEAP_PATH);
            return;
        }
        let file = allocate_dma_buffer(1024 * 1024).expect("dma-heap alloc failed");
        let metadata = file.metadata().expect("metadata failed");
        assert!(metadata.len() >= 1024 * 1024);
    }

    /// Тест: проверка что fd можно использовать для mmap (через metadata).
    ///
    /// Пропускается если dma-heap недоступен.
    #[test]
    fn test_dma_buffer_fd_valid() {
        if !dma_heap_available() {
            eprintln!("Skipping test: {} not available", DMA_HEAP_PATH);
            return;
        }
        let file = allocate_dma_buffer(4096).expect("dma-heap alloc failed");
        // Если metadata доступна, значит fd валиден.
        let _ = file.metadata().expect("fd should be valid");
    }
}
