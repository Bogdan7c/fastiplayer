use std::fmt;
use std::io::{self, Read};

use serde::de::{self, DeserializeSeed, MapAccess, SeqAccess, Visitor};
use serde::{Deserialize, Deserializer};
use sha2::{Digest, Sha256};

use crate::types::ProtectedStateCause;
use crate::{MAX_STATE_ENVELOPE_NESTING_DEPTH, MAX_STATE_ENVELOPE_TOKEN_BYTES};

/// Bytes и digest получены одним bounded streaming pass.
pub(crate) struct EnvelopeProof {
    pub(crate) schema_version: u64,
    pub(crate) inspected_bytes: Vec<u8>,
    pub(crate) content_sha256: [u8; 32],
}

/// Reader одновременно применяет hard budget, сохраняет exact bytes и считает digest.
struct BudgetedCaptureReader<R> {
    inner: R,
    maximum_bytes: u64,
    consumed_bytes: u64,
    captured_bytes: Vec<u8>,
    hasher: Sha256,
    exhausted: bool,
}

impl<R> BudgetedCaptureReader<R> {
    fn new(inner: R, maximum_bytes: u64) -> Self {
        Self {
            inner,
            maximum_bytes,
            consumed_bytes: 0,
            captured_bytes: Vec::new(),
            hasher: Sha256::new(),
            exhausted: false,
        }
    }

    fn finish(self) -> (Vec<u8>, [u8; 32]) {
        let digest = self.hasher.finalize();
        let mut exact_digest = [0_u8; 32];
        exact_digest.copy_from_slice(&digest);
        (self.captured_bytes, exact_digest)
    }
}

impl<R: Read> Read for BudgetedCaptureReader<R> {
    fn read(&mut self, output: &mut [u8]) -> io::Result<usize> {
        if output.is_empty() {
            return Ok(0);
        }

        if self.consumed_bytes == self.maximum_bytes {
            let mut overflow_probe = [0_u8; 1];
            let overflow_count = self.inner.read(&mut overflow_probe)?;
            if overflow_count == 0 {
                return Ok(0);
            }
            self.exhausted = true;
            return Err(io::Error::other("playlist state envelope budget exhausted"));
        }

        let remaining = self.maximum_bytes - self.consumed_bytes;
        let allowed = output.len().min(remaining as usize);
        let read_count = self.inner.read(&mut output[..allowed])?;
        if read_count == 0 {
            return Ok(0);
        }

        let bytes = &output[..read_count];
        self.consumed_bytes += read_count as u64;
        self.captured_bytes.extend_from_slice(bytes);
        self.hasher.update(bytes);
        Ok(read_count)
    }
}

#[derive(Default)]
struct EnvelopeObservation {
    schema_version_occurrences: usize,
    schema_version: Option<u64>,
    non_integer_schema_version: bool,
}

struct EnvelopeVisitor<'observation> {
    observation: &'observation mut EnvelopeObservation,
}

impl<'de> Visitor<'de> for EnvelopeVisitor<'_> {
    type Value = ();

    fn expecting(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str("top-level playlist state JSON object")
    }

    fn visit_map<Access>(self, mut map: Access) -> Result<Self::Value, Access::Error>
    where
        Access: MapAccess<'de>,
    {
        while let Some(key) = map.next_key::<String>()? {
            if key.len() > MAX_STATE_ENVELOPE_TOKEN_BYTES {
                return Err(de::Error::custom(
                    "playlist state envelope key is too large",
                ));
            }
            if key == "schema_version" {
                self.observation.schema_version_occurrences += 1;
                if self.observation.schema_version_occurrences == 1 {
                    match map.next_value::<IntegerSchemaVersion>() {
                        Ok(IntegerSchemaVersion(version)) => {
                            self.observation.schema_version = Some(version);
                        }
                        Err(error) => {
                            self.observation.non_integer_schema_version = true;
                            return Err(error);
                        }
                    }
                } else {
                    map.next_value_seed(BoundedIgnoredValue::root())?;
                }
            } else {
                map.next_value_seed(BoundedIgnoredValue::root())?;
            }
        }
        Ok(())
    }
}

struct IntegerSchemaVersion(u64);

impl<'de> Deserialize<'de> for IntegerSchemaVersion {
    fn deserialize<DeserializerType>(
        deserializer: DeserializerType,
    ) -> Result<Self, DeserializerType::Error>
    where
        DeserializerType: Deserializer<'de>,
    {
        deserializer.deserialize_u64(IntegerSchemaVersionVisitor)
    }
}

struct IntegerSchemaVersionVisitor;

impl Visitor<'_> for IntegerSchemaVersionVisitor {
    type Value = IntegerSchemaVersion;

    fn expecting(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str("non-negative integer schema_version")
    }

    fn visit_u64<Error>(self, value: u64) -> Result<Self::Value, Error> {
        Ok(IntegerSchemaVersion(value))
    }
}

/// Streaming skip, который сохраняет explicit token/depth limits без Value tree.
#[derive(Clone, Copy)]
struct BoundedIgnoredValue {
    depth: usize,
}

impl BoundedIgnoredValue {
    const fn root() -> Self {
        Self { depth: 0 }
    }

    fn child<Error: de::Error>(self) -> Result<Self, Error> {
        if self.depth >= MAX_STATE_ENVELOPE_NESTING_DEPTH {
            return Err(Error::custom("playlist state envelope nesting is too deep"));
        }
        Ok(Self {
            depth: self.depth + 1,
        })
    }
}

impl<'de> DeserializeSeed<'de> for BoundedIgnoredValue {
    type Value = ();

    fn deserialize<DeserializerType>(
        self,
        deserializer: DeserializerType,
    ) -> Result<Self::Value, DeserializerType::Error>
    where
        DeserializerType: Deserializer<'de>,
    {
        deserializer.deserialize_any(BoundedIgnoredVisitor { state: self })
    }
}

struct BoundedIgnoredVisitor {
    state: BoundedIgnoredValue,
}

impl<'de> Visitor<'de> for BoundedIgnoredVisitor {
    type Value = ();

    fn expecting(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str("bounded JSON value")
    }

    fn visit_bool<Error>(self, _value: bool) -> Result<Self::Value, Error> {
        Ok(())
    }

    fn visit_i64<Error>(self, _value: i64) -> Result<Self::Value, Error> {
        Ok(())
    }

    fn visit_u64<Error>(self, _value: u64) -> Result<Self::Value, Error> {
        Ok(())
    }

    fn visit_f64<Error>(self, _value: f64) -> Result<Self::Value, Error> {
        Ok(())
    }

    fn visit_str<Error>(self, value: &str) -> Result<Self::Value, Error>
    where
        Error: de::Error,
    {
        if value.len() > MAX_STATE_ENVELOPE_TOKEN_BYTES {
            return Err(Error::custom("playlist state envelope string is too large"));
        }
        Ok(())
    }

    fn visit_string<Error>(self, value: String) -> Result<Self::Value, Error>
    where
        Error: de::Error,
    {
        self.visit_str(&value)
    }

    fn visit_none<Error>(self) -> Result<Self::Value, Error> {
        Ok(())
    }

    fn visit_unit<Error>(self) -> Result<Self::Value, Error> {
        Ok(())
    }

    fn visit_some<DeserializerType>(
        self,
        deserializer: DeserializerType,
    ) -> Result<Self::Value, DeserializerType::Error>
    where
        DeserializerType: Deserializer<'de>,
    {
        self.state.deserialize(deserializer)
    }

    fn visit_seq<Access>(self, mut sequence: Access) -> Result<Self::Value, Access::Error>
    where
        Access: SeqAccess<'de>,
    {
        let child = self.state.child()?;
        while sequence.next_element_seed(child)?.is_some() {}
        Ok(())
    }

    fn visit_map<Access>(self, mut map: Access) -> Result<Self::Value, Access::Error>
    where
        Access: MapAccess<'de>,
    {
        let child = self.state.child()?;
        while let Some(key) = map.next_key::<String>()? {
            if key.len() > MAX_STATE_ENVELOPE_TOKEN_BYTES {
                return Err(de::Error::custom(
                    "playlist state envelope key is too large",
                ));
            }
            map.next_value_seed(child)?;
        }
        Ok(())
    }
}

/// Полностью сканирует один top-level object и не останавливается на первом key.
pub(crate) fn scan_envelope(
    reader: impl Read,
    maximum_bytes: u64,
) -> Result<EnvelopeProof, ProtectedStateCause> {
    let mut bounded_reader = BudgetedCaptureReader::new(reader, maximum_bytes);
    let mut observation = EnvelopeObservation::default();
    let parse_result = {
        let mut deserializer = serde_json::Deserializer::from_reader(&mut bounded_reader);
        let map_result = deserializer.deserialize_map(EnvelopeVisitor {
            observation: &mut observation,
        });
        map_result.and_then(|()| deserializer.end())
    };

    if bounded_reader.exhausted {
        return Err(ProtectedStateCause::EnvelopeBudgetExhausted);
    }
    if observation.schema_version_occurrences > 1 {
        return Err(ProtectedStateCause::DuplicateSchemaVersion);
    }
    if let Err(error) = parse_result {
        if observation.non_integer_schema_version
            && error.classify() == serde_json::error::Category::Data
        {
            return Err(ProtectedStateCause::NonIntegerSchemaVersion);
        }
        return Err(ProtectedStateCause::InvalidEnvelope);
    }
    let schema_version = observation
        .schema_version
        .ok_or(ProtectedStateCause::MissingSchemaVersion)?;
    let (inspected_bytes, content_sha256) = bounded_reader.finish();
    Ok(EnvelopeProof {
        schema_version,
        inspected_bytes,
        content_sha256,
    })
}
