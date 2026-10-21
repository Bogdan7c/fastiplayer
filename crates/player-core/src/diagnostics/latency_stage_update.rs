//! Обновление снимка задержек одной стадии на месте (cpu-parity сессия 05).
//!
//! Замер задержки приходит много раз за кадр, а снимок диагностики собирается на каждом
//! тике worker-а. Пересчёт всех 13 стадий (13 делений `u128` на среднее) на любом из этих
//! путей стоил ~2 % потока worker-а. Замер меняет ровно одну стадию, поэтому и снимок
//! обновляется ровно в одной стадии: хранимый снимок всегда актуален, чтение ничего не считает.

use super::{
    LatencyCounterSnapshot, PipelineLatencyCounters, PipelineLatencyCountersSnapshot,
    PipelineLatencyStage,
};

impl PipelineLatencyCounters {
    /// Снимок счётчика одной стадии: одно деление вместо полного `snapshot()`.
    pub(super) fn stage_snapshot(&mut self, stage: PipelineLatencyStage) -> LatencyCounterSnapshot {
        self.counter_mut(stage).snapshot()
    }
}

impl PipelineLatencyCountersSnapshot {
    /// Ячейка снимка для стадии; список стадий совпадает с `PipelineLatencyCounters::counter_mut`.
    pub(super) fn stage_mut(&mut self, stage: PipelineLatencyStage) -> &mut LatencyCounterSnapshot {
        match stage {
            PipelineLatencyStage::DemuxRead => &mut self.demux_read,
            PipelineLatencyStage::DecoderPacketReceive => &mut self.decoder_packet_receive,
            PipelineLatencyStage::DecoderSubmit => &mut self.decoder_submit,
            PipelineLatencyStage::DecoderEventDrain => &mut self.decoder_event_drain,
            PipelineLatencyStage::HardwareSync => &mut self.hardware_sync,
            PipelineLatencyStage::DmaBufExport => &mut self.dma_buf_export,
            PipelineLatencyStage::DmaBufImport => &mut self.dma_buf_import,
            PipelineLatencyStage::DecodedFramePublish => &mut self.decoded_frame_publish,
            PipelineLatencyStage::WorkerScheduler => &mut self.worker_scheduler,
            PipelineLatencyStage::RenderAcquire => &mut self.render_acquire,
            PipelineLatencyStage::RenderResourceLockWait => &mut self.render_resource_lock_wait,
            PipelineLatencyStage::GpuSubmitPresent => &mut self.gpu_submit_present,
            PipelineLatencyStage::ReleaseAcknowledgement => &mut self.release_acknowledgement,
        }
    }
}
