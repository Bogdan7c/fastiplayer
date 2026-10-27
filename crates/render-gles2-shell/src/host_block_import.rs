//! Кадр программного декодера без CPU-копии (сессия 12 плана `user/gles2-renderer/`).
//!
//! Обычный программный путь копирует кадр в текстуры (`glTexSubImage2D`, копию делает драйвер на
//! CPU нашего потока). Здесь вместо копии: блок памяти кадра декодера (запечатанный memfd,
//! `video_core::HostFrameMemoryKind::SealedMemfd` — его заказывает приложение для GLES2) →
//! `/dev/udmabuf` → dma-buf → три `EGLImage` R8 → те же текстуры и та же программа, что у копии
//! (`gles2_video::VideoLayerRenderer::import_planar_dma_buf_frame`).
//!
//! # Владелец состояния
//! [`HostBlockZeroCopy`] (одно на рендер): доступен ли путь на этой машине, нужен ли сброс кэша
//! CPU, dma-buf блоков, счётчики. Решение «без копии или копия» принимается на каждый кадр в
//! [`HostBlockZeroCopy::try_show`]; любой отказ — [`ZeroCopyShow::UseUpload`], и кадр идёт
//! прежней копией (fail-closed, решение владельца 12).
//!
//! # Доступность (один раз, при первом подходящем кадре)
//! 1. Драйвер умеет импорт DMA-BUF (уровень zero-copy `gles2-egui`).
//! 2. `/dev/udmabuf` открывается (модуль ядра + права; без прав путь просто недоступен — плеер
//!    не трогает права и udev, решение плана).
//! 3. Самопроверка `gles2_video::probe_cpu_write_visibility` на своём маленьком udmabuf: видит ли
//!    GPU запись CPU без сброса кэша (AMD, Intel с LLC, llvmpipe), только после сброса (Intel без
//!    LLC — Ironlake, T410) или вообще нет (путь выключен). Решение по поведению драйвера, а не по
//!    имени GPU (решение владельца 12).
//!
//! # Инварианты
//! - Сброс кэша CPU по видимым строкам — на КАЖДЫЙ новый кадр, если самопроверка его требует:
//!   декодер пишет блок заново при каждом переиспользовании, а GPU без LLC читает мимо кэша.
//! - Кадр возвращается декодеру только после fence кадра показа (решение владельца 30): блок не
//!   перепишется, пока GPU его читает. Это обеспечивает обёртка освобождения, а не этот модуль.
//! - dma-buf блоков ограничены ёмкостью ([`BlockDmaBufCapacity`]) и сбрасываются при смене
//!   декодера ([`HostBlockZeroCopy::decoder_changed`]): иначе udmabuf удерживал бы память блоков,
//!   которые декодер уже выбросил.
//! - Ошибка импорта выключает путь до пересоздания рендера (предупреждение один раз).
//! - Только 8 бит (решение владельца 12); 10/12 бит и кадры без memfd-блока (AV1, wgpu-память)
//!   идут копией без шума в логе.

mod block_dma_bufs;
#[allow(
    unsafe_code,
    reason = "модуль безопасности вызовов ядра Linux и сброса кэша CPU; каждый блок с // SAFETY:"
)]
mod kernel;
mod probe_buffer;

use std::io;

use gles2_egui::{DrmModifier, Gles2Context};
use gles2_video::{
    CachedDmaBufFrame, CpuWriteVisibility, DmaBufObjectIdentity, DmaBufPlanarFrame,
    DmaBufPlanarIdentity, DmaBufPlaneLocation, PlanarFrameUpload, SampleDepth, VideoLayerRenderer,
    probe_cpu_write_visibility,
};
use video_core::{
    DecodedFrame, FrameResourceDescriptor, HostPlanarFrameDescriptor, HostPlanarImportableMemory,
};

pub(crate) use self::block_dma_bufs::BlockDmaBufCapacity;
use self::block_dma_bufs::{BlockDmaBufLookup, BlockDmaBufs};
use self::kernel::{CPU_CACHE_FLUSH_SUPPORT, CpuCacheFlushSupport, UdmabufDevice};
use self::probe_buffer::UdmabufProbeBuffer;
use crate::capabilities::HardwareZeroCopy;
use crate::host_planar::host_planar_frame_upload;

/// Нужен ли сброс кэша CPU перед тем, как GPU прочитает новый кадр.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CpuCacheFlushPolicy {
    /// GPU видит запись CPU сам (общий кэш или когерентный доступ).
    NotNeeded,
    /// GPU читает мимо кэша CPU: видимые строки каждого кадра сбрасываются `CLFLUSH`.
    FlushVisibleRows,
}

/// Почему путь без копии выключен на этой машине или в этом рендере.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SoftwareZeroCopyDisabled {
    /// Драйвер не умеет импорт DMA-BUF в текстуру.
    DriverCannotImportDmaBuf,
    /// `/dev/udmabuf` не открывается (нет модуля ядра или прав).
    UdmabufUnavailable,
    /// Буфер самопроверки не создан (memfd/udmabuf отказали).
    ProbeBufferUnavailable,
    /// Самопроверка: GPU не видит запись CPU даже после сброса кэша.
    CpuWritesNotVisible,
    /// Самопроверка требует сброса кэша, а эта архитектура его не умеет.
    CacheFlushUnsupported,
    /// Самопроверка сама отказала (шейдер, GL, импорт).
    ProbeFailed,
    /// Импорт кадра отказал во время показа: путь выключен до пересоздания рендера.
    ImportFailed,
}

/// Состояние пути без копии.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SoftwareZeroCopyState {
    /// Подходящих кадров ещё не было: самопроверка не делалась.
    NotProbed,
    /// Путь включён.
    Enabled(CpuCacheFlushPolicy),
    /// Путь выключен, кадры идут копией.
    Disabled(SoftwareZeroCopyDisabled),
}

/// Счётчики пути программного кадра за жизнь рендера.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct SoftwareZeroCopyStats {
    /// Состояние пути.
    pub state: SoftwareZeroCopyState,
    /// Кадров показано без CPU-копии.
    pub frames_without_cpu_copy: u64,
    /// Кадров показано копией (`glTexSubImage2D`).
    pub frames_through_upload: u64,
    /// Создано dma-buf блоков (`UDMABUF_CREATE`): растёт до числа блоков пула, а не с кадрами.
    pub dma_bufs_created: u64,
    /// Кадров, перед которыми сброшен кэш CPU.
    pub cpu_cache_flushes: u64,
}

/// Итог попытки показать кадр без копии.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[must_use = "UseUpload значит: кадр не показан, его надо загрузить копией"]
pub(crate) enum ZeroCopyShow {
    /// Кадр текущий на видеослое без CPU-копии.
    Shown,
    /// Путь не применим к кадру или выключен: показать копией.
    UseUpload,
}

/// Доступность пути и то, что для неё нужно.
#[derive(Debug)]
enum Availability {
    NotProbed,
    Enabled {
        device: UdmabufDevice,
        flush: CpuCacheFlushPolicy,
    },
    Disabled(SoftwareZeroCopyDisabled),
}

impl Availability {
    const fn state(&self) -> SoftwareZeroCopyState {
        match self {
            Self::NotProbed => SoftwareZeroCopyState::NotProbed,
            Self::Enabled { flush, .. } => SoftwareZeroCopyState::Enabled(*flush),
            Self::Disabled(reason) => SoftwareZeroCopyState::Disabled(*reason),
        }
    }
}

/// Владелец пути «программный кадр без CPU-копии» одного рендера.
#[derive(Debug)]
pub(crate) struct HostBlockZeroCopy {
    driver: HardwareZeroCopy,
    availability: Availability,
    dma_bufs: BlockDmaBufs,
    stats: SoftwareZeroCopyStats,
}

/// Кадр, который годится для пути без копии: плоскости, блок и его memfd.
struct EligibleFrame<'d> {
    upload: PlanarFrameUpload<'d>,
    host: &'d HostPlanarFrameDescriptor,
    memory: HostPlanarImportableMemory,
}

impl HostBlockZeroCopy {
    /// `driver` — умеет ли драйвер импорт DMA-BUF (тот же уровень, что у аппаратного пути).
    pub(crate) const fn new(driver: HardwareZeroCopy) -> Self {
        Self {
            driver,
            availability: Availability::NotProbed,
            dma_bufs: BlockDmaBufs::new(),
            stats: SoftwareZeroCopyStats {
                state: SoftwareZeroCopyState::NotProbed,
                frames_without_cpu_copy: 0,
                frames_through_upload: 0,
                dma_bufs_created: 0,
                cpu_cache_flushes: 0,
            },
        }
    }

    /// Счётчики и состояние.
    pub(crate) const fn stats(&self) -> SoftwareZeroCopyStats {
        let mut stats = self.stats;
        stats.state = self.availability.state();
        stats
    }

    /// Кадр показан копией (для счётчика пути).
    pub(crate) const fn note_frame_uploaded(&mut self) {
        self.stats.frames_through_upload += 1;
    }

    /// Новый декодер: блоков прошлого больше нет — их dma-buf закрываются; ёмкость — по плану.
    pub(crate) fn decoder_changed(&mut self, capacity: BlockDmaBufCapacity) {
        self.log_summary();
        self.dma_bufs.purge();
        self.dma_bufs.set_capacity(capacity);
    }

    /// Итог пути в лог (смена декодера, закрытие рендера): доказательство «без копии» в живых
    /// прогонах. Без программных кадров молчит.
    pub(crate) fn log_summary(&self) {
        if self.stats.frames_without_cpu_copy == 0 && self.stats.frames_through_upload == 0 {
            return;
        }
        log::info!(
            "GLES2: software frames so far: {} without CPU copy, {} uploaded, {} block dma-bufs \
             created, {} CPU cache flushes, state {:?}",
            self.stats.frames_without_cpu_copy,
            self.stats.frames_through_upload,
            self.stats.dma_bufs_created,
            self.stats.cpu_cache_flushes,
            self.availability.state()
        );
    }

    /// Показать кадр без CPU-копии, если это возможно. Иначе [`ZeroCopyShow::UseUpload`]: кадр
    /// не тронут, видеослой в прежнем состоянии или без текущего кадра (тогда его покажет копия).
    pub(crate) fn try_show(
        &mut self,
        video: &mut VideoLayerRenderer,
        context: &Gles2Context,
        frame: &DecodedFrame,
        descriptor: &FrameResourceDescriptor,
    ) -> ZeroCopyShow {
        let Some(eligible) = eligible_frame(frame, descriptor) else {
            return ZeroCopyShow::UseUpload;
        };
        self.ensure_probed(context);
        let Availability::Enabled { device, flush } = &self.availability else {
            return ZeroCopyShow::UseUpload;
        };
        let flush = *flush;
        let block = eligible.memory.block();
        let Some(memfd) = block.sealed_memfd() else {
            return ZeroCopyShow::UseUpload;
        };
        let region_len = block.region().len();
        let attempt = match self
            .dma_bufs
            .get_or_create(device, block.id(), memfd, region_len)
        {
            Ok((dma_buf, lookup)) => {
                if lookup == BlockDmaBufLookup::Created {
                    self.stats.dma_bufs_created += 1;
                }
                show_block(video, context, &eligible, dma_buf, flush)
            }
            Err(error) => Err(ZeroCopyFailure::Udmabuf(error)),
        };
        match attempt {
            Ok(flushed) => {
                self.stats.frames_without_cpu_copy += 1;
                if flushed == CacheFlushed::Yes {
                    self.stats.cpu_cache_flushes += 1;
                }
                ZeroCopyShow::Shown
            }
            Err(ZeroCopyFailure::LayoutNotRepresentable) => ZeroCopyShow::UseUpload,
            Err(failure) => {
                log::warn!(
                    "GLES2: software frames without CPU copy are disabled until the renderer is \
                     recreated, frames are uploaded instead: {failure}"
                );
                self.availability = Availability::Disabled(SoftwareZeroCopyDisabled::ImportFailed);
                ZeroCopyShow::UseUpload
            }
        }
    }

    /// Самопроверка доступности — один раз за жизнь рендера.
    fn ensure_probed(&mut self, context: &Gles2Context) {
        if !matches!(self.availability, Availability::NotProbed) {
            return;
        }
        self.availability = probe_availability(self.driver, context);
        match &self.availability {
            Availability::Enabled { flush, .. } => log::info!(
                "GLES2: software frames are shown without CPU copy (udmabuf), CPU cache flush: {}",
                match flush {
                    CpuCacheFlushPolicy::NotNeeded => "not needed",
                    CpuCacheFlushPolicy::FlushVisibleRows => "visible rows of every frame",
                }
            ),
            Availability::Disabled(reason) => log::info!(
                "GLES2: software frames are uploaded (one copy), zero-copy is unavailable: \
                 {reason:?}"
            ),
            Availability::NotProbed => {}
        }
    }

    /// Только тесты: включить путь с заданной политикой сброса, минуя самопроверку (проверка
    /// ветки сброса на машине, где GPU видит запись CPU и без него).
    #[cfg(test)]
    pub(crate) fn enable_for_tests(&mut self, flush: CpuCacheFlushPolicy) -> io::Result<()> {
        self.availability = Availability::Enabled {
            device: UdmabufDevice::open()?,
            flush,
        };
        Ok(())
    }
}

/// Кадр годится: программный 8 бит, лежит в одном блоке с memfd.
fn eligible_frame<'d>(
    frame: &DecodedFrame,
    descriptor: &'d FrameResourceDescriptor,
) -> Option<EligibleFrame<'d>> {
    let FrameResourceDescriptor::HostPlanar(host) = descriptor else {
        return None;
    };
    // Ошибки разбора здесь не важны: копия разберёт кадр ещё раз и честно сообщит о них.
    let upload = host_planar_frame_upload(frame, descriptor).ok()?;
    if upload.layout.depth != SampleDepth::Bits8 {
        return None;
    }
    let memory = host.importable_memory().ok()??;
    memory.block().sealed_memfd()?;
    Some(EligibleFrame {
        upload,
        host,
        memory,
    })
}

/// Решение о доступности по драйверу, `/dev/udmabuf` и самопроверке.
fn probe_availability(driver: HardwareZeroCopy, context: &Gles2Context) -> Availability {
    if driver == HardwareZeroCopy::Unavailable {
        return Availability::Disabled(SoftwareZeroCopyDisabled::DriverCannotImportDmaBuf);
    }
    let device = match UdmabufDevice::open() {
        Ok(device) => device,
        Err(error) => {
            log::info!("GLES2: /dev/udmabuf is not accessible: {error}");
            return Availability::Disabled(SoftwareZeroCopyDisabled::UdmabufUnavailable);
        }
    };
    let mut buffer = match UdmabufProbeBuffer::create(&device) {
        Ok(buffer) => buffer,
        Err(error) => {
            log::warn!("GLES2: the zero-copy probe buffer was not created: {error}");
            return Availability::Disabled(SoftwareZeroCopyDisabled::ProbeBufferUnavailable);
        }
    };
    match probe_cpu_write_visibility(context, &mut buffer) {
        Ok(visibility) => match flush_policy_for(visibility, CPU_CACHE_FLUSH_SUPPORT) {
            Ok(flush) => Availability::Enabled { device, flush },
            Err(reason) => Availability::Disabled(reason),
        },
        Err(error) => {
            log::warn!("GLES2: the zero-copy probe failed: {error}");
            Availability::Disabled(SoftwareZeroCopyDisabled::ProbeFailed)
        }
    }
}

/// Чистое правило: итог самопроверки + умение сбросить кэш → политика сброса или причина отказа.
fn flush_policy_for(
    visibility: CpuWriteVisibility,
    flush_support: CpuCacheFlushSupport,
) -> Result<CpuCacheFlushPolicy, SoftwareZeroCopyDisabled> {
    match (visibility, flush_support) {
        (CpuWriteVisibility::CoherentWithoutCacheFlush, _) => Ok(CpuCacheFlushPolicy::NotNeeded),
        (CpuWriteVisibility::VisibleAfterCacheFlush, CpuCacheFlushSupport::Clflush) => {
            Ok(CpuCacheFlushPolicy::FlushVisibleRows)
        }
        (CpuWriteVisibility::VisibleAfterCacheFlush, CpuCacheFlushSupport::Unavailable) => {
            Err(SoftwareZeroCopyDisabled::CacheFlushUnsupported)
        }
        (CpuWriteVisibility::NotVisible, _) => Err(SoftwareZeroCopyDisabled::CpuWritesNotVisible),
    }
}

/// Сброшен ли кэш CPU перед показом этого кадра.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum CacheFlushed {
    Yes,
    No,
}

/// Почему кадр не показан без копии.
#[derive(Debug, thiserror::Error)]
enum ZeroCopyFailure {
    /// Смещение или шаг не помещаются в атрибуты EGL: этот кадр — копией, путь остаётся.
    #[error("the frame layout cannot be described to EGL")]
    LayoutNotRepresentable,
    #[error("udmabuf for a decoder block failed: {0}")]
    Udmabuf(io::Error),
    #[error("the decoder block was rejected as a dma-buf frame: {0}")]
    Frame(gles2_video::DmaBufPlanarFrameError),
    #[error("dma-buf import failed: {0}")]
    Import(gles2_video::VideoImportError),
}

/// Сбросить кэш (если нужно) и сделать кадр текущим из кэша импорта или новым импортом.
fn show_block(
    video: &mut VideoLayerRenderer,
    context: &Gles2Context,
    eligible: &EligibleFrame<'_>,
    dma_buf: &block_dma_bufs::BlockDmaBuf,
    flush: CpuCacheFlushPolicy,
) -> Result<CacheFlushed, ZeroCopyFailure> {
    let identity = planar_identity(eligible, dma_buf.object())?;
    let key = eligible.upload.key;
    // Сброс ДО того, как GPU получит команды, читающие блок (импорт сам данные не читает, их
    // читает рисование кадра). На каждый новый кадр: блок переписан декодером.
    let flushed = match flush {
        CpuCacheFlushPolicy::NotNeeded => CacheFlushed::No,
        CpuCacheFlushPolicy::FlushVisibleRows => {
            flush_visible_rows(eligible.host);
            CacheFlushed::Yes
        }
    };
    match video.show_cached_planar_dma_buf_frame(&identity, key) {
        CachedDmaBufFrame::Shown | CachedDmaBufFrame::AlreadyCurrent => Ok(flushed),
        CachedDmaBufFrame::NotCached => {
            let frame = DmaBufPlanarFrame::new(key, &identity, dma_buf.fd())
                .map_err(ZeroCopyFailure::Frame)?;
            video
                .import_planar_dma_buf_frame(context, &frame)
                .map_err(ZeroCopyFailure::Import)?;
            Ok(flushed)
        }
    }
}

/// Паспорт кадра для кэша импорта: блок (его номер — ресурс), объект dma-buf, раскладка и
/// места плоскостей. Порядок плоскостей дескриптора — Y, U, V (как у `host_planar_frame_upload`).
fn planar_identity(
    eligible: &EligibleFrame<'_>,
    object: kernel::KernelObjectIdentity,
) -> Result<DmaBufPlanarIdentity, ZeroCopyFailure> {
    let location = |plane_index: usize| -> Option<DmaBufPlaneLocation> {
        let offset = eligible.memory.plane_offset(plane_index)?;
        let stride = eligible.host.planes.get(plane_index)?.stride;
        Some(DmaBufPlaneLocation {
            offset: u32::try_from(offset).ok()?,
            pitch: u32::try_from(stride).ok()?,
        })
    };
    let (Some(luma), Some(chroma_u), Some(chroma_v)) = (location(0), location(1), location(2))
    else {
        return Err(ZeroCopyFailure::LayoutNotRepresentable);
    };
    Ok(DmaBufPlanarIdentity {
        resource_id: eligible.memory.block().id().get(),
        object: DmaBufObjectIdentity {
            device: object.device,
            inode: object.inode,
            size: object.size,
            modifier: DrmModifier::LINEAR.0,
        },
        layout: eligible.upload.layout,
        luma_extent: eligible.upload.luma_extent,
        planes: [luma, chroma_u, chroma_v],
    })
}

/// Сбросить из кэша CPU видимые строки всех плоскостей кадра (хвосты строк GPU не читает).
fn flush_visible_rows(host: &HostPlanarFrameDescriptor) {
    for plane_index in 0..host.planes.len() {
        // Дескриптор уже проверен при разборе кадра (`eligible_frame`): недоступной плоскости
        // здесь не бывает. Строки идут итератором — без аллокаций на кадр.
        let Ok(block) = host.visible_plane_block(plane_index) else {
            continue;
        };
        let visible_rows = usize::try_from(block.visible_height).unwrap_or(0);
        kernel::flush_cpu_cache_ranges((0..visible_rows).filter_map(|row| {
            let start = row.checked_mul(block.stride)?;
            block
                .bytes
                .get(start..start.checked_add(block.visible_row_bytes)?)
        }));
    }
}

#[cfg(test)]
mod tests;
