// Интеграционный тест целиком является тестовым кодом: unwrap/expect/panic
// здесь работают как assertions. Production-политика паник сюда не относится.
#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::panic,
    reason = "integration test crate: panics are test assertions"
)]

use std::time::Duration;

use bytes::Bytes;
use media_core::{Packet, TrackId, TrackKind};

/// Known container evidence остаётся различимым от `Unknown` после packet handoff.
#[test]
fn packet_preserves_both_known_keyframe_classifications() {
    for (is_keyframe, expected) in [(true, Some(true)), (false, Some(false))] {
        let packet = Packet::new_unbounded(
            TrackId::new(7),
            TrackKind::Video,
            Duration::from_millis(42),
            None,
            is_keyframe,
            Bytes::from_static(b"video"),
        );

        assert_eq!(packet.keyframe.as_known_bool(), expected);
    }
}
