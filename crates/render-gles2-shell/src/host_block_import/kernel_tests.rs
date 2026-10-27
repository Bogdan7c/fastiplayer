//! Тесты модуля безопасности: настоящие memfd, печати, udmabuf и `fstat`. Без `/dev/udmabuf`
//! тесты udmabuf пропускаются с причиной в stderr.

#![allow(
    unsafe_code,
    clippy::unwrap_used,
    clippy::expect_used,
    reason = "тесты: сырой fcntl/pread для проверки печатей и содержимого, громкие падения"
)]

use std::os::fd::{AsRawFd, FromRawFd, OwnedFd};

use super::*;

const PAGE: usize = 4096;

fn seals_of(fd: BorrowedFd<'_>) -> libc::c_int {
    // SAFETY: fd жив весь вызов; `F_GET_SEALS` только читает печати.
    unsafe { libc::fcntl(fd.as_raw_fd(), libc::F_GET_SEALS) }
}

fn read_back(fd: BorrowedFd<'_>, len: usize) -> Vec<u8> {
    let mut bytes = vec![0u8; len];
    // SAFETY: буфер длиной `len` живёт весь вызов, fd жив.
    let read = unsafe { libc::pread(fd.as_raw_fd(), bytes.as_mut_ptr().cast(), len, 0) };
    assert_eq!(read, len as isize, "pread: {}", io::Error::last_os_error());
    bytes
}

fn udmabuf_or_skip(test: &str) -> Option<UdmabufDevice> {
    match UdmabufDevice::open() {
        Ok(device) => Some(device),
        Err(error) => {
            eprintln!("{test}: skipped, /dev/udmabuf is not accessible: {error}");
            None
        }
    }
}

#[test]
fn sealed_pages_are_zeroed_sealed_and_cpu_writes_reach_the_memfd() {
    let mut pages = SealedMemfdPages::create(2 * PAGE).unwrap();
    assert_eq!(pages.len(), 2 * PAGE);
    assert!(pages.bytes().iter().all(|byte| *byte == 0));
    let seals = seals_of(pages.memfd());
    let wanted = libc::F_SEAL_SHRINK | libc::F_SEAL_GROW | libc::F_SEAL_SEAL;
    assert_eq!(seals & wanted, wanted, "seals {seals:#x}");
    assert_eq!(
        seals & libc::F_SEAL_WRITE,
        0,
        "udmabuf rejects write-sealed memfd"
    );

    pages.fill(0x5a);
    // Запись через отображение — это запись в страницы memfd (их читает и dma-buf).
    assert!(
        read_back(pages.memfd(), 2 * PAGE)
            .iter()
            .all(|byte| *byte == 0x5a)
    );
}

#[test]
fn sealed_pages_reject_lengths_that_are_not_whole_pages() {
    assert_eq!(
        SealedMemfdPages::create(0).unwrap_err().kind(),
        io::ErrorKind::InvalidInput
    );
    assert_eq!(
        SealedMemfdPages::create(PAGE + 1).unwrap_err().kind(),
        io::ErrorKind::InvalidInput
    );
}

#[test]
fn udmabuf_wraps_a_sealed_memfd_into_a_dma_buf_of_the_requested_size() {
    let Some(device) = udmabuf_or_skip("udmabuf_wraps_a_sealed_memfd") else {
        return;
    };
    let pages = SealedMemfdPages::create(3 * PAGE).unwrap();
    let dma_buf = device
        .wrap_sealed_memfd(pages.memfd(), PAGE as u64, 2 * PAGE as u64)
        .unwrap();
    let identity = kernel_object_identity(dma_buf.as_fd()).unwrap();
    assert_eq!(identity.size, 2 * PAGE as u64);
    // Каждый UDMABUF_CREATE — новый объект ядра (новый inode): паспорт различает импорты.
    let second = device
        .wrap_sealed_memfd(pages.memfd(), 0, PAGE as u64)
        .unwrap();
    assert_ne!(
        kernel_object_identity(second.as_fd()).unwrap().inode,
        identity.inode
    );
}

#[test]
fn udmabuf_refuses_a_memfd_without_the_shrink_seal() {
    let Some(device) = udmabuf_or_skip("udmabuf_refuses_a_memfd_without_the_shrink_seal") else {
        return;
    };
    // SAFETY: имя — статическая C-строка; результат проверяется и сразу идёт во владение.
    let raw = unsafe { libc::memfd_create(c"unsealed".as_ptr(), libc::MFD_CLOEXEC) };
    assert!(raw >= 0);
    // SAFETY: fd только что создан.
    let memfd = unsafe { OwnedFd::from_raw_fd(raw) };
    // SAFETY: fd открыт.
    assert_eq!(
        unsafe { libc::ftruncate(memfd.as_raw_fd(), PAGE as libc::off_t) },
        0
    );
    let error = device
        .wrap_sealed_memfd(memfd.as_fd(), 0, PAGE as u64)
        .unwrap_err();
    assert_eq!(error.raw_os_error(), Some(libc::EINVAL), "{error}");
}

#[test]
fn cache_flush_keeps_the_bytes_and_accepts_unaligned_and_empty_ranges() {
    let mut pages = SealedMemfdPages::create(PAGE).unwrap();
    pages.fill(0x33);
    let bytes = pages.bytes();
    flush_cpu_cache_ranges([&bytes[1..65], &bytes[100..100], &bytes[4000..]]);
    assert!(bytes.iter().all(|byte| *byte == 0x33));
    assert_eq!(
        CPU_CACHE_FLUSH_SUPPORT,
        if cfg!(target_arch = "x86_64") {
            CpuCacheFlushSupport::Clflush
        } else {
            CpuCacheFlushSupport::Unavailable
        }
    );
}
