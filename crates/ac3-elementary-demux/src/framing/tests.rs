//! Шаги нарезки на настоящих кадрах ffmpeg (`ac3-syncframe` фикстуры).

use ac3_syncframe::StreamContinuation;

use super::{FramingStep, SkipReason, SyncState, next_framing_step};

/// 4 кадра AC-3 stereo 48 кГц по 768 байт.
const STEREO_AC3: &[u8] = include_bytes!(concat!(
    env!("CARGO_MANIFEST_DIR"),
    "/../ac3-syncframe/tests/fixtures/stereo-48k-192k.ac3"
));

/// 4 кадра E-AC-3 5.1 48 кГц по 1792 байта.
const SURROUND_EAC3: &[u8] = include_bytes!(concat!(
    env!("CARGO_MANIFEST_DIR"),
    "/../ac3-syncframe/tests/fixtures/5p1-48k-448k.eac3"
));

const AC3_FRAME_BYTES: usize = 768;
const EAC3_FRAME_BYTES: usize = 1792;

fn unit_at(step: FramingStep) -> (usize, usize) {
    match step {
        FramingStep::AccessUnit {
            skipped_bytes,
            unit,
        } => (skipped_bytes, unit.total_length_bytes()),
        other => panic!("expected access unit, got {other:?}"),
    }
}

#[test]
fn locked_stream_yields_whole_frames() {
    let step = next_framing_step(
        STEREO_AC3,
        SyncState::Locked,
        StreamContinuation::MoreBytesMayFollow,
    );
    assert_eq!(unit_at(step), (0, AC3_FRAME_BYTES));
}

#[test]
fn searching_skips_garbage_before_verified_frame() {
    let mut bytes = vec![0x0B, 0x77, 0x13, 0x00, 0x00];
    bytes.extend_from_slice(&[0xA5; 200]);
    bytes.extend_from_slice(STEREO_AC3);
    let step = next_framing_step(
        &bytes,
        SyncState::Searching,
        StreamContinuation::MoreBytesMayFollow,
    );
    assert_eq!(unit_at(step), (205, AC3_FRAME_BYTES));
    assert_eq!(
        step.sync_state_after(SyncState::Searching),
        SyncState::Locked
    );
}

/// Синхрослово с корректным заголовком, но без следующего кадра за ним — ложное.
#[test]
fn unverified_sync_word_is_not_accepted() {
    let mut bytes = STEREO_AC3[..AC3_FRAME_BYTES].to_vec();
    bytes.extend_from_slice(&[0x11; 40]);
    bytes.extend_from_slice(STEREO_AC3);
    let step = next_framing_step(
        &bytes,
        SyncState::Searching,
        StreamContinuation::MoreBytesMayFollow,
    );
    assert_eq!(unit_at(step), (AC3_FRAME_BYTES + 40, AC3_FRAME_BYTES));
}

#[test]
fn eac3_unit_waits_for_next_header_until_end_of_stream() {
    let last = &SURROUND_EAC3[3 * EAC3_FRAME_BYTES..];
    assert_eq!(
        next_framing_step(
            last,
            SyncState::Locked,
            StreamContinuation::MoreBytesMayFollow
        ),
        FramingStep::NeedMoreData {
            discardable_bytes: 0
        }
    );
    let step = next_framing_step(last, SyncState::Locked, StreamContinuation::EndOfStream);
    assert_eq!(unit_at(step), (0, EAC3_FRAME_BYTES));
}

#[test]
fn corrupted_boundary_in_locked_state_loses_sync() {
    let mut bytes = STEREO_AC3.to_vec();
    bytes[0] = 0x00;
    let step = next_framing_step(
        &bytes,
        SyncState::Locked,
        StreamContinuation::MoreBytesMayFollow,
    );
    assert_eq!(
        step,
        FramingStep::Skip {
            bytes: 1,
            reason: SkipReason::LostSync
        }
    );
    assert_eq!(
        step.sync_state_after(SyncState::Locked),
        SyncState::Searching
    );
}

#[test]
fn truncated_last_frame_is_reported_only_at_end_of_stream() {
    let truncated = &STEREO_AC3[..AC3_FRAME_BYTES + 100];
    let tail = &truncated[AC3_FRAME_BYTES..];
    assert_eq!(
        next_framing_step(
            tail,
            SyncState::Locked,
            StreamContinuation::MoreBytesMayFollow
        ),
        FramingStep::NeedMoreData {
            discardable_bytes: 0
        }
    );
    assert_eq!(
        next_framing_step(tail, SyncState::Locked, StreamContinuation::EndOfStream),
        FramingStep::TruncatedTail { offset: 0 }
    );
}

#[test]
fn pure_garbage_is_discardable() {
    let garbage = [0x42_u8; 64];
    assert_eq!(
        next_framing_step(
            &garbage,
            SyncState::Searching,
            StreamContinuation::MoreBytesMayFollow
        ),
        FramingStep::NeedMoreData {
            discardable_bytes: 64
        }
    );
}
