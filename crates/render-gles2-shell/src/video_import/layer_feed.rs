//! «Сделать кадр текущим на видеослое»: загрузка плоскостей (программный путь) или импорт
//! DMA-BUF (аппаратный) с исходами, которые не сливаются в один `bool`.
//!
//! Функция [`show_frame_on_layer`] нужна дважды: при `materialize` (новый кадр) и при рисовании
//! ключа, который уже не текущий на слое (возврат картинки по рецепту ключа). Поэтому она не
//! знает ни склада, ни ключей — только видеослой, контекст, источник ресурсов и кадр.

use std::time::Duration;

use gles2_egui::Gles2Context;
use gles2_video::{UploadOutcome, VideoLayerRenderer};
use render_shell_api::{FrameImportError, FrameImportUnsupportedReason, VideoImportPath};
use video_backend_api::PresentFrameResourceDescriptorLookup;
use video_core::DecodedFrame;
use video_frame_contract::VideoFrameTransferPath;

use super::dma_buf_outcome::layer_outcome_for_dma_buf;
use super::prepared::FrameSource;
use crate::dma_buf_frame::materialize_dma_buf_frame;
use crate::host_block_import::{HostBlockZeroCopy, ZeroCopyShow};
use crate::host_planar::{HostPlanarFrameError, host_planar_frame_upload};

/// Итог показа кадра на видеослое и суммарное ожидание lock-ов пула ресурсов провайдера.
#[derive(Debug)]
#[must_use = "исход определяет, можно ли рисовать кадр"]
pub(crate) struct LayerShow {
    /// Что произошло.
    pub(crate) outcome: LayerShowOutcome,
    /// Сколько поток рендера ждал lock-и пула провайдера (для диагностики).
    pub(crate) resource_pool_lock_wait: Duration,
}

/// Что произошло с кадром.
#[derive(Debug)]
pub(crate) enum LayerShowOutcome {
    /// Кадр стал текущим на видеослое.
    Shown,
    /// Нет свободного слота / провайдер занят: повторить без ожидания.
    Busy,
    /// Провайдер не знает такого кадра.
    Missing,
    /// Этот кадр данный импорт не показывает (ограничение, не поломка).
    Unsupported(FrameImportUnsupportedReason),
    /// Поломка: фатальное состояние провайдера или отказ драйвера.
    Failed(FrameImportError),
}

/// Делает кадр `frame` текущим на видеослое `video`, читая его ресурсы из `source`.
pub(crate) fn show_frame_on_layer(
    video: &mut VideoLayerRenderer,
    context: &Gles2Context,
    software_frames: &mut HostBlockZeroCopy,
    source: &FrameSource,
    frame: &DecodedFrame,
) -> LayerShow {
    match source.path() {
        VideoImportPath::SoftwareHostUpload => {
            show_host_planar(video, context, software_frames, source, frame)
        }
        VideoImportPath::HardwareZeroCopy => show_dma_buf(video, context, source, frame),
    }
}

/// Аппаратный путь: идентичность → кэш → импорт (`materialize_dma_buf_frame`).
fn show_dma_buf(
    video: &mut VideoLayerRenderer,
    context: &Gles2Context,
    source: &FrameSource,
    frame: &DecodedFrame,
) -> LayerShow {
    if !matches!(
        frame.frame_contract.transfer_path,
        VideoFrameTransferPath::HardwareZeroCopy { .. }
    ) {
        return unsupported_transfer_path();
    }
    let materialization = materialize_dma_buf_frame(video, context, source.provider(), frame);
    LayerShow {
        outcome: layer_outcome_for_dma_buf(materialization.outcome),
        resource_pool_lock_wait: materialization.resource_pool_lock_wait,
    }
}

/// Программный путь: дескриптор плоскостей из провайдера → без CPU-копии (блок декодера через
/// udmabuf, `crate::host_block_import`), а если нельзя — одна копия в кольцо слотов.
fn show_host_planar(
    video: &mut VideoLayerRenderer,
    context: &Gles2Context,
    software_frames: &mut HostBlockZeroCopy,
    source: &FrameSource,
    frame: &DecodedFrame,
) -> LayerShow {
    if frame.frame_contract.transfer_path != VideoFrameTransferPath::SoftwareHostUpload {
        return unsupported_transfer_path();
    }
    let lookup = source
        .provider()
        .try_resource_descriptor_lookup(frame.resource_handle);
    let resource_pool_lock_wait = lookup.resource_pool_lock_wait();
    let outcome = match lookup {
        PresentFrameResourceDescriptorLookup::Ready { descriptor, .. } => {
            if software_frames.try_show(video, context, frame, &descriptor) == ZeroCopyShow::Shown {
                return LayerShow {
                    outcome: LayerShowOutcome::Shown,
                    resource_pool_lock_wait,
                };
            }
            match host_planar_frame_upload(frame, &descriptor) {
                Ok(upload) => match video.upload(context, &upload) {
                    Ok(UploadOutcome::Uploaded(_)) => {
                        software_frames.note_frame_uploaded();
                        LayerShowOutcome::Shown
                    }
                    Ok(UploadOutcome::AlreadyCurrent) => LayerShowOutcome::Shown,
                    Ok(UploadOutcome::Busy(_)) => LayerShowOutcome::Busy,
                    Err(error) => LayerShowOutcome::Failed(FrameImportError::ImportFailed {
                        message: error.to_string(),
                    }),
                },
                Err(error) => host_planar_error_outcome(error),
            }
        }
        PresentFrameResourceDescriptorLookup::Busy { .. } => LayerShowOutcome::Busy,
        PresentFrameResourceDescriptorLookup::Missing { .. } => LayerShowOutcome::Missing,
        PresentFrameResourceDescriptorLookup::Fatal { .. } => {
            LayerShowOutcome::Failed(FrameImportError::ProviderFatal)
        }
    };
    LayerShow {
        outcome,
        resource_pool_lock_wait,
    }
}

/// Кадр другого пути передачи, чем у активного импорта (DMA-BUF на программном и наоборот).
fn unsupported_transfer_path() -> LayerShow {
    LayerShow {
        outcome: LayerShowOutcome::Unsupported(
            FrameImportUnsupportedReason::TransferPathNotHandledByActiveImport,
        ),
        resource_pool_lock_wait: Duration::ZERO,
    }
}

/// Отказ разбора программного дескриптора: ограничение или нарушение контракта провайдером.
fn host_planar_error_outcome(error: HostPlanarFrameError) -> LayerShowOutcome {
    match error {
        HostPlanarFrameError::DmaBufDescriptor | HostPlanarFrameError::NotSoftwareHostUpload(_) => {
            LayerShowOutcome::Unsupported(
                FrameImportUnsupportedReason::TransferPathNotHandledByActiveImport,
            )
        }
        HostPlanarFrameError::UnsupportedLayout(_) => LayerShowOutcome::Unsupported(
            FrameImportUnsupportedReason::LayoutNotHandledByActiveImport,
        ),
        // Дескриптор противоречит контракту или нечитаем: провайдер нарушил обещание — ошибка.
        error @ (HostPlanarFrameError::ContractRejected { .. }
        | HostPlanarFrameError::PlaneUnavailable { .. }
        | HostPlanarFrameError::PlaneRows { .. }) => {
            LayerShowOutcome::Failed(FrameImportError::ImportFailed {
                message: error.to_string(),
            })
        }
    }
}

#[cfg(test)]
mod tests;
