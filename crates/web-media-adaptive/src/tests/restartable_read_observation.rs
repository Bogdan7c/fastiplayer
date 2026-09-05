//! Read-only наблюдение принадлежит владельцу phase, а не HTTP fixture.

use super::*;

impl AdaptiveRestartableReadInterruption {
    /// Тест вызывает это только для stalled body: read не завершится до явной отмены.
    /// Deadline ограничивает зависший тест; успешный исход определяется phase, не временем.
    pub(crate) fn wait_until_network_read_is_active(&self, timeout: std::time::Duration) {
        let deadline = std::time::Instant::now() + timeout;
        loop {
            let state = self.shared.current_attempt_state.load(Ordering::Acquire);
            if attempt_phase(state) == ATTEMPT_PHASE_READING {
                return;
            }
            assert!(
                std::time::Instant::now() < deadline,
                "network read did not become active"
            );
            std::thread::yield_now();
        }
    }
}
