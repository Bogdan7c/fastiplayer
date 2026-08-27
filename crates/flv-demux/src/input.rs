use std::io::Read;

use bytes::Bytes;
use demux_api::{DemuxByteSource, DemuxInput, OrderedSegmentDiscontinuity, OrderedSegmentSource};
use source_core::{ByteSource, CancellationToken, Seekability};

use crate::f4f::parse_f4f_segment;
use crate::framing::{
    FlvTag, parse_flv_header, parse_tag_bytes, parse_tag_header, previous_tag_size_bytes,
    tag_header_bytes, validate_previous_tag_size,
};
use crate::{FlvDemuxError, FlvDemuxOptions};

/// Input-level discontinuity, которую demuxer применяет до следующего tag.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum InputDiscontinuity {
    Continuous,
    StartsNewTimeline,
}

/// Один tag вместе с segment lifecycle evidence.
pub(crate) struct InputTag {
    pub(crate) tag: FlvTag,
    pub(crate) discontinuity: InputDiscontinuity,
}

enum RawReader {
    Source(DemuxByteSource),
    Stream(Box<dyn Read + Send>),
}

impl RawReader {
    fn read(
        &mut self,
        output: &mut [u8],
        cancellation: &CancellationToken,
    ) -> Result<usize, FlvDemuxError> {
        if cancellation.is_cancelled() {
            return Err(FlvDemuxError::Cancelled);
        }
        match self {
            Self::Source(source) => {
                source
                    .read(output, cancellation)
                    .map_err(|error| FlvDemuxError::Source {
                        reason: error.to_string(),
                    })
            }
            Self::Stream(stream) => stream.read(output).map_err(|error| FlvDemuxError::Source {
                reason: error.to_string(),
            }),
        }
    }

    fn position(&self, fallback: u64) -> u64 {
        match self {
            Self::Source(source) => source.position(),
            Self::Stream(_) => fallback,
        }
    }

    fn is_seekable(&self) -> bool {
        matches!(self, Self::Source(source) if source.seekability() == Seekability::Seekable)
    }

    fn seek(&mut self, offset: u64) -> Result<(), FlvDemuxError> {
        match self {
            Self::Source(source) if source.seekability() == Seekability::Seekable => {
                source.seek(offset).map_err(|error| FlvDemuxError::Source {
                    reason: error.to_string(),
                })
            }
            _ => Err(FlvDemuxError::NotSeekable),
        }
    }
}

pub(crate) struct RawFlvInput {
    reader: RawReader,
    logical_position: u64,
    cancellation: CancellationToken,
    options: FlvDemuxOptions,
    replay_bytes: std::collections::VecDeque<u8>,
    first_tag_offset: u64,
}

pub(crate) struct F4fInput {
    source: Box<dyn OrderedSegmentSource>,
    expected_sequence: Option<u64>,
    pending_payloads: std::collections::VecDeque<Bytes>,
    current_payload: Option<(Bytes, usize)>,
    current_discontinuity: InputDiscontinuity,
    cancellation: CancellationToken,
    options: FlvDemuxOptions,
}

/// Общий reader, который нормализует raw FLV и F4F mdat в последовательность tags.
pub(crate) enum FlvInput {
    Raw(RawFlvInput),
    F4f(F4fInput),
}

impl FlvInput {
    pub(crate) fn open_raw(
        input: DemuxInput,
        cancellation: CancellationToken,
        options: FlvDemuxOptions,
    ) -> Result<Self, FlvDemuxError> {
        let reader = match input {
            DemuxInput::ByteSource(source) => RawReader::Source(source),
            DemuxInput::ByteStream(stream) => RawReader::Stream(stream),
            DemuxInput::OrderedSegments(_) => {
                return Err(FlvDemuxError::UnsupportedInput {
                    container: "flv",
                    input: "ordered-segments",
                });
            }
            DemuxInput::OrderedResourceStream(_) => {
                return Err(FlvDemuxError::UnsupportedInput {
                    container: "flv",
                    input: "ordered-resource-stream",
                });
            }
        };
        let mut raw = RawFlvInput {
            reader,
            logical_position: 0,
            cancellation,
            options,
            replay_bytes: std::collections::VecDeque::new(),
            first_tag_offset: 0,
        };
        raw.read_and_validate_header()?;
        Ok(Self::Raw(raw))
    }

    pub(crate) fn open_f4f(
        input: DemuxInput,
        cancellation: CancellationToken,
        options: FlvDemuxOptions,
    ) -> Result<Self, FlvDemuxError> {
        let DemuxInput::OrderedSegments(source) = input else {
            return Err(FlvDemuxError::UnsupportedInput {
                container: "f4f",
                input: "raw-bytes",
            });
        };
        Ok(Self::F4f(F4fInput {
            source,
            expected_sequence: None,
            pending_payloads: std::collections::VecDeque::new(),
            current_payload: None,
            current_discontinuity: InputDiscontinuity::Continuous,
            cancellation,
            options,
        }))
    }

    pub(crate) fn next_tag(&mut self) -> Result<Option<InputTag>, FlvDemuxError> {
        match self {
            Self::Raw(raw) => raw.next_tag().map(|tag| {
                tag.map(|tag| InputTag {
                    tag,
                    discontinuity: InputDiscontinuity::Continuous,
                })
            }),
            Self::F4f(f4f) => f4f.next_tag(),
        }
    }

    pub(crate) fn is_seekable(&self) -> bool {
        matches!(self, Self::Raw(raw) if raw.reader.is_seekable())
    }

    pub(crate) fn can_recover_raw(&self) -> bool {
        matches!(self, Self::Raw(_))
    }

    pub(crate) fn position(&self) -> u64 {
        match self {
            Self::Raw(raw) => raw
                .reader
                .position(raw.logical_position)
                .saturating_sub(raw.replay_bytes.len() as u64),
            Self::F4f(_) => 0,
        }
    }

    pub(crate) fn seek_raw_tag(&mut self, offset: u64) -> Result<(), FlvDemuxError> {
        match self {
            Self::Raw(raw) => {
                raw.reader.seek(offset)?;
                raw.logical_position = offset;
                raw.replay_bytes.clear();
                Ok(())
            }
            Self::F4f(_) => Err(FlvDemuxError::NotSeekable),
        }
    }

    pub(crate) fn first_tag_offset(&self) -> Option<u64> {
        match self {
            Self::Raw(raw) => Some(raw.first_tag_offset),
            Self::F4f(_) => None,
        }
    }

    /// Ищет следующий полностью доказанный tag boundary в bounded raw window.
    pub(crate) fn recover_raw_tag(&mut self) -> Result<Option<InputTag>, FlvDemuxError> {
        let Self::Raw(raw) = self else {
            return Err(FlvDemuxError::RecoveryExhausted { searched_bytes: 0 });
        };
        let limit = raw.options.recovery_bytes.get();
        let mut recovery = Vec::new();
        recovery
            .try_reserve_exact(limit)
            .map_err(|_| FlvDemuxError::RecoveryExhausted {
                searched_bytes: limit,
            })?;
        while recovery.len() < limit {
            let mut byte = [0_u8; 1];
            if !raw.read_exact(&mut byte, true)? {
                break;
            }
            recovery.push(byte[0]);
        }
        for start in 0..recovery.len() {
            let offset = raw
                .logical_position
                .saturating_sub(recovery.len() as u64)
                .saturating_add(start as u64);
            let Ok((tag, tag_size)) = parse_tag_bytes(&recovery[start..], offset, raw.options)
            else {
                continue;
            };
            let previous_start = start.saturating_add(tag_size);
            let previous_end = previous_start.saturating_add(previous_tag_size_bytes());
            let Some(previous) = recovery.get(previous_start..previous_end) else {
                continue;
            };
            if validate_previous_tag_size(previous, tag_size, offset + tag_size as u64).is_err() {
                continue;
            }
            raw.replay_bytes
                .extend(recovery[previous_end..].iter().copied());
            return Ok(Some(InputTag {
                tag,
                discontinuity: InputDiscontinuity::StartsNewTimeline,
            }));
        }
        Err(FlvDemuxError::RecoveryExhausted {
            searched_bytes: recovery.len(),
        })
    }
}

impl RawFlvInput {
    fn read_and_validate_header(&mut self) -> Result<(), FlvDemuxError> {
        let mut fixed = [0_u8; 9];
        self.read_exact(&mut fixed, false)?;
        let header = parse_flv_header(&fixed)?;
        let extra_header_bytes = header.data_offset - fixed.len();
        if extra_header_bytes > self.options.tag_bytes.get() {
            return Err(FlvDemuxError::InvalidHeader {
                reason: "extended header превышает tag byte limit".to_owned(),
            });
        }
        let mut extension = vec![0_u8; extra_header_bytes];
        self.read_exact(&mut extension, false)?;
        let mut first_previous_size = [0_u8; 4];
        self.read_exact(&mut first_previous_size, false)?;
        validate_previous_tag_size(&first_previous_size, 0, self.logical_position - 4)?;
        self.first_tag_offset = self.logical_position;
        Ok(())
    }

    fn next_tag(&mut self) -> Result<Option<FlvTag>, FlvDemuxError> {
        let tag_offset = self
            .logical_position
            .saturating_sub(self.replay_bytes.len() as u64);
        let mut candidate = Vec::new();
        match self.read_exact_append(&mut candidate, tag_header_bytes(), true) {
            Ok(false) => return Ok(None),
            Ok(true) => {}
            Err(error) => return Err(self.rollback_candidate(candidate, error)),
        }
        let payload_size = match parse_tag_header(&candidate, tag_offset, self.options) {
            Ok(header) => header.payload_size,
            Err(error) => return Err(self.rollback_candidate(candidate, error)),
        };
        if candidate
            .try_reserve_exact(payload_size + previous_tag_size_bytes())
            .is_err()
        {
            let error = FlvDemuxError::TagTooLarge {
                offset: tag_offset,
                declared_bytes: payload_size,
                limit_bytes: self.options.tag_bytes.get(),
            };
            return Err(self.rollback_candidate(candidate, error));
        }
        if let Err(error) = self.read_exact_append(&mut candidate, payload_size, false) {
            return Err(self.rollback_candidate(candidate, error));
        }
        let framed_bytes = tag_header_bytes() + payload_size;
        let (tag, tag_size) =
            match parse_tag_bytes(&candidate[..framed_bytes], tag_offset, self.options) {
                Ok(parsed) => parsed,
                Err(error) => return Err(self.rollback_candidate(candidate, error)),
            };
        if let Err(error) = self.read_exact_append(&mut candidate, previous_tag_size_bytes(), false)
        {
            return Err(self.rollback_candidate(candidate, error));
        }
        if let Err(error) = validate_previous_tag_size(
            &candidate[framed_bytes..],
            tag_size,
            tag_offset.saturating_add(tag_size as u64),
        ) {
            return Err(self.rollback_candidate(candidate, error));
        }
        Ok(Some(tag))
    }

    /// Читает exact candidate bytes, не коммитя их удаление из logical stream.
    fn read_exact_append(
        &mut self,
        candidate: &mut Vec<u8>,
        required_bytes: usize,
        allow_clean_eof: bool,
    ) -> Result<bool, FlvDemuxError> {
        let initial_length = candidate.len();
        while candidate.len() - initial_length < required_bytes {
            while candidate.len() - initial_length < required_bytes {
                let Some(byte) = self.replay_bytes.pop_front() else {
                    break;
                };
                candidate.push(byte);
            }
            if candidate.len() - initial_length == required_bytes {
                break;
            }
            let remaining = required_bytes - (candidate.len() - initial_length);
            let mut buffer = [0_u8; 8 * 1024];
            let requested = remaining.min(buffer.len());
            let count = self
                .reader
                .read(&mut buffer[..requested], &self.cancellation)?;
            if count == 0 {
                if allow_clean_eof && candidate.len() == initial_length {
                    return Ok(false);
                }
                return Err(FlvDemuxError::Source {
                    reason: format!(
                        "short read: ожидалось {required_bytes} bytes, получено {}",
                        candidate.len() - initial_length
                    ),
                });
            }
            candidate.extend_from_slice(&buffer[..count]);
            self.logical_position = self.logical_position.saturating_add(count as u64);
        }
        Ok(true)
    }

    /// Возвращает все bytes неуспешного candidate-а перед bounded resync scan.
    fn rollback_candidate(&mut self, candidate: Vec<u8>, error: FlvDemuxError) -> FlvDemuxError {
        for byte in candidate.into_iter().rev() {
            self.replay_bytes.push_front(byte);
        }
        error
    }

    fn read_exact(
        &mut self,
        output: &mut [u8],
        allow_clean_eof: bool,
    ) -> Result<bool, FlvDemuxError> {
        let mut filled = 0_usize;
        while filled < output.len() {
            while filled < output.len() {
                let Some(byte) = self.replay_bytes.pop_front() else {
                    break;
                };
                output[filled] = byte;
                filled += 1;
            }
            if filled == output.len() {
                break;
            }
            let count = self
                .reader
                .read(&mut output[filled..], &self.cancellation)?;
            if count == 0 {
                if allow_clean_eof && filled == 0 {
                    return Ok(false);
                }
                for byte in output[..filled].iter().rev() {
                    self.replay_bytes.push_front(*byte);
                }
                return Err(FlvDemuxError::Source {
                    reason: format!(
                        "short read: ожидалось {} bytes, получено {filled}",
                        output.len()
                    ),
                });
            }
            filled += count;
            self.logical_position = self.logical_position.saturating_add(count as u64);
        }
        Ok(true)
    }
}

impl F4fInput {
    fn next_tag(&mut self) -> Result<Option<InputTag>, FlvDemuxError> {
        loop {
            if let Some((payload, cursor)) = &mut self.current_payload {
                if *cursor < payload.len() {
                    let offset = u64::try_from(*cursor).unwrap_or(u64::MAX);
                    let (tag, tag_size) =
                        parse_tag_bytes(&payload[*cursor..], offset, self.options)?;
                    let previous_start = cursor.checked_add(tag_size).ok_or_else(|| {
                        FlvDemuxError::MalformedF4f {
                            sequence: self.expected_sequence.unwrap_or(0),
                            reason: "FLV tag cursor overflow".to_owned(),
                        }
                    })?;
                    let previous_end = previous_start + previous_tag_size_bytes();
                    let previous = payload.get(previous_start..previous_end).ok_or_else(|| {
                        FlvDemuxError::MalformedF4f {
                            sequence: self.expected_sequence.unwrap_or(0),
                            reason: "mdat FLV PreviousTagSize обрезан".to_owned(),
                        }
                    })?;
                    validate_previous_tag_size(previous, tag_size, offset + tag_size as u64)?;
                    *cursor = previous_end;
                    let discontinuity = self.current_discontinuity;
                    self.current_discontinuity = InputDiscontinuity::Continuous;
                    return Ok(Some(InputTag { tag, discontinuity }));
                }
                self.current_payload = None;
            }
            if let Some(payload) = self.pending_payloads.pop_front() {
                self.current_payload = Some((payload, 0));
                continue;
            }
            if self.cancellation.is_cancelled() {
                return Err(FlvDemuxError::Cancelled);
            }
            let Some(segment) = self
                .source
                .next_segment(&self.cancellation)
                .map_err(|error| FlvDemuxError::Source {
                    reason: error.to_string(),
                })?
            else {
                return Ok(None);
            };
            let sequence = segment.sequence.get();
            if let Some(expected) = self.expected_sequence
                && sequence != expected
            {
                return Err(FlvDemuxError::SegmentSequence {
                    expected,
                    actual: sequence,
                });
            }
            self.expected_sequence = Some(sequence.saturating_add(1));
            let parsed = parse_f4f_segment(sequence, segment.kind, segment.bytes, self.options)?;
            if segment.discontinuity == OrderedSegmentDiscontinuity::StartsNewTimeline {
                self.current_discontinuity = InputDiscontinuity::StartsNewTimeline;
            }
            self.pending_payloads.extend(parsed.media_payloads);
        }
    }
}
