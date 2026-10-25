//! Перевод отказов аппаратного пути (`dma_buf_frame`) в словарь контракта рендера.
//!
//! # Две разные причины «не показываем»
//!
//! - Настоящий отказ дескриптора (`DescriptorRejected`, валидатор `video-core`) идёт как
//!   `FrameImportUnsupportedReason::DmaBufDescriptorRejected` с исходной причиной.
//! - Всё остальное — «рендер не умеет»: драйвер не импортирует формат, формат не NV12,
//!   провайдер отдал память процесса. Это `RendererCannotShowHardwareFrame`: честная причина
//!   (решение владельца 41), а не подмена под «неверный дескриптор». Программный фолбэк
//!   приложения запускается по обеим.

use render_shell_api::{FrameImportError, FrameImportUnsupportedReason, HardwareFrameRefusal};

use crate::dma_buf_frame::{DmaBufMaterializeOutcome, DmaBufUnsupportedReason};

use super::layer_feed::LayerShowOutcome;

/// Причина «аппаратный кадр не показываем» в словаре контракта рендера.
#[must_use]
pub(crate) fn unsupported_reason_for_dma_buf(
    reason: &DmaBufUnsupportedReason,
) -> FrameImportUnsupportedReason {
    match reason {
        DmaBufUnsupportedReason::DescriptorRejected(rejection) => {
            FrameImportUnsupportedReason::DmaBufDescriptorRejected(rejection.clone())
        }
        DmaBufUnsupportedReason::HostPlanarDescriptor => {
            FrameImportUnsupportedReason::RendererCannotShowHardwareFrame(
                HardwareFrameRefusal::HostMemoryDescriptor,
            )
        }
        DmaBufUnsupportedReason::FrameFormat(error) => {
            FrameImportUnsupportedReason::RendererCannotShowHardwareFrame(
                HardwareFrameRefusal::FrameFormatNotShown {
                    details: error.to_string(),
                },
            )
        }
        DmaBufUnsupportedReason::DriverCannotImport(error) => {
            FrameImportUnsupportedReason::RendererCannotShowHardwareFrame(
                HardwareFrameRefusal::DriverCannotImport {
                    details: error.to_string(),
                },
            )
        }
    }
}

/// Итог аппаратного показа кадра в итог «кадр на видеослое».
#[must_use]
pub(super) fn layer_outcome_for_dma_buf(outcome: DmaBufMaterializeOutcome) -> LayerShowOutcome {
    match outcome {
        DmaBufMaterializeOutcome::Shown(_) => LayerShowOutcome::Shown,
        DmaBufMaterializeOutcome::Busy => LayerShowOutcome::Busy,
        DmaBufMaterializeOutcome::Missing => LayerShowOutcome::Missing,
        DmaBufMaterializeOutcome::ProviderFatal => {
            LayerShowOutcome::Failed(FrameImportError::ProviderFatal)
        }
        DmaBufMaterializeOutcome::Unsupported(reason) => {
            LayerShowOutcome::Unsupported(unsupported_reason_for_dma_buf(&reason))
        }
        DmaBufMaterializeOutcome::ImportFailed(error) => {
            LayerShowOutcome::Failed(FrameImportError::ImportFailed {
                message: error.to_string(),
            })
        }
    }
}

#[cfg(test)]
mod tests;
