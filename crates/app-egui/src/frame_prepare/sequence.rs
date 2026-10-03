//! Проверяемый контракт порядка side-effect стадий одного кадра.

/// Side-effect стадии, порядок которых нельзя менять при decomposition.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) enum FrameSequenceStage {
    WorkerEventDrain,
    WorkerEventRecord,
    DesktopPublish,
    EguiOutput,
    MaterializerLookup,
    RendererSubmit,
}

/// Наблюдатель позволяет тесту записать sequence без GPU/window fixture.
pub(super) trait FrameSequenceObserver {
    fn reached(&mut self, stage: FrameSequenceStage);
}

const EXPECTED_FRAME_SEQUENCE: [FrameSequenceStage; 6] = [
    FrameSequenceStage::WorkerEventDrain,
    FrameSequenceStage::WorkerEventRecord,
    FrameSequenceStage::DesktopPublish,
    FrameSequenceStage::EguiOutput,
    FrameSequenceStage::MaterializerLookup,
    FrameSequenceStage::RendererSubmit,
];

/// Лёгкий production contract проверяет перестановку стадий в debug/test сборках.
#[derive(Default)]
pub(super) struct FrameSequenceContract {
    next_stage_index: usize,
}

impl FrameSequenceObserver for FrameSequenceContract {
    fn reached(&mut self, stage: FrameSequenceStage) {
        debug_assert_eq!(
            EXPECTED_FRAME_SEQUENCE.get(self.next_stage_index),
            Some(&stage)
        );
        self.next_stage_index += 1;
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Production-контракт принимает ожидаемый порядок стадий кадра целиком.
    #[test]
    fn contract_accepts_full_expected_frame_sequence() {
        let mut contract = FrameSequenceContract::default();
        for stage in EXPECTED_FRAME_SEQUENCE {
            contract.reached(stage);
        }

        assert_eq!(contract.next_stage_index, EXPECTED_FRAME_SEQUENCE.len());
    }

    /// Перестановка стадий (publish до drain-а worker events) ловится контрактом.
    #[test]
    #[should_panic]
    fn contract_rejects_reordered_runtime_stage() {
        let mut contract = FrameSequenceContract::default();
        contract.reached(FrameSequenceStage::DesktopPublish);
    }
}
