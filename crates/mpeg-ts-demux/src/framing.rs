use std::collections::VecDeque;

use demux_api::{
    DemuxByteSource, DemuxInput, OrderedSegmentDiscontinuity, OrderedSegmentReadError,
    OrderedSegmentSource,
};
use source_core::{ByteSource, CancellationToken, Seekability, SourceError};

use crate::{MpegTsDemuxError, MpegTsDemuxOptions};

/// Единственный поддерживаемый wire packet size.
pub(crate) const TS_PACKET_BYTES: usize = 188;

/// Нормализованный transport packet без раскрытия input storage наружу.
#[derive(Debug)]
pub(crate) struct TransportPacket {
    /// Packet identifier.
    pub(crate) pid: u16,
    /// Payload-unit-start indicator.
    pub(crate) payload_unit_start: bool,
    /// Transport error indicator.
    pub(crate) transport_error: bool,
    /// Scrambling control bits.
    pub(crate) scrambling: u8,
    /// Continuity counter.
    pub(crate) continuity_counter: u8,
    /// Adaptation-field discontinuity indicator.
    pub(crate) discontinuity: bool,
    /// Ordered transport явно начал новый media timeline на этом packet-е.
    pub(crate) starts_new_timeline: bool,
    /// Первый packet нового ordered media segment-а без обязательной смены timeline.
    pub(crate) starts_new_segment: bool,
    /// PCR base in 90 kHz units, если adaptation field его содержит.
    pub(crate) pcr_base: Option<u64>,
    /// Payload bytes после header/adaptation field.
    pub(crate) payload: Vec<u8>,
    /// Абсолютная byte-позиция sync byte.
    pub(crate) byte_offset: u64,
}

/// Input variants остаются нейтральными относительно local/network происхождения.
enum TransportInput {
    /// Random-access `ByteSource`, включая `LocalFileSource`.
    ByteSource(DemuxByteSource),
    /// Последовательный `Read` adapter.
    ByteStream(Box<dyn demux_api::DemuxByteStream>),
    /// Ordered segments с явным discontinuity marker-ом.
    OrderedSegments(Box<dyn OrderedSegmentSource>),
}

/// Bounded framing reader с replay-safe внутренним буфером.
pub(crate) struct TransportPacketReader {
    input: TransportInput,
    buffered_bytes: VecDeque<u8>,
    stream_position: u64,
    source_seekable: bool,
    discontinuity_offsets: VecDeque<u64>,
    segment_start_offsets: VecDeque<u64>,
    cancellation: CancellationToken,
    options: MpegTsDemuxOptions,
}

impl TransportPacketReader {
    /// Принимает уже восстановленный registry input.
    pub(crate) fn new(
        input: DemuxInput,
        cancellation: CancellationToken,
        options: MpegTsDemuxOptions,
    ) -> Self {
        let (input, source_seekable, stream_position) = match input {
            DemuxInput::ByteSource(source) => {
                let source_seekable = matches!(source.seekability(), Seekability::Seekable);
                let stream_position = source.position();
                (
                    TransportInput::ByteSource(source),
                    source_seekable,
                    stream_position,
                )
            }
            DemuxInput::ByteStream(reader) => (TransportInput::ByteStream(reader), false, 0),
            DemuxInput::OrderedSegments(source) => {
                (TransportInput::OrderedSegments(source), false, 0)
            }
        };
        Self {
            input,
            buffered_bytes: VecDeque::new(),
            stream_position,
            source_seekable,
            discontinuity_offsets: VecDeque::new(),
            segment_start_offsets: VecDeque::new(),
            cancellation,
            options,
        }
    }

    /// Возвращает offset следующего transport packet-а.
    pub(crate) const fn position(&self) -> u64 {
        self.stream_position
    }

    /// Проверяет random-access capability без догадок по duration.
    pub(crate) const fn is_seekable(&self) -> bool {
        self.source_seekable
    }

    /// Переставляет только настоящий `ByteSource` и сбрасывает framing state.
    pub(crate) fn seek_absolute(&mut self, offset: u64) -> Result<(), MpegTsDemuxError> {
        self.seek_internal(offset, true, true)
    }

    /// Возвращает cursor после служебного index scan без fake discontinuity event-а.
    pub(crate) fn restore_after_index_scan(&mut self, offset: u64) -> Result<(), MpegTsDemuxError> {
        self.seek_internal(offset, false, false)
    }

    /// Начинает служебный scan без playback discontinuity marker-а.
    pub(crate) fn begin_index_scan(&mut self, offset: u64) -> Result<(), MpegTsDemuxError> {
        self.seek_internal(offset, false, true)
    }

    fn seek_internal(
        &mut self,
        offset: u64,
        starts_new_timeline: bool,
        honor_cancellation: bool,
    ) -> Result<(), MpegTsDemuxError> {
        if honor_cancellation && self.cancellation.is_cancelled() {
            return Err(MpegTsDemuxError::Cancelled);
        }
        let TransportInput::ByteSource(source) = &mut self.input else {
            return Err(MpegTsDemuxError::NotSeekable);
        };
        source
            .seek(offset)
            .map_err(|error| MpegTsDemuxError::Source {
                reason: error.to_string(),
            })?;
        self.buffered_bytes.clear();
        self.stream_position = offset;
        self.discontinuity_offsets.clear();
        self.segment_start_offsets.clear();
        if starts_new_timeline {
            self.discontinuity_offsets.push_back(offset);
        }
        Ok(())
    }

    /// Читает один устойчиво синхронизированный 188-byte packet.
    pub(crate) fn next_packet(&mut self) -> Result<Option<TransportPacket>, MpegTsDemuxError> {
        if self.cancellation.is_cancelled() {
            return Err(MpegTsDemuxError::Cancelled);
        }
        self.fill_until(TS_PACKET_BYTES)?;
        if self.buffered_bytes.is_empty() {
            return Ok(None);
        }
        if self.looks_like_m2ts()? {
            return Err(MpegTsDemuxError::UnsupportedM2ts);
        }
        self.resynchronize()?;
        self.fill_until(TS_PACKET_BYTES)?;
        if self.buffered_bytes.len() < TS_PACKET_BYTES {
            return Err(MpegTsDemuxError::Malformed {
                reason: "оборван последний 188-byte transport packet".to_owned(),
            });
        }
        let packet_offset = self.stream_position;
        let raw_packet: Vec<u8> = self.buffered_bytes.drain(..TS_PACKET_BYTES).collect();
        self.stream_position = self.stream_position.saturating_add(TS_PACKET_BYTES as u64);
        parse_transport_packet(
            &raw_packet,
            packet_offset,
            self.take_discontinuity(packet_offset),
            self.take_segment_start(packet_offset),
        )
    }

    fn take_discontinuity(&mut self, packet_offset: u64) -> bool {
        if self
            .discontinuity_offsets
            .front()
            .is_some_and(|offset| *offset <= packet_offset)
        {
            self.discontinuity_offsets.pop_front();
            true
        } else {
            false
        }
    }

    fn take_segment_start(&mut self, packet_offset: u64) -> bool {
        if self
            .segment_start_offsets
            .front()
            .is_some_and(|offset| *offset <= packet_offset)
        {
            self.segment_start_offsets.pop_front();
            true
        } else {
            false
        }
    }

    fn looks_like_m2ts(&mut self) -> Result<bool, MpegTsDemuxError> {
        self.fill_until(4 + TS_PACKET_BYTES * 2 + 1)?;
        Ok(self.buffered_bytes.front() != Some(&0x47)
            && self.buffered_bytes.get(4) == Some(&0x47)
            && self.buffered_bytes.get(4 + 192) == Some(&0x47))
    }

    fn resynchronize(&mut self) -> Result<(), MpegTsDemuxError> {
        if self.buffered_bytes.front() == Some(&0x47) {
            return Ok(());
        }
        let bound = self.options.resync_bytes.get();
        self.fill_until(bound.saturating_add(TS_PACKET_BYTES * 2))?;
        let search_limit = self.buffered_bytes.len().min(bound);
        for skipped in 1..search_limit {
            if self.buffered_bytes.get(skipped) == Some(&0x47)
                && self.buffered_bytes.get(skipped + TS_PACKET_BYTES) == Some(&0x47)
            {
                self.buffered_bytes.drain(..skipped);
                self.stream_position = self.stream_position.saturating_add(skipped as u64);
                self.discontinuity_offsets.push_back(self.stream_position);
                return Ok(());
            }
        }
        Err(MpegTsDemuxError::SyncLost {
            searched_bytes: bound,
        })
    }

    fn fill_until(&mut self, minimum_bytes: usize) -> Result<(), MpegTsDemuxError> {
        while self.buffered_bytes.len() < minimum_bytes {
            if self.cancellation.is_cancelled() {
                return Err(MpegTsDemuxError::Cancelled);
            }
            let bytes_read = match &mut self.input {
                TransportInput::ByteSource(source) => {
                    let mut chunk = [0_u8; 32 * 1024];
                    let count = source
                        .read(&mut chunk, &self.cancellation)
                        .map_err(|error| {
                            if matches!(error, SourceError::Cancelled)
                                || self.cancellation.is_cancelled()
                            {
                                MpegTsDemuxError::Cancelled
                            } else {
                                MpegTsDemuxError::Source {
                                    reason: error.to_string(),
                                }
                            }
                        })?;
                    self.buffered_bytes.extend(&chunk[..count]);
                    count
                }
                TransportInput::ByteStream(reader) => {
                    let mut chunk = [0_u8; 32 * 1024];
                    let count = std::io::Read::read(reader, &mut chunk).map_err(|error| {
                        if self.cancellation.is_cancelled() {
                            MpegTsDemuxError::Cancelled
                        } else {
                            MpegTsDemuxError::Source {
                                reason: error.to_string(),
                            }
                        }
                    })?;
                    self.buffered_bytes.extend(&chunk[..count]);
                    count
                }
                TransportInput::OrderedSegments(source) => {
                    let Some(segment) =
                        source.next_segment(&self.cancellation).map_err(|error| {
                            if matches!(error, OrderedSegmentReadError::Cancelled)
                                || self.cancellation.is_cancelled()
                            {
                                MpegTsDemuxError::Cancelled
                            } else {
                                MpegTsDemuxError::Source {
                                    reason: error.to_string(),
                                }
                            }
                        })?
                    else {
                        return Ok(());
                    };
                    let marker_offset = self
                        .stream_position
                        .saturating_add(self.buffered_bytes.len() as u64);
                    self.segment_start_offsets.push_back(marker_offset);
                    if segment.discontinuity == OrderedSegmentDiscontinuity::StartsNewTimeline {
                        self.discontinuity_offsets.push_back(marker_offset);
                    }
                    let count = segment.bytes.len();
                    self.buffered_bytes.extend(segment.bytes);
                    count
                }
            };
            if bytes_read == 0 {
                break;
            }
        }
        Ok(())
    }
}

fn parse_transport_packet(
    bytes: &[u8],
    byte_offset: u64,
    external_discontinuity: bool,
    starts_new_segment: bool,
) -> Result<Option<TransportPacket>, MpegTsDemuxError> {
    if bytes.first() != Some(&0x47) {
        return Err(MpegTsDemuxError::Malformed {
            reason: "transport packet не начинается sync byte 0x47".to_owned(),
        });
    }
    let transport_error = bytes[1] & 0x80 != 0;
    let payload_unit_start = bytes[1] & 0x40 != 0;
    let pid = (u16::from(bytes[1] & 0x1f) << 8) | u16::from(bytes[2]);
    let scrambling = bytes[3] >> 6;
    let adaptation_control = (bytes[3] >> 4) & 0x03;
    let continuity_counter = bytes[3] & 0x0f;
    if adaptation_control == 0 {
        return Err(MpegTsDemuxError::Malformed {
            reason: format!("reserved adaptation_field_control на PID {pid}"),
        });
    }
    let mut payload_start = 4_usize;
    let mut discontinuity = external_discontinuity;
    let mut pcr_base = None;
    if adaptation_control & 0x02 != 0 {
        let adaptation_length = usize::from(bytes[4]);
        if adaptation_length > 183 || 5 + adaptation_length > TS_PACKET_BYTES {
            return Err(MpegTsDemuxError::Malformed {
                reason: format!("adaptation field выходит за packet на PID {pid}"),
            });
        }
        payload_start = 5 + adaptation_length;
        if adaptation_length > 0 {
            let flags = bytes[5];
            discontinuity |= flags & 0x80 != 0;
            if flags & 0x10 != 0 {
                if adaptation_length < 7 {
                    return Err(MpegTsDemuxError::Malformed {
                        reason: format!("оборван PCR на PID {pid}"),
                    });
                }
                pcr_base = Some(
                    (u64::from(bytes[6]) << 25)
                        | (u64::from(bytes[7]) << 17)
                        | (u64::from(bytes[8]) << 9)
                        | (u64::from(bytes[9]) << 1)
                        | (u64::from(bytes[10]) >> 7),
                );
            }
        }
    }
    let payload = if adaptation_control & 0x01 != 0 {
        bytes[payload_start..].to_vec()
    } else {
        Vec::new()
    };
    Ok(Some(TransportPacket {
        pid,
        payload_unit_start,
        transport_error,
        scrambling,
        continuity_counter,
        discontinuity,
        starts_new_timeline: external_discontinuity,
        starts_new_segment,
        pcr_base,
        payload,
        byte_offset,
    }))
}
