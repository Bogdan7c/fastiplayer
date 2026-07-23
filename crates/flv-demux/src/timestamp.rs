/// Разворачивает независимый FLV u32 millisecond clock в монотонный u64 domain.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub(crate) struct MillisecondTimestampUnwrapper {
    previous_raw: Option<u32>,
    epoch: u64,
}

impl MillisecondTimestampUnwrapper {
    /// Разворачивает wrap только при переходе через половину u32 domain-а.
    pub(crate) fn unwrap(&mut self, raw: u32) -> u64 {
        if let Some(previous_raw) = self.previous_raw
            && previous_raw > raw
            && previous_raw.wrapping_sub(raw) > (u32::MAX / 2)
        {
            self.epoch = self.epoch.saturating_add(1_u64 << 32);
        }
        self.previous_raw = Some(raw);
        self.epoch.saturating_add(u64::from(raw))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn rollover_is_distinct_from_small_backward_discontinuity() {
        let mut clock = MillisecondTimestampUnwrapper::default();
        assert_eq!(clock.unwrap(u32::MAX - 2), u64::from(u32::MAX - 2));
        assert_eq!(clock.unwrap(3), (1_u64 << 32) + 3);
        assert_eq!(clock.unwrap(2), (1_u64 << 32) + 2);
    }
}
