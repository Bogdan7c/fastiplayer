//! Adapter helpers between neutral video contracts and future FFmpeg setup.

use thiserror::Error;
use video_frame_contract::{VideoFrameContract, VideoFrameContractValidationError};

/// План, который подтверждает: выбранный stream contract подходит software upload path-у.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct SoftwareDecodeContractPlan {
    /// Neutral contract, выбранный capability layer-ом.
    frame_contract: VideoFrameContract,
}

impl SoftwareDecodeContractPlan {
    /// Возвращает выбранный decoder->renderer contract.
    #[must_use]
    pub const fn frame_contract(&self) -> VideoFrameContract {
        self.frame_contract
    }
}

/// Ошибка адаптации neutral stream contract-а в FFmpeg software plan.
#[derive(Debug, Error, Clone, PartialEq, Eq)]
pub enum FfmpegCodecAdapterError {
    /// Contract сам по себе невалиден по правилам `video-frame-contract`.
    #[error("invalid software frame contract: {reason}")]
    InvalidFrameContract {
        /// Текст neutral validation error-а для diagnostics.
        reason: String,
    },

    /// FFmpeg software path не должен получать hardware zero-copy contract.
    #[error("FFmpeg software decode requires SoftwareHostUpload, got {transfer_path}")]
    NonSoftwareTransferPath {
        /// Diagnostic label фактического transfer path-а.
        transfer_path: String,
    },
}

/// Валидирует только neutral contract; codec-specific mapping придёт позже.
pub fn validate_software_frame_contract(
    frame_contract: VideoFrameContract,
) -> Result<SoftwareDecodeContractPlan, FfmpegCodecAdapterError> {
    frame_contract
        .validate()
        .map_err(map_contract_validation_error)?;

    if !frame_contract.transfer_path.is_software_host_upload() {
        return Err(FfmpegCodecAdapterError::NonSoftwareTransferPath {
            transfer_path: frame_contract.transfer_path.to_string(),
        });
    }

    Ok(SoftwareDecodeContractPlan { frame_contract })
}

/// Сохраняет typed boundary, но не заставляет `thiserror` зависеть от foreign type.
fn map_contract_validation_error(
    error: VideoFrameContractValidationError,
) -> FfmpegCodecAdapterError {
    FfmpegCodecAdapterError::InvalidFrameContract {
        reason: error.to_string(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use video_frame_contract::{DmaBufImageLayout, VideoFrameContract};

    #[test]
    fn software_contract_plan_accepts_host_upload_contract() {
        let contract = VideoFrameContract::host_yuv420_planar8();
        let plan = validate_software_frame_contract(contract).unwrap();

        assert_eq!(plan.frame_contract(), contract);
    }

    #[test]
    fn software_contract_plan_rejects_dma_buf_contract() {
        let hardware_contract = VideoFrameContract::dma_buf_nv12(DmaBufImageLayout::ComposedLayers);
        let error = validate_software_frame_contract(hardware_contract)
            .expect_err("hardware contract must not enter FFmpeg software adapter");

        assert!(matches!(
            error,
            FfmpegCodecAdapterError::NonSoftwareTransferPath { .. }
        ));
    }
}
