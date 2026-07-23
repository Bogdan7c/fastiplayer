use bytes::Bytes;

use crate::{FlvDemuxError, FlvDemuxOptions};

pub(crate) const FLV_SIGNATURE: &[u8; 3] = b"FLV";
const FLV_HEADER_BYTES: usize = 9;
const TAG_HEADER_BYTES: usize = 11;
const PREVIOUS_TAG_SIZE_BYTES: usize = 4;

/// Exact FLV tag families; unknown values остаются typed malformed input.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum FlvTagKind {
    Audio,
    Video,
    Script,
}

/// Один полностью framed tag с byte offset исходного tag header-а.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct FlvTag {
    pub(crate) kind: FlvTagKind,
    pub(crate) timestamp_ms: u32,
    pub(crate) payload: Bytes,
    pub(crate) byte_offset: u64,
}

/// Проверенный raw FLV header.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) struct FlvHeader {
    pub(crate) has_audio: bool,
    pub(crate) has_video: bool,
    pub(crate) data_offset: usize,
}

/// Проверенные fixed поля tag header-а до allocation payload buffer-а.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) struct FlvTagHeader {
    pub(crate) kind: FlvTagKind,
    pub(crate) timestamp_ms: u32,
    pub(crate) payload_size: usize,
}

/// Разбирает fixed header и не принимает reserved flags/unsupported version.
pub(crate) fn parse_flv_header(bytes: &[u8]) -> Result<FlvHeader, FlvDemuxError> {
    let header = bytes
        .get(..FLV_HEADER_BYTES)
        .ok_or_else(|| FlvDemuxError::InvalidHeader {
            reason: format!("нужно {FLV_HEADER_BYTES} bytes, доступно {}", bytes.len()),
        })?;
    if &header[..3] != FLV_SIGNATURE {
        return Err(FlvDemuxError::InvalidHeader {
            reason: "signature не равна FLV".to_owned(),
        });
    }
    if header[3] != 1 {
        return Err(FlvDemuxError::InvalidHeader {
            reason: format!("version {} не поддерживается", header[3]),
        });
    }
    let flags = header[4];
    if flags & !0b0000_0101 != 0 || flags & 0b0000_0010 != 0 {
        return Err(FlvDemuxError::InvalidHeader {
            reason: format!("reserved flags выставлены: 0x{flags:02x}"),
        });
    }
    let data_offset = u32::from_be_bytes(header[5..9].try_into().expect("exact slice"));
    let data_offset = usize::try_from(data_offset).map_err(|_| FlvDemuxError::InvalidHeader {
        reason: "data offset не помещается в usize".to_owned(),
    })?;
    if data_offset < FLV_HEADER_BYTES {
        return Err(FlvDemuxError::InvalidHeader {
            reason: format!("data offset {data_offset} меньше header"),
        });
    }
    Ok(FlvHeader {
        has_audio: flags & 0b0000_0100 != 0,
        has_video: flags & 0b0000_0001 != 0,
        data_offset,
    })
}

/// Разбирает tag header + payload без PreviousTagSize.
pub(crate) fn parse_tag_bytes(
    bytes: &[u8],
    byte_offset: u64,
    options: FlvDemuxOptions,
) -> Result<(FlvTag, usize), FlvDemuxError> {
    let tag_header = parse_tag_header(bytes, byte_offset, options)?;
    let payload_size = tag_header.payload_size;
    let total_size = TAG_HEADER_BYTES
        .checked_add(payload_size)
        .ok_or_else(|| malformed(byte_offset, "tag size overflow"))?;
    let payload = bytes
        .get(TAG_HEADER_BYTES..total_size)
        .ok_or_else(|| malformed(byte_offset, "tag payload обрезан"))?;
    Ok((
        FlvTag {
            kind: tag_header.kind,
            timestamp_ms: tag_header.timestamp_ms,
            payload: Bytes::copy_from_slice(payload),
            byte_offset,
        },
        total_size,
    ))
}

/// Валидирует fixed header до memory allocation по недоверенному payload size.
pub(crate) fn parse_tag_header(
    bytes: &[u8],
    byte_offset: u64,
    options: FlvDemuxOptions,
) -> Result<FlvTagHeader, FlvDemuxError> {
    let header = bytes
        .get(..TAG_HEADER_BYTES)
        .ok_or_else(|| malformed(byte_offset, "tag header обрезан"))?;
    let tag_flags = header[0];
    if tag_flags & 0b1100_0000 != 0 {
        return Err(malformed(byte_offset, "reserved tag bits выставлены"));
    }
    if tag_flags & 0b0010_0000 != 0 {
        return Err(FlvDemuxError::UnsupportedCodec {
            reason: "filtered/encrypted FLV tags не поддерживаются".to_owned(),
        });
    }
    let kind = match tag_flags & 0x1f {
        8 => FlvTagKind::Audio,
        9 => FlvTagKind::Video,
        18 => FlvTagKind::Script,
        tag_type => {
            return Err(malformed(
                byte_offset,
                format!("unknown tag type {tag_type}"),
            ));
        }
    };
    let payload_size = read_u24(&header[1..4]);
    if payload_size > options.tag_bytes.get() {
        return Err(FlvDemuxError::TagTooLarge {
            offset: byte_offset,
            declared_bytes: payload_size,
            limit_bytes: options.tag_bytes.get(),
        });
    }
    let stream_id = read_u24(&header[8..11]);
    if stream_id != 0 {
        return Err(malformed(
            byte_offset,
            format!("StreamID должен быть 0, получено {stream_id}"),
        ));
    }
    let timestamp_ms = u32::from(header[7]) << 24 | read_u24_u32(&header[4..7]);
    Ok(FlvTagHeader {
        kind,
        timestamp_ms,
        payload_size,
    })
}

/// Проверяет PreviousTagSize относительно фактического tag size.
pub(crate) fn validate_previous_tag_size(
    bytes: &[u8],
    expected_tag_size: usize,
    offset: u64,
) -> Result<(), FlvDemuxError> {
    let value = bytes
        .get(..PREVIOUS_TAG_SIZE_BYTES)
        .ok_or_else(|| malformed(offset, "PreviousTagSize обрезан"))?;
    let actual = u32::from_be_bytes(value.try_into().expect("exact slice"));
    if usize::try_from(actual).ok() != Some(expected_tag_size) {
        return Err(malformed(
            offset,
            format!("PreviousTagSize={actual}, ожидалось {expected_tag_size}"),
        ));
    }
    Ok(())
}

#[must_use]
pub(crate) const fn tag_header_bytes() -> usize {
    TAG_HEADER_BYTES
}

#[must_use]
pub(crate) const fn previous_tag_size_bytes() -> usize {
    PREVIOUS_TAG_SIZE_BYTES
}

fn read_u24(bytes: &[u8]) -> usize {
    usize::from(bytes[0]) << 16 | usize::from(bytes[1]) << 8 | usize::from(bytes[2])
}

fn read_u24_u32(bytes: &[u8]) -> u32 {
    u32::from(bytes[0]) << 16 | u32::from(bytes[1]) << 8 | u32::from(bytes[2])
}

fn malformed(offset: u64, reason: impl Into<String>) -> FlvDemuxError {
    FlvDemuxError::MalformedTag {
        offset,
        reason: reason.into(),
    }
}
