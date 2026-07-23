use std::time::Duration;

use media_core::{MediaContainerMetadata, MediaMetadata, MediaTagMetadata};

use crate::{FlvDemuxError, FlvDemuxOptions};

/// Недоверенный metadata anchor; seek обязан подтвердить actual tag/config/keyframe.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) struct MetadataAnchor {
    pub(crate) timestamp: Duration,
    pub(crate) byte_offset: u64,
}

/// Bounded подмножество `onMetaData`, которое влияет на runtime.
#[derive(Debug, Clone, Default, PartialEq)]
pub(crate) struct FlvMetadata {
    pub(crate) duration: Option<Duration>,
    pub(crate) anchors: Vec<MetadataAnchor>,
    pub(crate) media_metadata: MediaMetadata,
}

#[derive(Debug, Clone, PartialEq)]
enum AmfValue {
    Number(f64),
    Boolean,
    String(String),
    Object(Vec<(String, AmfValue)>),
    Array(Vec<AmfValue>),
    Null,
}

struct AmfReader<'a> {
    bytes: &'a [u8],
    cursor: usize,
    entries: usize,
    options: FlvDemuxOptions,
}

/// Разбирает только exact `onMetaData` payload; прочие script events игнорируются.
pub(crate) fn parse_on_metadata(
    bytes: &[u8],
    options: FlvDemuxOptions,
) -> Result<Option<FlvMetadata>, FlvDemuxError> {
    let mut reader = AmfReader {
        bytes,
        cursor: 0,
        entries: 0,
        options,
    };
    let event = reader.read_value(0)?;
    if event != AmfValue::String("onMetaData".to_owned()) {
        return Ok(None);
    }
    let root = reader.read_value(0)?;
    if reader.cursor != bytes.len() {
        return Err(malformed("script tag содержит trailing AMF bytes"));
    }
    let AmfValue::Object(entries) = root else {
        return Err(malformed("onMetaData value должен быть object/ECMA array"));
    };
    let duration = object_number(&entries, "duration").and_then(duration_from_amf_seconds);
    let title = object_string(&entries, "title").map(ToOwned::to_owned);
    let anchors = parse_keyframe_anchors(&entries, options.index_entries.get());
    Ok(Some(FlvMetadata {
        duration,
        anchors,
        media_metadata: MediaMetadata {
            container: Some(MediaContainerMetadata {
                format_name: Some("Flash Video".to_owned()),
            }),
            tags: MediaTagMetadata {
                title,
                ..Default::default()
            },
        },
    }))
}

fn parse_keyframe_anchors(entries: &[(String, AmfValue)], limit: usize) -> Vec<MetadataAnchor> {
    let Some(AmfValue::Object(keyframes)) = object_value(entries, "keyframes") else {
        return Vec::new();
    };
    let Some(AmfValue::Array(times)) = object_value(keyframes, "times") else {
        return Vec::new();
    };
    let Some(AmfValue::Array(file_positions)) = object_value(keyframes, "filepositions") else {
        return Vec::new();
    };
    times
        .iter()
        .zip(file_positions)
        .take(limit)
        .filter_map(|(time, position)| {
            let (AmfValue::Number(time), AmfValue::Number(position)) = (time, position) else {
                return None;
            };
            let timestamp = duration_from_amf_seconds(*time)?;
            let byte_offset = u64_from_amf_integer(*position)?;
            Some(MetadataAnchor {
                timestamp,
                byte_offset,
            })
        })
        .collect()
}

/// Конвертирует недоверенное AMF number без panic на NaN/inf/overflow.
fn duration_from_amf_seconds(seconds: f64) -> Option<Duration> {
    if !seconds.is_finite() || seconds < 0.0 {
        return None;
    }
    Duration::try_from_secs_f64(seconds).ok()
}

/// Конвертирует только exact non-negative integer внутри представимого u64 domain.
fn u64_from_amf_integer(value: f64) -> Option<u64> {
    if !value.is_finite() || value < 0.0 || value.fract() != 0.0 || value >= u64::MAX as f64 {
        return None;
    }
    Some(value as u64)
}

fn object_value<'a>(entries: &'a [(String, AmfValue)], key: &str) -> Option<&'a AmfValue> {
    entries
        .iter()
        .find_map(|(name, value)| (name == key).then_some(value))
}

fn object_number(entries: &[(String, AmfValue)], key: &str) -> Option<f64> {
    match object_value(entries, key) {
        Some(AmfValue::Number(value)) => Some(*value),
        _ => None,
    }
}

fn object_string<'a>(entries: &'a [(String, AmfValue)], key: &str) -> Option<&'a str> {
    match object_value(entries, key) {
        Some(AmfValue::String(value)) => Some(value),
        _ => None,
    }
}

impl AmfReader<'_> {
    fn read_value(&mut self, depth: usize) -> Result<AmfValue, FlvDemuxError> {
        if depth >= self.options.metadata_depth.get() {
            return Err(malformed("AMF nesting depth превышен"));
        }
        let marker = self.read_u8()?;
        match marker {
            0 => Ok(AmfValue::Number(f64::from_bits(self.read_u64()?))),
            1 => {
                let _value = self.read_u8()?;
                Ok(AmfValue::Boolean)
            }
            2 => Ok(AmfValue::String(self.read_short_string()?)),
            3 => Ok(AmfValue::Object(self.read_object(depth + 1)?)),
            5 | 6 => Ok(AmfValue::Null),
            8 => {
                let declared = self.read_u32()?;
                if usize::try_from(declared).unwrap_or(usize::MAX)
                    > self.options.metadata_entries.get()
                {
                    return Err(malformed("AMF ECMA array declared count превышен"));
                }
                Ok(AmfValue::Object(self.read_object(depth + 1)?))
            }
            10 => {
                let count = usize::try_from(self.read_u32()?)
                    .map_err(|_| malformed("AMF array count не помещается в usize"))?;
                if count > self.options.metadata_entries.get() {
                    return Err(malformed("AMF strict array count превышен"));
                }
                let mut values = Vec::new();
                values
                    .try_reserve_exact(count)
                    .map_err(|_| malformed("AMF array allocation отклонена"))?;
                for _ in 0..count {
                    values.push(self.read_value(depth + 1)?);
                }
                Ok(AmfValue::Array(values))
            }
            11 => {
                let _milliseconds = self.read_u64()?;
                let _timezone = self.read_u16()?;
                Ok(AmfValue::Null)
            }
            12 => Ok(AmfValue::String(self.read_long_string()?)),
            unsupported => Err(malformed(format!(
                "AMF marker {unsupported} не поддерживается"
            ))),
        }
    }

    fn read_object(&mut self, depth: usize) -> Result<Vec<(String, AmfValue)>, FlvDemuxError> {
        let mut entries = Vec::new();
        loop {
            let name_length = usize::from(self.peek_u16()?);
            if name_length == 0 && self.bytes.get(self.cursor + 2) == Some(&9) {
                self.cursor += 3;
                return Ok(entries);
            }
            self.entries += 1;
            if self.entries > self.options.metadata_entries.get() {
                return Err(malformed("AMF object entry limit превышен"));
            }
            let name = self.read_short_string()?;
            let value = self.read_value(depth)?;
            entries.push((name, value));
        }
    }

    fn read_short_string(&mut self) -> Result<String, FlvDemuxError> {
        let length = usize::from(self.read_u16()?);
        self.read_string_bytes(length)
    }

    fn read_long_string(&mut self) -> Result<String, FlvDemuxError> {
        let length = usize::try_from(self.read_u32()?)
            .map_err(|_| malformed("AMF string length не помещается в usize"))?;
        self.read_string_bytes(length)
    }

    fn read_string_bytes(&mut self, length: usize) -> Result<String, FlvDemuxError> {
        if length > self.options.metadata_string_bytes.get() {
            return Err(malformed("AMF string byte limit превышен"));
        }
        let bytes = self.take(length)?;
        std::str::from_utf8(bytes)
            .map(ToOwned::to_owned)
            .map_err(|_| malformed("AMF string не UTF-8"))
    }

    fn peek_u16(&self) -> Result<u16, FlvDemuxError> {
        let bytes = self
            .bytes
            .get(self.cursor..self.cursor + 2)
            .ok_or_else(|| malformed("AMF object key обрезан"))?;
        Ok(u16::from_be_bytes(bytes.try_into().expect("exact slice")))
    }

    fn read_u8(&mut self) -> Result<u8, FlvDemuxError> {
        Ok(self.take(1)?[0])
    }

    fn read_u16(&mut self) -> Result<u16, FlvDemuxError> {
        Ok(u16::from_be_bytes(
            self.take(2)?.try_into().expect("exact slice"),
        ))
    }

    fn read_u32(&mut self) -> Result<u32, FlvDemuxError> {
        Ok(u32::from_be_bytes(
            self.take(4)?.try_into().expect("exact slice"),
        ))
    }

    fn read_u64(&mut self) -> Result<u64, FlvDemuxError> {
        Ok(u64::from_be_bytes(
            self.take(8)?.try_into().expect("exact slice"),
        ))
    }

    fn take(&mut self, length: usize) -> Result<&[u8], FlvDemuxError> {
        let end = self
            .cursor
            .checked_add(length)
            .ok_or_else(|| malformed("AMF cursor overflow"))?;
        let bytes = self
            .bytes
            .get(self.cursor..end)
            .ok_or_else(|| malformed("AMF payload обрезан"))?;
        self.cursor = end;
        Ok(bytes)
    }
}

fn malformed(reason: impl Into<String>) -> FlvDemuxError {
    FlvDemuxError::MalformedTag {
        offset: 0,
        reason: reason.into(),
    }
}
