use std::cmp::Ordering;

use crate::{HlsLineNumber, HlsParseError, HlsParseErrorKind, MediaSegment};

/// Проверяет RFC 8216 §4.3.3.1 exact decimal arithmetic без `float`.
pub(crate) fn validate_target_duration(
    target_duration: u64,
    segments: &[MediaSegment],
    segment_lines: &[HlsLineNumber],
) -> Result<(), HlsParseError> {
    for (index, segment) in segments.iter().enumerate() {
        if rounded_duration_exceeds_target(segment.duration.as_decimal_str(), target_duration) {
            return Err(HlsParseError::new(
                HlsParseErrorKind::InvalidRequiredStructure {
                    line: segment_lines[index],
                },
            ));
        }
    }
    Ok(())
}

/// RFC говорит «rounded to nearest integer»; ровно половина округляется вверх.
fn rounded_duration_exceeds_target(duration: &str, target_duration: u64) -> bool {
    let (whole, fraction) = duration
        .split_once('.')
        .map_or((duration, None), |(whole, fraction)| {
            (whole, Some(fraction))
        });
    let normalized_whole = whole.trim_start_matches('0');
    let normalized_whole = if normalized_whole.is_empty() {
        "0"
    } else {
        normalized_whole
    };
    let target = target_duration.to_string();
    match compare_decimal_integers(normalized_whole, &target) {
        Ordering::Greater => true,
        Ordering::Less => false,
        Ordering::Equal => fraction.is_some_and(|fraction| {
            fraction
                .as_bytes()
                .first()
                .is_some_and(|first_digit| *first_digit >= b'5')
        }),
    }
}

fn compare_decimal_integers(left: &str, right: &str) -> Ordering {
    left.len()
        .cmp(&right.len())
        .then_with(|| left.as_bytes().cmp(right.as_bytes()))
}

#[cfg(test)]
mod tests {
    use super::rounded_duration_exceeds_target;

    #[test]
    fn exact_decimal_rounding_has_no_float_or_overflow_boundary() {
        assert!(!rounded_duration_exceeds_target("5.499999999999999999", 5));
        assert!(rounded_duration_exceeds_target("5.5", 5));
        assert!(!rounded_duration_exceeds_target("00004.999", 5));
        assert!(rounded_duration_exceeds_target(
            "18446744073709551615.5",
            u64::MAX
        ));
        assert!(rounded_duration_exceeds_target(
            "184467440737095516160.0",
            u64::MAX
        ));
    }
}
