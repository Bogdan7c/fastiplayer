//! Concurrency regression для seek-а внутрь уже выполняемого prefetch request-а.

use super::*;

/// Forward seek переиспользует active range без cancellation и duplicate refetch-а.
#[test]
fn forward_seek_inside_active_fetch_coalesces_without_cancel_or_refetch() {
    let bytes = sample_bytes(96);
    let read_delay = Duration::from_millis(80);
    let (inner, handle) = FakeByteSource::seekable(bytes.clone());
    let inner = inner.with_read_delay(read_delay);
    let mut source = start_test_source(Box::new(inner), test_config(8, 16, 48));

    let active_fetch = wait_for_read_offset(&handle, 8, Duration::from_millis(500));
    assert_eq!(active_fetch.requested_len, 16);
    source
        .seek(20)
        .expect("seek внутрь active fetch должен изменить только logical cursor");

    let mut output = [0_u8; 4];
    let bytes_read = source
        .read(&mut output, &token())
        .expect("foreground read должен дождаться active fetch");

    assert_eq!(bytes_read, output.len());
    assert_eq!(output, bytes[20..24]);
    assert_eq!(source.diagnostics().refetches, 0);
    assert_eq!(source.diagnostics().cancelled_fetches, 0);
    assert!(!handle.seek_offsets().contains(&20));
    assert!(
        !handle
            .read_records()
            .iter()
            .any(|record| record.offset == 20)
    );
}

/// Active fetch не расширяет публичную seekability источника.
#[test]
fn non_seekable_source_rejects_forward_seek_inside_active_fetch() {
    let bytes = sample_bytes(96);
    let read_delay = Duration::from_millis(80);
    let seekability = Seekability::NotSeekable {
        reason: NotSeekableReason::Unknown,
    };
    let (inner, handle) = FakeByteSource::new(bytes, seekability);
    let inner = inner.with_read_delay(read_delay);
    let mut source = start_test_source(Box::new(inner), test_config(8, 16, 48));

    let active_fetch = wait_for_read_offset(&handle, 8, Duration::from_millis(500));
    assert_eq!(active_fetch.requested_len, 16);
    let error = source
        .seek(20)
        .expect_err("active fetch не должен обходить NotSeekable contract");

    assert!(matches!(error, SourceError::NotSeekable { .. }));
    assert_eq!(source.position(), 0);
    assert_eq!(source.diagnostics().refetches, 0);
    assert_eq!(source.diagnostics().cancelled_fetches, 0);
}
