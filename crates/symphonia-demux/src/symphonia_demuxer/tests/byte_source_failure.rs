//! Проверка сохранения concrete `ByteSource` failure через eager Symphonia probe.

use source_core::{
    ByteSource, CancellationToken, Seekability, SourceError, SourceFingerprint, SourceResult,
    SourceValidators,
};

use crate::{DemuxError, DemuxerOptions, SymphoniaDemuxer};

/// Source всегда падает до первого byte-а, как HTTP Range после terminal status-а.
struct FailingByteSource;

impl ByteSource for FailingByteSource {
    /// Возвращает typed source error, которую upstream probe не имеет права маскировать.
    fn read(
        &mut self,
        output: &mut [u8],
        _cancellation: &CancellationToken,
    ) -> SourceResult<usize> {
        Err(SourceError::UnexpectedEof {
            offset: 0,
            expected_bytes: output.len(),
            actual_bytes: 0,
        })
    }

    /// Seek остаётся доступным, чтобы тест проверял именно read failure.
    fn seek(&mut self, _offset: u64) -> SourceResult<()> {
        Ok(())
    }

    /// Source ни разу не отдал byte, поэтому позиция остаётся нулевой.
    fn position(&self) -> u64 {
        0
    }

    /// Factory получает обычный seekable byte-source capability.
    fn seekability(&self) -> Seekability {
        Seekability::Seekable
    }

    /// Validators не участвуют в demux probe.
    fn validators(&self) -> SourceValidators {
        SourceValidators::default()
    }

    /// Ненулевая длина исключает честный EOF до первого read-а.
    fn content_length(&self) -> Option<u64> {
        Some(16)
    }

    /// Stable fake fingerprint не раскрывает request material.
    fn fingerprint(&self) -> SourceFingerprint {
        SourceFingerprint::new("failing-byte-source")
    }
}

/// Generic open возвращает original SourceError chain, а не ложный unsupported format.
#[test]
fn eager_probe_preserves_first_byte_source_failure() {
    let open_result = SymphoniaDemuxer::from_byte_source_with_options(
        FailingByteSource,
        "ogg",
        "failure-observer-test",
        DemuxerOptions::default(),
    );
    let error = match open_result {
        Ok(_) => panic!("failing source не должен открыться как Ogg"),
        Err(error) => error,
    };

    let DemuxError::Io(io_error) = error else {
        panic!("ожидалась concrete I/O error, получено: {error}");
    };
    assert!(matches!(
        io_error
            .get_ref()
            .and_then(|source| source.downcast_ref::<SourceError>()),
        Some(SourceError::UnexpectedEof {
            offset: 0,
            actual_bytes: 0,
            ..
        })
    ));
    assert!(!io_error.to_string().contains("no suitable format reader"));
}
