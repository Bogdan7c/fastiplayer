//! Ошибка следующего fragment-а должна доходить до consumer после успешного media read.

use super::*;

/// Начальные segments реальны; последующий отказ возникает только на следующем pull.
struct FailingContinuationSource {
    segments: VecDeque<PresentationWindowOrderedSegmentReadOutcome>,
    failure: Option<OrderedSegmentReadError>,
}

impl PresentationWindowOrderedSegmentSource for FailingContinuationSource {
    fn next_segment(
        &mut self,
        _cancellation: &CancellationToken,
    ) -> Result<PresentationWindowOrderedSegmentReadOutcome, OrderedSegmentReadError> {
        if let Some(segment) = self.segments.pop_front() {
            return Ok(segment);
        }
        Err(self
            .failure
            .take()
            .expect("consumer должен остановиться после injected source failure"))
    }
}

#[test]
fn source_failure_after_media_reaches_demux_consumer_without_false_eof() {
    for failure in [
        OrderedSegmentReadError::Failed {
            reason: "continuation fixture failure".to_owned(),
        },
        OrderedSegmentReadError::Cancelled,
    ] {
        let expected_cancelled = matches!(failure, OrderedSegmentReadError::Cancelled);
        let window = PacketPresentationWindow::Unbounded;
        let source = FailingContinuationSource {
            segments: VecDeque::from([ready(initialization(0)), ready(media(1, window))]),
            failure: Some(failure),
        };
        let mut demuxer = PresentationWindowOrderedIsoMp4Demuxer::new(
            Box::new(source),
            CancellationToken::new(),
            sniff_budget(),
            DemuxerOptions::default(),
        )
        .expect("canonical first fragment должен открыться до отказа continuation");

        // Проверяем публичный consumer path: реальные packets, затем typed error.
        // Fixture конечен; EOF/readiness вместо ошибки немедленно проваливает тест.
        let mut packet_count = 0;
        let error = loop {
            match demuxer.next_event() {
                Ok(DemuxReadEvent::Packet(_)) => packet_count += 1,
                Ok(event) => panic!("ожидались media packets или source error, получено {event:?}"),
                Err(error) => break error,
            }
        };
        assert!(
            packet_count > 0,
            "ошибка должна возникнуть после media delivery"
        );
        let typed = error
            .downcast_ref::<PresentationWindowOrderedIsoMp4Error>()
            .expect("consumer должен получить typed ordered-source error");
        if expected_cancelled {
            assert!(matches!(
                typed,
                PresentationWindowOrderedIsoMp4Error::Cancelled
            ));
        } else {
            assert!(matches!(
                typed,
                PresentationWindowOrderedIsoMp4Error::Source(_)
            ));
        }
    }
}
