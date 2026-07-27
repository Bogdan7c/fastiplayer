/// Backend-local recovery после потери H.264 reference chain.
#[derive(Debug, Default)]
pub(super) struct H264DecodeRecovery {
    awaiting_keyframe: bool,
}

impl H264DecodeRecovery {
    /// Вооружает recovery после успешного flush повреждённого decoder state.
    pub(super) fn begin(&mut self) {
        self.awaiting_keyframe = true;
    }

    /// Новый stream/explicit flush начинает с пустого DPB только для H.264.
    pub(super) fn reset_for_stream(&mut self, requires_keyframe: bool) {
        self.awaiting_keyframe = requires_keyframe;
    }

    /// Inter-frame после recovery flush нельзя снова подавать в пустой DPB.
    pub(super) const fn should_drop(&self, is_keyframe: bool) -> bool {
        self.awaiting_keyframe && !is_keyframe
    }

    /// Только действительно принятый keyframe восстанавливает reference chain.
    pub(super) fn note_packet_accepted(&mut self, is_keyframe: bool) -> bool {
        if self.awaiting_keyframe && is_keyframe {
            self.awaiting_keyframe = false;
            return true;
        }
        false
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn recovery_drops_interframes_until_an_accepted_keyframe() {
        let mut recovery = H264DecodeRecovery::default();
        assert!(!recovery.should_drop(false));

        recovery.begin();
        assert!(recovery.should_drop(false));
        assert!(!recovery.note_packet_accepted(false));
        assert!(recovery.should_drop(false));

        assert!(!recovery.should_drop(true));
        assert!(recovery.note_packet_accepted(true));
        assert!(!recovery.should_drop(false));
    }

    #[test]
    fn stream_lifecycle_requires_keyframe_only_when_requested() {
        let mut recovery = H264DecodeRecovery::default();
        recovery.reset_for_stream(true);
        assert!(recovery.should_drop(false));

        recovery.reset_for_stream(false);

        assert!(!recovery.should_drop(false));
        assert!(!recovery.note_packet_accepted(true));
    }
}
