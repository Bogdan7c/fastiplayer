//! Приём ресурса с учётом packed audio: sniff первых байт и нормализация в audio-only TS.
//!
//! Весь packed-сегмент читается в память целиком (аудио-сегмент мал, размер уже ограничен
//! политикой транспорта), поэтому после нормализации он отдаётся demux-у через уже существующее
//! состояние `Buffered`. Обычные TS-ресурсы по-прежнему идут потоком без буферизации.

use bytes::Bytes;
use web_media_adaptive::{AdaptiveStreamingResource, AdaptiveTransportError};

use super::streaming::HlsResourceStreamState;
use super::{HlsEpochSegmentSource, HlsSegmentSourceError};
use crate::packed_audio::{
    PACKED_AUDIO_SNIFF_PREFIX_BYTES, SegmentTimestampFallback, looks_like_packed_audio,
    normalize_to_transport_stream,
};
use crate::plan::PlannedResource;

impl HlsEpochSegmentSource {
    /// Превращает packed audio в audio-only TS; любые другие байты возвращает как есть.
    ///
    /// Проверка идёт по содержимому, а не по расширению URI. Init-ресурсы (`EXT-X-MAP`) никогда
    /// не являются packed audio и не трогаются.
    pub(super) fn normalize_packed_audio(
        resource: &PlannedResource,
        plaintext: Bytes,
    ) -> Result<Bytes, HlsSegmentSourceError> {
        if resource.kind != demux_api::OrderedSegmentKind::Media
            || !looks_like_packed_audio(&plaintext)
        {
            return Ok(plaintext);
        }
        // Нет ID3 PRIV - PTS берём из позиции сегмента на таймлайне манифеста.
        let fallback = resource
            .manifest_segment
            .map_or(SegmentTimestampFallback::Zero, |segment| {
                SegmentTimestampFallback::ManifestTimelineStart(segment.timeline_start)
            });
        Ok(normalize_to_transport_stream(&plaintext, fallback)?)
    }

    /// Открывает не зашифрованный ресурс и выбирает режим чтения по первым байтам:
    /// packed audio буферизуется и нормализуется, остальное остаётся потоковым.
    pub(super) fn open_sniffed_resource(
        &self,
        resource: PlannedResource,
        resource_start: u64,
    ) -> Result<HlsResourceStreamState, HlsSegmentSourceError> {
        let mut body = self.open_streaming_resource(&resource)?;
        let (head, ended) = self.read_sniff_head(&mut body)?;
        if !ended && !looks_like_packed_audio(&head) {
            return Ok(HlsResourceStreamState::Streaming {
                resource,
                resource_start,
                body,
                remainder: head,
            });
        }
        // Либо тело закончилось внутри префикса (читать больше нечего), либо это packed audio.
        let mut all_bytes = head.to_vec();
        if !ended {
            self.drain_remaining_chunks(&mut body, &mut all_bytes)?;
        }
        let plaintext = Self::normalize_packed_audio(&resource, Bytes::from(all_bytes))?;
        Ok(HlsResourceStreamState::Buffered {
            resource,
            resource_start,
            remainder: plaintext,
        })
    }

    /// Дочитывает минимальный префикс для sniff. Второй элемент - тело уже закончилось.
    fn read_sniff_head(
        &self,
        body: &mut AdaptiveStreamingResource,
    ) -> Result<(Bytes, bool), HlsSegmentSourceError> {
        let mut head = Bytes::new();
        while head.len() < PACKED_AUDIO_SNIFF_PREFIX_BYTES {
            match self.next_observed_chunk(body)? {
                Some(chunk) if head.is_empty() => head = chunk,
                Some(chunk) => {
                    let mut joined = head.to_vec();
                    joined.extend_from_slice(&chunk);
                    head = Bytes::from(joined);
                }
                None => return Ok((head, true)),
            }
        }
        Ok((head, false))
    }

    fn drain_remaining_chunks(
        &self,
        body: &mut AdaptiveStreamingResource,
        into: &mut Vec<u8>,
    ) -> Result<(), HlsSegmentSourceError> {
        while let Some(chunk) = self.next_observed_chunk(body)? {
            into.extend_from_slice(&chunk);
        }
        Ok(())
    }

    /// Читает chunk и сообщает transient body-ошибки manifest-attempt observer-у.
    fn next_observed_chunk(
        &self,
        body: &mut AdaptiveStreamingResource,
    ) -> Result<Option<Bytes>, HlsSegmentSourceError> {
        body.next_chunk().map_err(|error: AdaptiveTransportError| {
            self.resource_attempt_observer
                .observe_transport_error(&error);
            HlsSegmentSourceError::from(error)
        })
    }
}
