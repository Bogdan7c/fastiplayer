//! Время показа кадров для кодеков, где кадр показывает конкретный temporal unit.
//!
//! В AV1 (temporal unit) и VP9 (packet/superframe) показанный кадр принадлежит
//! тому unit-у, который его показал. При `show_existing_frame` cros-codecs
//! публикует ранее декодированный reference handle, а у handle остаётся
//! timestamp unit-а, где кадр *декодировали*. Без переназначения плеер видит
//! PTS, прыгающие назад (0, 20, 20, 60, 20 ms…), и дропает кадры как поздние.
//!
//! Владелец правила — adapter: только он знает, какой unit сейчас отправлен.
//! Инвариант decode loop-а: события одного packet-а дренируются до отправки
//! следующего (`run_decode_with_event_retry`), а AV1/VP9 stateless путь не
//! держит отложенный DPB-хвост. Поэтому каждый `FrameReady` между двумя
//! `submit_packet` показан именно текущим unit-ом. H.264/H.265 этим типом
//! не пользуются: там DPB законно выпускает кадры позже с их собственным PTS.

#[cfg(test)]
use super::VaapiDecodedFrameHandle;
use super::VaapiDecoderEvent;

/// Помнит timestamp текущего temporal unit-а и проставляет его показанным кадрам.
#[derive(Debug, Default)]
pub(super) struct TemporalUnitPresentationTime {
    /// Timestamp (мкс) unit-а, события которого сейчас дренируются.
    ///
    /// `None` до первого packet-а и после seek flush: тогда кадр сохраняет
    /// timestamp, который сообщил cros-codecs.
    current_unit_timestamp_us: Option<u64>,
}

impl TemporalUnitPresentationTime {
    /// Отмечает начало отправки нового unit-а; retry того же unit-а безопасен.
    pub(super) fn begin_unit(&mut self, timestamp_us: u64) {
        self.current_unit_timestamp_us = Some(timestamp_us);
    }

    /// Seek flush завершает старый unit: его время не должно попасть на новые кадры.
    pub(super) fn reset_after_flush(&mut self) {
        self.current_unit_timestamp_us = None;
    }

    /// Проставляет кадру время показа текущего unit-а.
    ///
    /// Меняется только время показа; surface, backing frame и прочие события
    /// (например `FormatChanged`) проходят без изменений.
    pub(super) fn stamp(&self, event: VaapiDecoderEvent) -> VaapiDecoderEvent {
        match (event, self.current_unit_timestamp_us) {
            (VaapiDecoderEvent::FrameReady(handle), Some(unit_timestamp_us)) => {
                VaapiDecoderEvent::FrameReady(
                    handle.with_presentation_timestamp_us(unit_timestamp_us),
                )
            }
            (event, _) => event,
        }
    }
}

/// Только для тестов: достаёт handle из события или падает с понятной причиной.
#[cfg(test)]
fn expect_frame_ready(event: VaapiDecoderEvent) -> VaapiDecodedFrameHandle {
    match event {
        VaapiDecoderEvent::FrameReady(handle) => handle,
        VaapiDecoderEvent::FormatChanged => panic!("ожидался FrameReady, получен FormatChanged"),
    }
}

#[cfg(test)]
mod tests {
    use std::sync::Arc;

    use super::*;
    use crate::codec_adapter::test_support::{FakeSurfaceReadiness, fake_decoded_frame_handle};

    /// Fake cros handle всегда сообщает этот timestamp (см. `test_support`).
    const DECODER_TIMESTAMP_US: u64 = 123;

    /// Новый fake `FrameReady` с decoder timestamp-ом `DECODER_TIMESTAMP_US`.
    fn fake_frame_ready() -> VaapiDecoderEvent {
        let (handle, _sync_called) = fake_decoded_frame_handle(FakeSurfaceReadiness::Ready(true));
        VaapiDecoderEvent::FrameReady(handle)
    }

    #[test]
    fn frame_of_active_unit_gets_unit_timestamp_instead_of_decode_timestamp() {
        let mut presentation_time = TemporalUnitPresentationTime::default();
        presentation_time.begin_unit(40_000);

        let handle = expect_frame_ready(presentation_time.stamp(fake_frame_ready()));

        assert_eq!(handle.timestamp(), 40_000);
    }

    #[test]
    fn frame_without_active_unit_keeps_decoder_timestamp() {
        let presentation_time = TemporalUnitPresentationTime::default();

        let handle = expect_frame_ready(presentation_time.stamp(fake_frame_ready()));

        assert_eq!(handle.timestamp(), DECODER_TIMESTAMP_US);
    }

    #[test]
    fn flush_stops_stamping_old_unit_time() {
        let mut presentation_time = TemporalUnitPresentationTime::default();
        presentation_time.begin_unit(40_000);
        presentation_time.reset_after_flush();

        let handle = expect_frame_ready(presentation_time.stamp(fake_frame_ready()));

        assert_eq!(handle.timestamp(), DECODER_TIMESTAMP_US);
    }

    #[test]
    fn next_unit_replaces_previous_unit_time() {
        let mut presentation_time = TemporalUnitPresentationTime::default();
        presentation_time.begin_unit(20_000);
        presentation_time.begin_unit(60_000);

        let handle = expect_frame_ready(presentation_time.stamp(fake_frame_ready()));

        assert_eq!(handle.timestamp(), 60_000);
    }

    #[test]
    fn format_change_passes_through_and_does_not_consume_unit_time() {
        let mut presentation_time = TemporalUnitPresentationTime::default();
        presentation_time.begin_unit(40_000);

        let format_event = presentation_time.stamp(VaapiDecoderEvent::FormatChanged);
        let handle = expect_frame_ready(presentation_time.stamp(fake_frame_ready()));

        assert!(matches!(format_event, VaapiDecoderEvent::FormatChanged));
        assert_eq!(handle.timestamp(), 40_000);
    }

    #[test]
    fn stamping_keeps_backing_frame_and_surface_readiness() {
        let mut presentation_time = TemporalUnitPresentationTime::default();
        presentation_time.begin_unit(40_000);
        let (handle, sync_called) = fake_decoded_frame_handle(FakeSurfaceReadiness::Ready(false));
        let backing_frame_before = handle.video_frame();

        let stamped =
            expect_frame_ready(presentation_time.stamp(VaapiDecoderEvent::FrameReady(handle)));

        assert!(Arc::ptr_eq(&backing_frame_before, &stamped.video_frame()));
        assert!(!stamped.surface_ready().unwrap());
        assert!(
            !sync_called.get(),
            "stamp не должен синхронизировать surface"
        );
    }
}
