//! Отображение запечатанного memfd — второй вид памяти блоков кадров (GLES2, сессия 12).
//!
//! Зачем. GLES2-рендер импортирует блок не через Vulkan `userptr`, а через dma-buf:
//! `/dev/udmabuf` умеет делать dma-buf только из страниц memfd. Поэтому для рендера
//! GLES2 декодер выделяет блоки как `memfd_create` + `ftruncate` + `mmap(MAP_SHARED)`.
//!
//! Печати (`F_SEAL_SHRINK | F_SEAL_GROW | F_SEAL_SEAL`): udmabuf требует минимум
//! `F_SEAL_SHRINK` (иначе файл мог бы усохнуть под импортом GPU); `F_SEAL_WRITE` НЕ
//! ставим — декодер продолжает писать в блок. Весь `unsafe` memfd/mmap живёт здесь.

use std::io;
use std::os::fd::{AsFd, AsRawFd, BorrowedFd, FromRawFd, OwnedFd};
use std::ptr::NonNull;

use thiserror::Error;

/// Имя memfd для `/proc/<pid>/fd` и отладки (на семантику не влияет).
const MEMFD_DEBUG_NAME: &std::ffi::CStr = c"fastiplayer-frame-block";

/// Почему блок memfd не удалось выделить (отказ системы, не ошибка программы).
#[derive(Debug, Error)]
pub(crate) enum SealedMemfdError {
    /// Длина не помещается в `off_t`.
    #[error("memfd block length {len} does not fit off_t")]
    LengthOutOfRange {
        /// Запрошенная длина.
        len: usize,
    },

    /// `memfd_create` отказал (нет поддержки ядра, лимит дескрипторов).
    #[error("memfd_create failed: {0}")]
    Create(#[source] io::Error),

    /// `ftruncate` не выделил размер (ENOMEM/лимит tmpfs).
    #[error("ftruncate of memfd to {len} bytes failed: {source}")]
    Truncate {
        /// Запрошенная длина.
        len: usize,
        /// Причина от ядра.
        #[source]
        source: io::Error,
    },

    /// `F_ADD_SEALS` отказал.
    #[error("F_ADD_SEALS on memfd failed: {0}")]
    Seal(#[source] io::Error),

    /// `mmap` отказал (ENOMEM, лимит отображений).
    #[error("mmap of {len} bytes of memfd failed: {source}")]
    Map {
        /// Запрошенная длина.
        len: usize,
        /// Причина от ядра.
        #[source]
        source: io::Error,
    },
}

/// Запечатанный memfd вместе с его `MAP_SHARED` отображением на всю длину.
///
/// Владеет и fd, и отображением: `Drop` снимает отображение, затем закрывает fd.
#[derive(Debug)]
pub(super) struct SealedMemfdMapping {
    /// Дескриптор memfd; закрывается после `munmap` (порядок полей и `Drop`).
    memfd: OwnedFd,
    /// Начало отображения (выровнено по странице ядром).
    address: NonNull<u8>,
    /// Длина файла и отображения.
    len: usize,
}

impl SealedMemfdMapping {
    /// Создаёт memfd длиной `len`, запечатывает и отображает на чтение/запись.
    ///
    /// Память обнулена ядром (новые страницы файла). `len` — кратно странице (проверяет
    /// вызывающий `HostFrameBlock`).
    pub(super) fn create(len: usize) -> Result<Self, SealedMemfdError> {
        let file_len =
            libc::off_t::try_from(len).map_err(|_| SealedMemfdError::LengthOutOfRange { len })?;
        let memfd = create_sealable_memfd()?;
        // SAFETY: `memfd` — наш открытый дескриптор; `ftruncate` не трогает память процесса.
        if unsafe { libc::ftruncate(memfd.as_raw_fd(), file_len) } != 0 {
            return Err(SealedMemfdError::Truncate {
                len,
                source: io::Error::last_os_error(),
            });
        }
        let seals = libc::F_SEAL_SHRINK | libc::F_SEAL_GROW | libc::F_SEAL_SEAL;
        // SAFETY: `memfd` открыт и создан с MFD_ALLOW_SEALING; `F_ADD_SEALS` принимает int.
        if unsafe { libc::fcntl(memfd.as_raw_fd(), libc::F_ADD_SEALS, seals) } != 0 {
            return Err(SealedMemfdError::Seal(io::Error::last_os_error()));
        }
        // SAFETY: адрес null — ядро выбирает место; длина > 0 проверена вызывающим, offset 0;
        // `memfd` открыт и имеет длину `len`. Результат проверяется на MAP_FAILED ниже.
        let mapped = unsafe {
            libc::mmap(
                std::ptr::null_mut(),
                len,
                libc::PROT_READ | libc::PROT_WRITE,
                libc::MAP_SHARED,
                memfd.as_raw_fd(),
                0,
            )
        };
        if mapped == libc::MAP_FAILED {
            return Err(SealedMemfdError::Map {
                len,
                source: io::Error::last_os_error(),
            });
        }
        let address = NonNull::new(mapped.cast::<u8>()).ok_or_else(|| SealedMemfdError::Map {
            len,
            source: io::Error::other("mmap returned a null address"),
        })?;
        Ok(Self {
            memfd,
            address,
            len,
        })
    }

    /// Начало отображения.
    pub(super) fn address(&self) -> NonNull<u8> {
        self.address
    }

    /// Длина отображения в байтах.
    pub(super) fn len(&self) -> usize {
        self.len
    }

    /// Дескриптор memfd (живёт, пока жив блок).
    pub(super) fn memfd(&self) -> BorrowedFd<'_> {
        self.memfd.as_fd()
    }
}

/// `memfd_create(MFD_CLOEXEC | MFD_ALLOW_SEALING)`.
fn create_sealable_memfd() -> Result<OwnedFd, SealedMemfdError> {
    // SAFETY: имя — валидная C-строка с nul; флаги — допустимые константы libc.
    let raw_fd = unsafe {
        libc::memfd_create(
            MEMFD_DEBUG_NAME.as_ptr(),
            libc::MFD_CLOEXEC | libc::MFD_ALLOW_SEALING,
        )
    };
    if raw_fd < 0 {
        return Err(SealedMemfdError::Create(io::Error::last_os_error()));
    }
    // SAFETY: `memfd_create` вернул новый дескриптор, которым больше никто не владеет.
    Ok(unsafe { OwnedFd::from_raw_fd(raw_fd) })
}

impl Drop for SealedMemfdMapping {
    fn drop(&mut self) {
        // SAFETY: адрес и длина получены из успешного `mmap` в `create` и снимаются ровно
        // один раз (Drop); `memfd` закрывается после этого как поле.
        unsafe { libc::munmap(self.address.as_ptr().cast(), self.len) };
    }
}
