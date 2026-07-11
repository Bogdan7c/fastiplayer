use std::fs::{File, OpenOptions};
use std::os::fd::{AsRawFd, FromRawFd, RawFd};

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

    // SAFETY: macro ожидает writable pointer на структуру kernel UAPI точного
    // `#[repr(C)]` layout-а; `allocation_request` живёт весь ioctl. `heap`
    // содержит открытый dma-heap fd, а ioctl не сохраняет переданный pointer.
    unsafe {
        dma_heap_ioctl_alloc(heap.as_raw_fd(), &mut allocation_request)
            .map_err(|e| anyhow::anyhow!("DMA_HEAP_IOCTL_ALLOC failed: {}", e))?;
    }

    // Нулевой fd валиден, если до ioctl был закрыт stdin. Проверяем только
    // представимость kernel `u32` в пользовательском `RawFd` (`c_int`).
    let allocated_raw_fd = dma_heap_fd_to_raw_fd(allocation_request.fd)?;

    // SAFETY: успешный DMA_HEAP_IOCTL_ALLOC возвращает новый owned fd в поле
    // `fd`. До этой строки его не оборачивают и не закрывают; после неё
    // единственным владельцем становится `File`.
    let file = unsafe { File::from_raw_fd(allocated_raw_fd) };
    Ok(file)
}

/// Проверяет, что беззнаковое поле kernel UAPI представимо как Linux `RawFd`.
fn dma_heap_fd_to_raw_fd(kernel_fd: u32) -> anyhow::Result<RawFd> {
    RawFd::try_from(kernel_fd).map_err(|_| {
        anyhow::anyhow!("DMA_HEAP_IOCTL_ALLOC returned fd outside RawFd range: {kernel_fd}")
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use static_assertions::assert_impl_all;
    use std::path::Path;

    // `File` является единственным RAII-owner DMA-BUF fd. Его можно передать
    // между потоками без дублирования ownership; закрытие остаётся exactly-once.
    assert_impl_all!(File: Send, Sync);

    #[test]
    fn dma_heap_fd_zero_is_a_valid_raw_fd_value() {
        assert_eq!(dma_heap_fd_to_raw_fd(0).unwrap(), 0);
    }

    #[test]
    fn dma_heap_fd_rejects_values_outside_signed_raw_fd_range() {
        let invalid_kernel_fd = i32::MAX as u32 + 1;
        let error = dma_heap_fd_to_raw_fd(invalid_kernel_fd).unwrap_err();

        assert!(error.to_string().contains("outside RawFd range"));
    }

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
