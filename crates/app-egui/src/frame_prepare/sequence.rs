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

    #[derive(Default)]
    struct RecordingFrameSequence {
        stages: Vec<FrameSequenceStage>,
        contract: FrameSequenceContract,
    }

    impl FrameSequenceObserver for RecordingFrameSequence {
        fn reached(&mut self, stage: FrameSequenceStage) {
            self.contract.reached(stage);
            self.stages.push(stage);
        }
    }

    #[test]
    fn recording_fake_preserves_runtime_sensitive_order() {
        let mut sequence = RecordingFrameSequence::default();
        for stage in EXPECTED_FRAME_SEQUENCE {
            sequence.reached(stage);
        }

        assert_eq!(
            sequence.stages,
            vec![
                FrameSequenceStage::WorkerEventDrain,
                FrameSequenceStage::WorkerEventRecord,
                FrameSequenceStage::DesktopPublish,
                FrameSequenceStage::EguiOutput,
                FrameSequenceStage::MaterializerLookup,
                FrameSequenceStage::RendererSubmit,
            ]
        );
    }

    #[test]
    #[should_panic]
    fn recording_fake_rejects_reordered_runtime_stage() {
        let mut sequence = RecordingFrameSequence::default();
        sequence.reached(FrameSequenceStage::DesktopPublish);
    }
}
