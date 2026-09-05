//! Отмена внутри чтения имеет приоритет над одновременно возвращаемой ошибкой.

use super::*;

struct CancellingResourceSource {
    inner: PullResourceSource,
}

impl OrderedResourceStreamSource for CancellingResourceSource {
    fn next_event(
        &mut self,
        maximum_chunk_bytes: NonZeroUsize,
        cancellation: &CancellationToken,
    ) -> Result<OrderedResourceReadOutcome, OrderedResourceReadError> {
        let outcome = self.inner.next_event(maximum_chunk_bytes, cancellation);
        if matches!(
            outcome,
            Err(OrderedResourceReadError::RestartableReadInterrupted)
        ) {
            // Отмена возникает после preflight demux-а, но до обработки ошибки источника.
            cancellation.cancel();
        }
        outcome
    }
}

#[test]
fn cancellation_during_resource_read_overrides_restartable_error_after_audio_delivery() {
    let body = TsFixtureBuilder::new()
        .pat(&[(1, PMT_PID)], 0)
        .pmt(PMT_PID, 1, &[(0x0f, AUDIO_PID)], 0)
        .pes(AUDIO_PID, 0, None, &adts_frame(&[0x31; 96]))
        .pes(AUDIO_PID, 1_920, None, &adts_frame(&[0x32; 96]))
        .null_packets(6_000)
        .finish();
    let source = CancellingResourceSource {
        inner: PullResourceSource {
            stage: PullResourceStage::Begin,
            body: Bytes::from(body),
            maximum_fragment_bytes: usize::MAX,
            delivered_bytes: Arc::new(AtomicUsize::new(0)),
            end_resource_pulled: Arc::new(AtomicBool::new(false)),
            requested_bounds: Arc::new(std::sync::Mutex::new(Vec::new())),
            interruption: PullResourceInterruption::AfterDeliveredBytes(32 * 1024),
        },
    };
    let cancellation = CancellationToken::new();
    let mut demuxer = MpegTsDemuxer::open(
        DemuxInput::ordered_resource_stream(Box::new(source)),
        cancellation.clone(),
        MpegTsDemuxOptions::default(),
    )
    .expect("open before runtime cancellation");
    let mut packets = 0;
    for _ in 0..32 {
        match demuxer.next_event() {
            Ok(DemuxReadEvent::Packet(_)) => packets += 1,
            Ok(DemuxReadEvent::EndOfStream) => panic!("cancellation must not become EOF"),
            Ok(_) => {}
            Err(error) => {
                assert!(matches!(
                    error.downcast_ref::<MpegTsDemuxError>(),
                    Some(MpegTsDemuxError::Cancelled)
                ));
                assert!(cancellation.is_cancelled());
                assert!(
                    packets > 0,
                    "real audio packets must reach the consumer first"
                );
                return;
            }
        }
    }
    panic!("runtime cancellation must reach the consumer");
}
