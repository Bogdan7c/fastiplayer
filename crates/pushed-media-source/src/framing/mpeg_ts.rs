//! Сканер MPEG-TS: целые 188-байтовые пакеты.
//!
//! У TS нет init-сегмента: таблицы PAT/PMT идут внутри потока, а разбор
//! разрывов и смены программ уже умеет `mpeg-ts-demux`. Сканеру остаётся
//! отдавать только целые пакеты и проверять синхробайт: тогда `abort()`
//! выбрасывает недособранный пакет, а демуксер не видит обрывков.

use bytes::{Buf, BytesMut};

use super::{FramedUnit, FramingError, FramingLimits, SegmentKind};

/// Размер пакета транспортного потока (ISO/IEC 13818-1).
const TS_PACKET_BYTES: usize = 188;
/// Синхробайт в начале каждого пакета.
const TS_SYNC_BYTE: u8 = 0x47;

/// Сканер одной полосы MPEG-TS.
#[derive(Debug)]
pub(crate) struct MpegTsFramer {
    /// Пределы размера сегментов.
    limits: FramingLimits,
    /// Хвост, не дотянувший до целого пакета.
    partial_packet: BytesMut,
}

impl MpegTsFramer {
    /// Пустой сканер.
    pub(crate) fn new(limits: FramingLimits) -> Self {
        Self {
            limits,
            partial_packet: BytesMut::new(),
        }
    }

    /// Целые пакеты куска становятся медиасегментами не больше предела.
    ///
    /// У TS нет настоящих границ сегментов, поэтому большой кусок — не ошибка, а
    /// несколько сегментов по целому числу пакетов.
    pub(crate) fn push(
        &mut self,
        chunk: &[u8],
        completed: &mut Vec<FramedUnit>,
    ) -> Result<(), FramingError> {
        let packets_per_unit = self.limits.max_media_bytes / TS_PACKET_BYTES;
        if packets_per_unit == 0 {
            // Предел меньше одного пакета: ни один сегмент в него не поместится.
            return Err(FramingError::SegmentTooLarge {
                kind: SegmentKind::Media,
                declared: TS_PACKET_BYTES as u64,
                limit: self.limits.max_media_bytes,
            });
        }
        self.partial_packet.extend_from_slice(chunk);
        while self.partial_packet.len() >= TS_PACKET_BYTES {
            let packet_count = (self.partial_packet.len() / TS_PACKET_BYTES).min(packets_per_unit);
            self.release_packets(packet_count * TS_PACKET_BYTES, completed)?;
        }
        Ok(())
    }

    /// Выдаёт первые `unit_len` байтов (целые пакеты) одним сегментом.
    fn release_packets(
        &mut self,
        unit_len: usize,
        completed: &mut Vec<FramedUnit>,
    ) -> Result<(), FramingError> {
        let packets = self.partial_packet.split_to(unit_len);
        let (whole_packets, _no_remainder) = packets.as_chunks::<TS_PACKET_BYTES>();
        let synchronized = whole_packets.iter().all(|packet| packet[0] == TS_SYNC_BYTE);
        if !synchronized {
            return Err(FramingError::TransportStreamSyncLost);
        }
        completed.push(FramedUnit::Media(packets.freeze()));
        Ok(())
    }

    /// `abort()`: недособранный пакет выбрасывается.
    pub(crate) fn reset(&mut self) {
        self.partial_packet.clear();
    }

    /// Конец потока: недособранный пакет выбрасывается, возвращается его объём.
    pub(crate) fn finish(&mut self) -> usize {
        let dropped = self.partial_packet.len();
        self.partial_packet.advance(dropped);
        dropped
    }

    /// Сколько байтов удерживается до целого пакета.
    pub(crate) fn held_bytes(&self) -> usize {
        self.partial_packet.len()
    }
}

#[cfg(test)]
mod tests {
    use super::{MpegTsFramer, TS_PACKET_BYTES, TS_SYNC_BYTE};
    use crate::framing::{FramedUnit, FramingError, FramingLimits};

    const LIMITS: FramingLimits = FramingLimits {
        max_initialization_bytes: 1024,
        max_media_bytes: 4 * TS_PACKET_BYTES,
    };

    fn packets(count: usize) -> Vec<u8> {
        let mut bytes = vec![0_u8; count * TS_PACKET_BYTES];
        let (whole_packets, _no_remainder) = bytes.as_chunks_mut::<TS_PACKET_BYTES>();
        for packet in whole_packets {
            packet[0] = TS_SYNC_BYTE;
        }
        bytes
    }

    #[test]
    fn whole_packets_are_released_and_tail_waits() {
        let stream = packets(3);
        let mut framer = MpegTsFramer::new(LIMITS);
        let mut completed = Vec::new();
        framer.push(&stream[..250], &mut completed).expect("valid");
        assert_eq!(completed.len(), 1);
        assert_eq!(framer.held_bytes(), 250 - TS_PACKET_BYTES);
        framer.push(&stream[250..], &mut completed).expect("valid");
        let released: usize = completed
            .iter()
            .map(|unit| match unit {
                FramedUnit::Media(bytes) => bytes.len(),
                FramedUnit::Initialization(_) => 0,
            })
            .sum();
        assert_eq!(released, stream.len());
        assert_eq!(framer.held_bytes(), 0);
    }

    #[test]
    fn lost_sync_byte_is_an_error_and_abort_drops_partial_packet() {
        let mut framer = MpegTsFramer::new(LIMITS);
        let mut completed = Vec::new();
        framer
            .push(&packets(1)[..100], &mut completed)
            .expect("partial");
        framer.reset();
        assert_eq!(framer.held_bytes(), 0);
        let mut broken = packets(1);
        broken[0] = 0x00;
        assert_eq!(
            framer.push(&broken, &mut completed),
            Err(FramingError::TransportStreamSyncLost)
        );
        assert!(completed.is_empty());
    }

    #[test]
    fn large_append_is_split_into_units_within_the_limit() {
        let mut framer = MpegTsFramer::new(LIMITS);
        let mut completed = Vec::new();
        framer
            .push(&packets(9), &mut completed)
            .expect("split, not refused");
        let sizes: Vec<usize> = completed
            .iter()
            .map(|unit| match unit {
                FramedUnit::Media(bytes) => bytes.len(),
                FramedUnit::Initialization(_) => 0,
            })
            .collect();
        assert_eq!(
            sizes,
            [4 * TS_PACKET_BYTES, 4 * TS_PACKET_BYTES, TS_PACKET_BYTES]
        );
        let tiny = FramingLimits {
            max_initialization_bytes: 1024,
            max_media_bytes: 100,
        };
        assert!(
            MpegTsFramer::new(tiny)
                .push(&packets(1), &mut completed)
                .is_err()
        );
    }

    #[test]
    fn finish_reports_dropped_tail() {
        let mut completed = Vec::new();
        let mut framer = MpegTsFramer::new(LIMITS);
        framer
            .push(&packets(1)[..42], &mut completed)
            .expect("partial");
        assert_eq!(framer.finish(), 42);
        assert_eq!(framer.held_bytes(), 0);
    }
}
