//! Software frame planning for future AVFrame-backed host-planar descriptors.

use thiserror::Error;
use video_frame_contract::VideoFrameContract;

use crate::codec_adapter::{
    FfmpegCodecAdapterError, SoftwareDecodeContractPlan, validate_software_frame_contract,
};

/// План владения software frame-ом без создания fake decoded frame-а.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct SoftwareFramePlan {
    /// Подтверждённый neutral software contract.
    contract_plan: SoftwareDecodeContractPlan,
}

impl SoftwareFramePlan {
    /// Создаёт plan только для valid SoftwareHostUpload contracts.
    pub fn new(frame_contract: VideoFrameContract) -> Result<Self, SoftwareFramePlanError> {
        let contract_plan = validate_software_frame_contract(frame_contract)?;

        Ok(Self { contract_plan })
    }

    /// Возвращает frame contract, который должен попасть в future `DecodedFrame`.
    #[must_use]
    pub const fn frame_contract(&self) -> VideoFrameContract {
        self.contract_plan.frame_contract()
    }
}

/// Ошибка планирования software frame ownership.
#[derive(Debug, Error, Clone, PartialEq, Eq)]
pub enum SoftwareFramePlanError {
    /// Ошибка neutral contract validation/adaptation.
    #[error(transparent)]
    Contract(#[from] FfmpegCodecAdapterError),
}

#[cfg(test)]
mod tests {
    use super::*;
    use video_frame_contract::VideoFrameContract;

    #[test]
    fn software_frame_plan_uses_neutral_contract_without_allocating_frame() {
        let contract = VideoFrameContract::host_yuv420_planar10le();
        let plan = SoftwareFramePlan::new(contract).unwrap();

        assert_eq!(plan.frame_contract(), contract);
    }
}
