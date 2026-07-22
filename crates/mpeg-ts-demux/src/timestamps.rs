/// Modulus MPEG 33-bit timestamp domain-а.
const TIMESTAMP_MODULUS: i64 = 1_i64 << 33;
/// Порог выбора соседней epoch при rollover.
const HALF_TIMESTAMP_MODULUS: i64 = TIMESTAMP_MODULUS / 2;

/// Stateful unwrap одного timestamp field; PTS и DTS используют разные instances.
#[derive(Debug, Default, Clone)]
pub(crate) struct TimestampUnwrapper {
    last_unwrapped: Option<i64>,
}

impl TimestampUnwrapper {
    /// Начинает bounded continuation scan в epoch последнего committed anchor-а.
    pub(crate) const fn from_unwrapped_reference(reference: i64) -> Self {
        Self {
            last_unwrapped: Some(reference),
        }
    }

    /// Переносит raw 33-bit значение в ближайшую непрерывную epoch.
    pub(crate) fn unwrap(&mut self, raw_timestamp: u64) -> i64 {
        let raw = (raw_timestamp & ((1_u64 << 33) - 1)) as i64;
        let unwrapped = if let Some(previous) = self.last_unwrapped {
            let epoch = previous.div_euclid(TIMESTAMP_MODULUS);
            let mut candidate = epoch * TIMESTAMP_MODULUS + raw;
            if candidate - previous > HALF_TIMESTAMP_MODULUS {
                candidate -= TIMESTAMP_MODULUS;
            } else if previous - candidate > HALF_TIMESTAMP_MODULUS {
                candidate += TIMESTAMP_MODULUS;
            }
            candidate
        } else {
            raw
        };
        self.last_unwrapped = Some(unwrapped);
        unwrapped
    }

    /// Новая discontinuity generation не наследует старую wrap epoch.
    pub(crate) fn reset(&mut self) {
        self.last_unwrapped = None;
    }
}
