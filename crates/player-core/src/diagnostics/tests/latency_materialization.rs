//! Снимок задержек обновляется по одной стадии на замер, а не пересчётом всех стадий
//! (cpu-parity сессия 05): результат обязан совпадать с полным пересчётом после каждого замера.

use super::*;

#[test]
fn per_stage_latency_update_matches_full_recount_after_every_sample() {
    let mut diagnostics = PlaybackDiagnostics::new();
    let queues = queue_depths_for_tests(2);
    let mut expected = PipelineLatencyCounters::default();

    // Чередуем стадии и длительности (включая повтор худшего значения): после каждого
    // замера и хранимый снимок, и снимок для UI равны эталонному полному пересчёту.
    for (step, (stage, micros)) in [
        (PipelineLatencyStage::DecoderSubmit, 300_u64),
        (PipelineLatencyStage::RenderAcquire, 50),
        (PipelineLatencyStage::DecoderSubmit, 100),
        (PipelineLatencyStage::DecoderSubmit, 900),
        (PipelineLatencyStage::GpuSubmitPresent, 10),
        (PipelineLatencyStage::ReleaseAcknowledgement, 70),
        (PipelineLatencyStage::DemuxRead, 70),
        (PipelineLatencyStage::DecoderSubmit, 900),
    ]
    .into_iter()
    .enumerate()
    {
        let duration = Duration::from_micros(micros);
        let pts = Some(Duration::from_millis(step as u64));
        diagnostics.record_latency(stage, duration, pts, None, queues);
        expected.record(PipelineLatencySampleSnapshot {
            stage,
            duration,
            pts,
            memory_path: None,
            queues,
        });

        assert_eq!(diagnostics.snapshot.worst_latencies, expected.snapshot());
        let published = diagnostics.snapshot_with_queues(queues);
        assert_eq!(published.worst_latencies, expected.snapshot());
        assert_eq!(published.queues, queues);
    }

    let snapshot = diagnostics.snapshot_with_queues(queues);
    assert_eq!(snapshot.worst_latencies.decoder_submit.samples, 4);
    assert_eq!(
        snapshot.worst_latencies.decoder_submit.average,
        Duration::from_micros(550)
    );
    // Лог-сводка считает счётчики напрямую и совпадает со снимком.
    assert_eq!(
        diagnostics.log_summary(queues).worst_latencies,
        snapshot.worst_latencies
    );
}

#[test]
fn untouched_stages_keep_default_snapshot() {
    let mut diagnostics = PlaybackDiagnostics::new();
    let queues = queue_depths_for_tests(1);

    diagnostics.record_latency(
        PipelineLatencyStage::HardwareSync,
        Duration::from_micros(42),
        None,
        None,
        queues,
    );

    let snapshot = diagnostics.snapshot_with_queues(queues);
    assert_eq!(snapshot.worst_latencies.hardware_sync.samples, 1);
    // Остальные стадии не тронуты замером и равны пустому счётчику.
    let only_hardware_sync = PipelineLatencyCountersSnapshot {
        hardware_sync: snapshot.worst_latencies.hardware_sync,
        ..PipelineLatencyCountersSnapshot::default()
    };
    assert_eq!(snapshot.worst_latencies, only_hardware_sync);
}
