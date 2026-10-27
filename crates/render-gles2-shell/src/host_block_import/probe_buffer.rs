//! Буфер самопроверки «видит ли GPU запись CPU в импортированную память»
//! ([`gles2_video::probe_cpu_write_visibility`]): запечатанный memfd → `/dev/udmabuf` → dma-buf.
//! Тот же механизм, что у блоков кадров декодера, только свой маленький буфер.

use std::io;
use std::os::fd::{AsFd, BorrowedFd, OwnedFd};

use gles2_video::{CPU_WRITE_PROBE_MIN_BUFFER_BYTES, CpuWriteProbeProducer};

use super::kernel::{SealedMemfdPages, UdmabufDevice, flush_cpu_cache_ranges};

/// Длина буфера самопроверки: минимум `gles2-video`, округлённый до страницы.
const PROBE_BUFFER_BYTES: usize = CPU_WRITE_PROBE_MIN_BUFFER_BYTES.next_multiple_of(4096);

/// Производитель самопроверки: CPU пишет через отображение memfd, GPU читает dma-buf.
#[derive(Debug)]
pub(super) struct UdmabufProbeBuffer {
    pages: SealedMemfdPages,
    dma_buf: OwnedFd,
}

impl UdmabufProbeBuffer {
    /// Буфер поверх нового memfd.
    pub(super) fn create(device: &UdmabufDevice) -> io::Result<Self> {
        let pages = SealedMemfdPages::create(PROBE_BUFFER_BYTES)?;
        let size = u64::try_from(pages.len())
            .map_err(|_| io::Error::new(io::ErrorKind::InvalidInput, "probe buffer too large"))?;
        let dma_buf = device.wrap_sealed_memfd(pages.memfd(), 0, size)?;
        Ok(Self { pages, dma_buf })
    }
}

impl CpuWriteProbeProducer for UdmabufProbeBuffer {
    fn dma_buf(&self) -> BorrowedFd<'_> {
        self.dma_buf.as_fd()
    }

    fn write_all_bytes(&mut self, value: u8) {
        // Обычная запись CPU через кэш — ровно как пишет декодер.
        self.pages.fill(value);
    }

    fn flush_cpu_cache(&mut self) {
        flush_cpu_cache_ranges([self.pages.bytes()]);
    }
}
