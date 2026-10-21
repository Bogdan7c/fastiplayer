//! Проверяемый контракт порядка side-effect стадий одного кадра.

/// Side-effect стадии, порядок которых нельзя менять при decomposition.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) enum FrameSequenceStage {
    WorkerEventDrain,
    WorkerEventRecord,
    DesktopPublish,
    EguiOutput,
    /// Буфер swapchain получен. Строго до выбора видеокадра (`MaterializerLookup`): иначе
    /// ожидание FIFO делает выбранный кадр старше на vsync (сессия 06a).
    SurfaceAcquire,
    MaterializerLookup,
    RendererSubmit,
}

/// Наблюдатель позволяет тесту записать sequence без GPU/window fixture.
pub(super) trait FrameSequenceObserver {
    fn reached(&mut self, stage: FrameSequenceStage);
}

const EXPECTED_FRAME_SEQUENCE: [FrameSequenceStage; 7] = [
    FrameSequenceStage::WorkerEventDrain,
    FrameSequenceStage::WorkerEventRecord,
    FrameSequenceStage::DesktopPublish,
    FrameSequenceStage::EguiOutput,
    FrameSequenceStage::SurfaceAcquire,
    FrameSequenceStage::MaterializerLookup,
    FrameSequenceStage::RendererSubmit,
];

/// Лёгкий production contract проверяет перестановку стадий в debug/test сборках.
///
/// С сессии 04 кадр может пропускать стадии (без egui-прохода нет `EguiOutput`, без
/// показа нет `MaterializerLookup`/`RendererSubmit`), поэтому контракт требует только
/// монотонного порядка достигнутых стадий, а не полного прохода всех.
#[derive(Default)]
pub(super) struct FrameSequenceContract {
    /// Индекс последней достигнутой стадии в `EXPECTED_FRAME_SEQUENCE` (+1).
    next_stage_index: usize,
}

impl FrameSequenceObserver for FrameSequenceContract {
    fn reached(&mut self, stage: FrameSequenceStage) {
        let position = EXPECTED_FRAME_SEQUENCE
            .iter()
            .position(|expected| *expected == stage);
        debug_assert!(
            position.is_some_and(|position| position >= self.next_stage_index),
            "frame stage {stage:?} reached out of order (next expected index {})",
            self.next_stage_index
        );
        if let Some(position) = position {
            self.next_stage_index = position + 1;
        }
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

    /// Пропуск стадий разрешён (кадр без egui-прохода и без показа), порядок — нет.
    #[test]
    fn contract_allows_skipped_stages_but_keeps_order() {
        let mut skipping_ui = FrameSequenceContract::default();
        skipping_ui.reached(FrameSequenceStage::WorkerEventDrain);
        skipping_ui.reached(FrameSequenceStage::WorkerEventRecord);
        skipping_ui.reached(FrameSequenceStage::DesktopPublish);
        skipping_ui.reached(FrameSequenceStage::MaterializerLookup);
        skipping_ui.reached(FrameSequenceStage::RendererSubmit);
        assert_eq!(skipping_ui.next_stage_index, EXPECTED_FRAME_SEQUENCE.len());

        let mut skipping_frame = FrameSequenceContract::default();
        skipping_frame.reached(FrameSequenceStage::WorkerEventDrain);
        skipping_frame.reached(FrameSequenceStage::WorkerEventRecord);
        skipping_frame.reached(FrameSequenceStage::DesktopPublish);
        assert_eq!(skipping_frame.next_stage_index, 3);
    }

    /// Возврат к уже пройденной стадии (egui после submit-а) ловится контрактом.
    #[test]
    #[should_panic(expected = "reached out of order")]
    fn contract_rejects_stage_after_a_later_one() {
        let mut contract = FrameSequenceContract::default();
        contract.reached(FrameSequenceStage::WorkerEventDrain);
        contract.reached(FrameSequenceStage::RendererSubmit);
        contract.reached(FrameSequenceStage::EguiOutput);
    }

    /// Выбор видеокадра до получения буфера swapchain (прежний порядок до сессии 06a)
    /// ловится контрактом.
    #[test]
    #[should_panic(expected = "reached out of order")]
    fn contract_rejects_video_selection_before_surface_acquire() {
        let mut contract = FrameSequenceContract::default();
        contract.reached(FrameSequenceStage::EguiOutput);
        contract.reached(FrameSequenceStage::MaterializerLookup);
        contract.reached(FrameSequenceStage::SurfaceAcquire);
    }

    /// Перестановка стадий (publish до drain-а worker events) ловится контрактом.
    #[test]
    #[should_panic(expected = "reached out of order")]
    fn contract_rejects_reordered_runtime_stage() {
        let mut contract = FrameSequenceContract::default();
        contract.reached(FrameSequenceStage::DesktopPublish);
        contract.reached(FrameSequenceStage::WorkerEventDrain);
    }
}
