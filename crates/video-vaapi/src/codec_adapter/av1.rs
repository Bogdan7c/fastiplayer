use cros_codecs::decoder::stateless::av1::Av1;

use super::*;

/// Concrete cros-codecs decoder, который остаётся private деталью AV1 adapter-а.
type VaapiAv1Decoder = StatelessDecoder<Av1, VaapiBackend<InternalVaapiFrame>>;

/// Один AV1 temporal unit, которым adapter владеет до полного consume всех OBU.
struct Av1PendingTemporalUnit {
    /// Полная копия исходного packet-а нужна между retry после backpressure/events.
    encoded_bytes: Vec<u8>,

    /// Timestamp входит в доступную на adapter boundary identity исходного packet-а.
    timestamp_us: u64,

    /// Количество bytes, уже безвозвратно принятых cros-codecs decoder-ом.
    consumed_bytes: usize,
}

impl Av1PendingTemporalUnit {
    /// Создаёт owned temporal unit до отправки первого OBU.
    fn new(encoded_bytes: Vec<u8>, timestamp_us: u64) -> Self {
        Self {
            encoded_bytes,
            timestamp_us,
            consumed_bytes: 0,
        }
    }

    /// Проверяет максимально точную identity, доступную без расширения neutral packet API.
    fn matches_retry(&self, timestamp_us: u64, packet_data: &[u8]) -> bool {
        self.timestamp_us == timestamp_us && self.encoded_bytes == packet_data
    }

    /// Возвращает длину исходного temporal unit для packet ACK после полного consume.
    fn source_packet_len(&self) -> usize {
        self.encoded_bytes.len()
    }

    /// Кормит cros-codecs по одному OBU, пока весь temporal unit не принят или не заблокирован.
    fn feed_until_blocked(
        &mut self,
        mut decode_next_obu: impl FnMut(&[u8]) -> std::result::Result<usize, VaapiAdapterDecodeError>,
    ) -> std::result::Result<usize, VaapiAdapterDecodeError> {
        while self.consumed_bytes < self.encoded_bytes.len() {
            let remaining_bytes = &self.encoded_bytes[self.consumed_bytes..];
            let consumed_now = decode_next_obu(remaining_bytes)?;

            if consumed_now == 0 {
                return Err(VaapiAdapterDecodeError::Decoder(
                    "AV1 decoder accepted an OBU but reported 0 consumed bytes".to_string(),
                ));
            }
            if consumed_now > remaining_bytes.len() {
                return Err(VaapiAdapterDecodeError::Decoder(format!(
                    "AV1 decoder reported {consumed_now} consumed bytes for {} available bytes",
                    remaining_bytes.len()
                )));
            }

            self.consumed_bytes += consumed_now;
        }

        Ok(self.source_packet_len())
    }
}

/// Владеет pending temporal unit и reusable storage независимо от cros decoder state.
#[derive(Default)]
struct Av1TemporalUnitInput {
    /// Незавершённый packet между `CheckEvents`/output-buffer retries.
    pending_temporal_unit: Option<Av1PendingTemporalUnit>,

    /// Освобождённый большой packet buffer переиспользуется следующим temporal unit.
    reusable_encoded_bytes: Vec<u8>,
}

impl Av1TemporalUnitInput {
    /// Начинает новый temporal unit либо доказывает, что caller повторил тот же packet.
    fn prepare_or_validate_retry(
        &mut self,
        timestamp_us: u64,
        packet_data: &[u8],
    ) -> std::result::Result<(), VaapiAdapterDecodeError> {
        if packet_data.is_empty() {
            self.recycle_pending_temporal_unit();
            return Err(VaapiAdapterDecodeError::ParseFrameError(
                "AV1 temporal unit is empty".to_string(),
            ));
        }

        if let Some(pending_temporal_unit) = self.pending_temporal_unit.as_ref() {
            if pending_temporal_unit.matches_retry(timestamp_us, packet_data) {
                return Ok(());
            }

            let pending_timestamp_us = pending_temporal_unit.timestamp_us;
            let pending_packet_len = pending_temporal_unit.source_packet_len();
            self.recycle_pending_temporal_unit();
            return Err(VaapiAdapterDecodeError::Decoder(format!(
                "AV1 retry packet does not match pending temporal unit: pending timestamp {pending_timestamp_us} us / {pending_packet_len} bytes, retry timestamp {timestamp_us} us / {} bytes",
                packet_data.len()
            )));
        }

        let mut encoded_bytes = std::mem::take(&mut self.reusable_encoded_bytes);
        encoded_bytes.clear();
        encoded_bytes
            .try_reserve(packet_data.len())
            .map_err(|error| {
                VaapiAdapterDecodeError::Backend(format!(
                    "failed to reserve {} bytes for AV1 temporal unit: {error}",
                    packet_data.len()
                ))
            })?;
        encoded_bytes.extend_from_slice(packet_data);
        self.pending_temporal_unit = Some(Av1PendingTemporalUnit::new(encoded_bytes, timestamp_us));
        Ok(())
    }

    /// Даёт submit loop-у mutable pending unit без раскрытия storage наружу owner-а.
    fn pending_temporal_unit_mut(
        &mut self,
    ) -> std::result::Result<&mut Av1PendingTemporalUnit, VaapiAdapterDecodeError> {
        self.pending_temporal_unit.as_mut().ok_or_else(|| {
            VaapiAdapterDecodeError::Backend(
                "AV1 pending temporal unit missing after preparation".to_string(),
            )
        })
    }

    /// Завершает полностью принятый packet и возвращает его storage в reusable slot.
    fn complete_pending_temporal_unit(
        &mut self,
    ) -> std::result::Result<usize, VaapiAdapterDecodeError> {
        let pending_temporal_unit = self.pending_temporal_unit.take().ok_or_else(|| {
            VaapiAdapterDecodeError::Backend(
                "AV1 completed temporal unit missing from adapter state".to_string(),
            )
        })?;
        let source_packet_len = pending_temporal_unit.source_packet_len();
        self.recycle_encoded_bytes(pending_temporal_unit.encoded_bytes);
        Ok(source_packet_len)
    }

    /// Сохраняет pending input только для двух явно повторяемых cros состояний.
    fn settle_after_submit_error(&mut self, error: &VaapiAdapterDecodeError) {
        let same_temporal_unit_retry = matches!(
            error,
            VaapiAdapterDecodeError::CheckEvents
                | VaapiAdapterDecodeError::NotEnoughOutputBuffers(_)
        );
        if !same_temporal_unit_retry {
            self.recycle_pending_temporal_unit();
        }
    }

    /// Сбрасывает old-generation input на seek/reconfigure flush boundary.
    fn reset_after_flush(&mut self) {
        self.recycle_pending_temporal_unit();
    }

    /// Запрещает EOF drain, пока cros decoder не принял весь temporal unit.
    fn reject_end_of_stream_drain_if_pending(
        &self,
    ) -> std::result::Result<(), VaapiAdapterDecodeError> {
        if self.pending_temporal_unit.is_some() {
            return Err(VaapiAdapterDecodeError::Decoder(
                "cannot drain AV1 while a temporal unit is partially submitted".to_string(),
            ));
        }

        Ok(())
    }

    /// Возвращает buffer незавершённого packet-а в reusable storage.
    fn recycle_pending_temporal_unit(&mut self) {
        if let Some(pending_temporal_unit) = self.pending_temporal_unit.take() {
            self.recycle_encoded_bytes(pending_temporal_unit.encoded_bytes);
        }
    }

    /// Сохраняет наиболее вместительный освобождённый buffer для следующего packet-а.
    fn recycle_encoded_bytes(&mut self, mut encoded_bytes: Vec<u8>) {
        encoded_bytes.clear();
        if encoded_bytes.capacity() >= self.reusable_encoded_bytes.capacity() {
            self.reusable_encoded_bytes = encoded_bytes;
        }
    }
}

/// Production AV1 Main/Profile 0 adapter поверх cros-codecs VA-API decoder-а.
pub(super) struct Av1VaapiCodecAdapter {
    /// Concrete cros decoder скрыт внутри codec-owned module boundary.
    inner: VaapiAv1Decoder,

    /// Codec-owned packet lifetime и partial-consumption accounting.
    temporal_unit_input: Av1TemporalUnitInput,
}

impl Av1VaapiCodecAdapter {
    /// Создаёт AV1 decoder для уже открытого VA display.
    pub(super) fn new(display: Rc<Display>) -> Result<Self> {
        let inner = VaapiAv1Decoder::new_vaapi(display, BlockingMode::Blocking)
            .map_err(|error| anyhow::anyhow!("Failed to create VA-API AV1 decoder: {error:?}"))?;

        Ok(Self {
            inner,
            temporal_unit_input: Av1TemporalUnitInput::default(),
        })
    }
}

impl VaapiCodecAdapter for Av1VaapiCodecAdapter {
    /// Сообщает codec production adapter-а.
    fn codec(&self) -> VideoCodec {
        VideoCodec::Av1
    }

    /// Возвращает backend label для AV1 diagnostics.
    fn backend_name(&self) -> &'static str {
        "VA-API AV1"
    }

    /// Возвращает codec label для сообщений retry-loop-а.
    fn codec_label(&self) -> &'static str {
        "AV1"
    }

    /// Отправляет все OBU temporal unit-а, не ACK-ая packet после первого OBU.
    fn submit_packet(
        &mut self,
        timestamp_us: u64,
        packet_data: &[u8],
        _decode_hints: VaapiPacketDecodeHints,
        frame_pool: &mut DmaFramePool,
    ) -> std::result::Result<usize, VaapiAdapterDecodeError> {
        if let Err(error) = self
            .temporal_unit_input
            .prepare_or_validate_retry(timestamp_us, packet_data)
        {
            self.temporal_unit_input.settle_after_submit_error(&error);
            return Err(error);
        }

        let inner = &mut self.inner;
        let pending_temporal_unit = self.temporal_unit_input.pending_temporal_unit_mut()?;
        let feed_result = pending_temporal_unit.feed_until_blocked(|remaining_bytes| {
            let mut alloc_cb = || {
                let frame = frame_pool.alloc_or_allocate();
                if frame.is_none() {
                    tracing::warn!("Frame pool exhausted; AV1 decoder needs output buffers");
                }
                frame
            };

            inner
                .decode(timestamp_us, remaining_bytes, &mut alloc_cb)
                .map_err(VaapiAdapterDecodeError::from)
        });

        match feed_result {
            Ok(source_packet_len) => {
                let completed_packet_len =
                    self.temporal_unit_input.complete_pending_temporal_unit()?;
                if completed_packet_len != source_packet_len {
                    return Err(VaapiAdapterDecodeError::Backend(format!(
                        "AV1 completed packet length changed from {source_packet_len} to {completed_packet_len}"
                    )));
                }
                Ok(completed_packet_len)
            }
            Err(error) => {
                self.temporal_unit_input.settle_after_submit_error(&error);
                Err(error)
            }
        }
    }

    /// Flush-ит cros decoder и сначала освобождает partially consumed old-generation input.
    fn flush(&mut self) -> std::result::Result<(), VaapiAdapterDecodeError> {
        self.temporal_unit_input.reset_after_flush();
        self.inner.flush().map_err(VaapiAdapterDecodeError::from)
    }

    /// AV1 публикует frames во время decode; отдельного DPB tail у этого path-а нет.
    fn begin_end_of_stream_drain(&mut self) -> std::result::Result<(), VaapiAdapterDecodeError> {
        self.temporal_unit_input
            .reject_end_of_stream_drain_if_pending()
    }

    /// Возвращает следующий cros event в локальном wrapper-е.
    fn next_event(&mut self) -> Option<VaapiDecoderEvent> {
        self.inner.next_event().map(VaapiDecoderEvent::from)
    }

    /// Возвращает stream info без раскрытия AV1 parser/backend типов.
    fn stream_info(&self) -> Option<VaapiAdapterStreamInfo> {
        self.inner.stream_info().map(VaapiAdapterStreamInfo::from)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Создаёт типичную owned input state для hermetic lifecycle tests.
    fn prepared_input(timestamp_us: u64, packet_data: &[u8]) -> Av1TemporalUnitInput {
        let mut input = Av1TemporalUnitInput::default();
        input
            .prepare_or_validate_retry(timestamp_us, packet_data)
            .expect("test temporal unit must be accepted");
        input
    }

    #[test]
    fn temporal_unit_consumes_every_obu_before_packet_ack() {
        let packet_data = [1_u8, 2, 3, 4, 5, 6];
        let mut input = prepared_input(41, &packet_data);
        let mut consumed_obu_lengths = [2_usize, 3, 1].into_iter();
        let mut submitted_suffixes = Vec::new();

        let accepted_len = input
            .pending_temporal_unit_mut()
            .expect("pending temporal unit must exist")
            .feed_until_blocked(|remaining_bytes| {
                submitted_suffixes.push(remaining_bytes.to_vec());
                Ok(consumed_obu_lengths
                    .next()
                    .expect("one fake result per OBU"))
            })
            .expect("all fake OBU submissions must succeed");

        assert_eq!(accepted_len, packet_data.len());
        assert_eq!(
            submitted_suffixes,
            vec![vec![1, 2, 3, 4, 5, 6], vec![3, 4, 5, 6], vec![6]]
        );
        assert_eq!(
            input
                .complete_pending_temporal_unit()
                .expect("completed packet must be acknowledged"),
            packet_data.len()
        );
        assert!(input.pending_temporal_unit.is_none());
    }

    #[test]
    fn check_events_retry_resumes_at_first_unconsumed_obu() {
        let packet_data = [10_u8, 11, 12, 13, 14];
        let mut input = prepared_input(51, &packet_data);
        let mut first_call = true;

        let error = input
            .pending_temporal_unit_mut()
            .expect("pending temporal unit must exist")
            .feed_until_blocked(|_remaining_bytes| {
                if first_call {
                    first_call = false;
                    Ok(2)
                } else {
                    Err(VaapiAdapterDecodeError::CheckEvents)
                }
            })
            .expect_err("format event must interrupt temporal unit");
        input.settle_after_submit_error(&error);

        input
            .prepare_or_validate_retry(51, &packet_data)
            .expect("same packet retry must be accepted");
        let mut retried_suffix = None;
        let accepted_len = input
            .pending_temporal_unit_mut()
            .expect("pending temporal unit must survive CheckEvents")
            .feed_until_blocked(|remaining_bytes| {
                retried_suffix = Some(remaining_bytes.to_vec());
                Ok(remaining_bytes.len())
            })
            .expect("retry must complete remaining OBU bytes");

        assert_eq!(retried_suffix, Some(vec![12, 13, 14]));
        assert_eq!(accepted_len, packet_data.len());
    }

    #[test]
    fn output_backpressure_preserves_exact_consumed_offset() {
        let packet_data = [20_u8, 21, 22, 23, 24, 25, 26];
        let mut input = prepared_input(61, &packet_data);
        let mut first_call = true;

        let error = input
            .pending_temporal_unit_mut()
            .expect("pending temporal unit must exist")
            .feed_until_blocked(|_remaining_bytes| {
                if first_call {
                    first_call = false;
                    Ok(3)
                } else {
                    Err(VaapiAdapterDecodeError::NotEnoughOutputBuffers(2))
                }
            })
            .expect_err("surface pressure must interrupt temporal unit");
        input.settle_after_submit_error(&error);

        let pending = input
            .pending_temporal_unit
            .as_ref()
            .expect("backpressure must preserve pending temporal unit");
        assert_eq!(pending.consumed_bytes, 3);
        input
            .prepare_or_validate_retry(61, &packet_data)
            .expect("same packet must resume after surface release");
    }

    #[test]
    fn different_retry_packet_is_typed_invariant_error_and_discards_old_input() {
        let packet_data = [30_u8, 31, 32, 33];
        let mut input = prepared_input(71, &packet_data);

        let error = input
            .prepare_or_validate_retry(72, &packet_data)
            .expect_err("different timestamp must not continue pending bytes");
        assert!(matches!(error, VaapiAdapterDecodeError::Decoder(_)));
        assert!(input.pending_temporal_unit.is_none());

        input
            .prepare_or_validate_retry(73, &packet_data)
            .expect("fresh packet must start after terminal mismatch cleanup");
        let error = input
            .prepare_or_validate_retry(73, &[30, 31, 32, 34])
            .expect_err("different bytes must not continue pending temporal unit");
        assert!(matches!(error, VaapiAdapterDecodeError::Decoder(_)));
        assert!(input.pending_temporal_unit.is_none());
    }

    #[test]
    fn invalid_consumed_counts_are_terminal_and_recycle_input() {
        for reported_consumed in [0_usize, 5] {
            let packet_data = [40_u8, 41, 42, 43];
            let mut input = prepared_input(81, &packet_data);
            let error = input
                .pending_temporal_unit_mut()
                .expect("pending temporal unit must exist")
                .feed_until_blocked(|_remaining_bytes| Ok(reported_consumed))
                .expect_err("invalid cros consumed count must fail closed");

            assert!(matches!(error, VaapiAdapterDecodeError::Decoder(_)));
            input.settle_after_submit_error(&error);
            assert!(input.pending_temporal_unit.is_none());
            assert!(input.reusable_encoded_bytes.capacity() >= packet_data.len());
        }
    }

    #[test]
    fn parse_and_backend_errors_discard_failed_temporal_unit() {
        for error in [
            VaapiAdapterDecodeError::ParseFrameError("bad OBU".to_string()),
            VaapiAdapterDecodeError::Backend("VA failure".to_string()),
        ] {
            let packet_data = [50_u8, 51, 52];
            let mut input = prepared_input(91, &packet_data);

            input.settle_after_submit_error(&error);

            assert!(input.pending_temporal_unit.is_none());
            assert!(input.reusable_encoded_bytes.capacity() >= packet_data.len());
        }
    }

    #[test]
    fn flush_discards_partial_unit_and_reuses_storage_for_next_packet() {
        let first_packet = [60_u8; 64];
        let mut input = prepared_input(101, &first_packet);
        let original_capacity = input
            .pending_temporal_unit
            .as_ref()
            .expect("pending temporal unit must exist")
            .encoded_bytes
            .capacity();

        input.reset_after_flush();

        assert!(input.pending_temporal_unit.is_none());
        assert!(input.reusable_encoded_bytes.capacity() >= original_capacity);
        input
            .prepare_or_validate_retry(102, &[61, 62, 63])
            .expect("new-generation packet must start after flush");
        assert_eq!(
            input
                .pending_temporal_unit
                .as_ref()
                .expect("new pending temporal unit must exist")
                .encoded_bytes,
            [61, 62, 63]
        );
    }

    #[test]
    fn eof_drain_is_noop_only_without_partial_temporal_unit() {
        let mut input = prepared_input(111, &[70, 71, 72]);

        let error = input
            .reject_end_of_stream_drain_if_pending()
            .expect_err("partial temporal unit must block EOF drain");
        assert!(matches!(error, VaapiAdapterDecodeError::Decoder(_)));
        assert!(input.pending_temporal_unit.is_some());

        input.reset_after_flush();
        input
            .reject_end_of_stream_drain_if_pending()
            .expect("AV1 has no separate DPB tail after complete input");
    }

    #[test]
    fn empty_temporal_unit_is_rejected_without_pending_state() {
        let mut input = Av1TemporalUnitInput::default();

        let error = input
            .prepare_or_validate_retry(121, &[])
            .expect_err("empty temporal unit must fail before cros parser");

        assert!(matches!(error, VaapiAdapterDecodeError::ParseFrameError(_)));
        assert!(input.pending_temporal_unit.is_none());
    }
}
