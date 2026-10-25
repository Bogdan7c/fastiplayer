//! Буфер ядра для паритета HW-видео: `/dev/udmabuf` оборачивает обычную память `memfd` в
//! настоящий dma-buf fd (всегда linear). Тот же рецепт, что у `gles2-video` и
//! `render-wgpu-video`, но самостоятельный: тестовые стенды этих crate-ов снаружи недоступны.
//!
//! Это ЕДИНСТВЕННОЕ место crate-а `render-wgpu-shell` с `unsafe` и только в тестовой сборке:
//! создание memfd и ioctl udmabuf не имеют безопасных обёрток в std. Каждый блок — с `SAFETY`.

#![allow(
    unsafe_code,
    clippy::unwrap_used,
    clippy::expect_used,
    reason = "тестовый стенд: сырые вызовы libc с SAFETY, громкие падения"
)]

use std::io::Write;
use std::os::fd::{AsRawFd, FromRawFd, OwnedFd};
use std::os::unix::fs::MetadataExt;

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

/// Паспорт объекта ядра (то, что адаптер плеера снимает `fstat`-ом с dma-buf fd).
#[derive(Debug, Clone, Copy)]
pub(super) struct KernelObjectPassport {
    pub(super) device: u64,
    pub(super) inode: u64,
    pub(super) size: u64,
}

/// dma-buf из памяти memfd.
pub(super) struct Udmabuf {
    pub(super) dma_buf: OwnedFd,
}

impl Udmabuf {
    /// Новый буфер с содержимым `contents` (размер округляется до страницы). `Err` — причина
    /// недоступности `/dev/udmabuf` для строки пропуска теста.
    pub(super) fn create(contents: &[u8]) -> Result<Self, String> {
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
            unsafe { libc::memfd_create(c"parity-nv12".as_ptr(), libc::MFD_ALLOW_SEALING) };
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
        // SAFETY: ioctl вернул новый fd, которым больше никто не владеет. memfd можно закрыть:
        // dma-buf держит ссылку на его страницы.
        let dma_buf = unsafe { OwnedFd::from_raw_fd(raw_dma_buf) };
        Ok(Self { dma_buf })
    }

    /// Паспорт объекта по `fstat` его dma-buf fd (безопасно: через `File::metadata`).
    pub(super) fn passport(&self) -> KernelObjectPassport {
        let metadata = std::fs::File::from(self.dma_buf.try_clone().unwrap())
            .metadata()
            .unwrap();
        KernelObjectPassport {
            device: metadata.dev(),
            inode: metadata.ino(),
            size: metadata.size(),
        }
    }
}
