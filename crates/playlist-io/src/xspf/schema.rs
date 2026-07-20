//! Compact XSPF child-sequence и integer schema primitives.

use media_core::{MediaDuration, TrackNumber};

use super::error::{XspfParseError, XspfParseErrorKind};
use super::uri::trim_xml_whitespace;

/// XSD nonNegativeInteger parser с explicit positive policy.
pub(super) fn parse_positive_u32(value: &str) -> Result<u32, XspfParseError> {
    let parsed = parse_non_negative_u64(value)?;
    u32::try_from(parsed)
        .ok()
        .filter(|value| *value > 0)
        .ok_or_else(|| XspfParseError::new(XspfParseErrorKind::InvalidInteger))
}

/// XSPF trackNum обязан быть positive несмотря на base XSD lexical type.
pub(super) fn parse_track_number(value: &str) -> Result<TrackNumber, XspfParseError> {
    let parsed = parse_non_negative_u64(value)?;
    if parsed == 0 {
        return Err(XspfParseError::new(XspfParseErrorKind::InvalidInteger));
    }
    Ok(TrackNumber::new(parsed))
}

/// Duration milliseconds остаётся metadata hint.
pub(super) fn parse_duration_hint(value: &str) -> Result<MediaDuration, XspfParseError> {
    parse_non_negative_u64(value).map(MediaDuration::from_millis)
}

/// Bounded XSD nonNegativeInteger lexical subset: optional plus и ASCII digits.
fn parse_non_negative_u64(value: &str) -> Result<u64, XspfParseError> {
    let normalized = trim_xml_whitespace(value);
    let digits = normalized.strip_prefix('+').unwrap_or(normalized);
    if digits.is_empty() || !digits.bytes().all(|byte| byte.is_ascii_digit()) {
        return Err(XspfParseError::new(XspfParseErrorKind::InvalidInteger));
    }
    digits
        .parse::<u64>()
        .map_err(|_| XspfParseError::new(XspfParseErrorKind::InvalidInteger))
}

/// Cardinality управляет duplicate semantics одного rank-а.
#[derive(Clone, Copy)]
pub(super) enum ChildCardinality {
    /// Child допускается не более одного раза.
    Optional,
    /// Child допускается повторять contiguous в своём rank-е.
    Repeated,
}

/// Маленький state machine проверяет monotonic child ranks и duplicates.
#[derive(Default)]
pub(super) struct ChildSequence {
    /// Последний accepted rank.
    last_rank: Option<u8>,
    /// Optional ranks фиксируются compact bitset-ом без HashMap allocation.
    optional_seen: u64,
}

impl ChildSequence {
    /// Валидирует order/cardinality до side effects child parser-а.
    pub(super) fn accept(
        &mut self,
        rank: u8,
        cardinality: ChildCardinality,
    ) -> Result<(), XspfParseError> {
        if self.last_rank.is_some_and(|last_rank| rank < last_rank) {
            return Err(XspfParseError::new(XspfParseErrorKind::ChildOrderViolation));
        }
        if matches!(cardinality, ChildCardinality::Optional) {
            let rank_bit = 1u64
                .checked_shl(u32::from(rank))
                .ok_or_else(|| XspfParseError::new(XspfParseErrorKind::DuplicateChild))?;
            if self.optional_seen & rank_bit != 0 {
                return Err(XspfParseError::new(XspfParseErrorKind::DuplicateChild));
            }
            self.optional_seen |= rank_bit;
        }
        self.last_rank = Some(rank);
        Ok(())
    }
}
