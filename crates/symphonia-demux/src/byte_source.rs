use std::io::{self, Read, Seek, SeekFrom};
use std::sync::{Arc, Mutex};

use source_core::{ByteSource, CancellationToken, Seekability, SourceError};
use symphonia::core::io::MediaSource;

use crate::DemuxError;

/// First-failure observer сохраняет concrete source error, которую Symphonia probe
/// может проглотить и заменить misleading `no suitable format reader found`.
#[derive(Clone, Default)]
pub(crate) struct ByteSourceFailureObserver {
    /// Состояние одной eager-probe фазы разделяется с adapter-ом.
    state: Arc<Mutex<ByteSourceFailureObservationState>>,
}

/// Observer обязан отключиться после probe и не менять runtime error semantics.
#[derive(Default)]
struct ByteSourceFailureObservationState {
    /// Ошибка хранится как `io::Error`, чтобы сохранить original `SourceError` в source chain.
    first_failure: Option<io::Error>,

    /// После завершения probe adapter возвращает обычную typed source chain.
    probe_finished: bool,
}

impl ByteSourceFailureObserver {
    /// Публикует только первую ошибку и возвращает отдельную копию для Symphonia.
    fn observe(&self, error: SourceError) -> io::Error {
        let mut state = self
            .state
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        if state.probe_finished {
            return source_error_to_io_error(error);
        }

        let error_kind = source_error_io_kind(&error);
        let error_message = error.to_string();
        let stored_error = source_error_to_io_error(error);
        if state.first_failure.is_none() {
            state.first_failure = Some(stored_error);
        }
        io::Error::new(error_kind, error_message)
    }

    /// Забирает concrete source failure после неудачного eager probe-а.
    pub(crate) fn take_demux_error(&self) -> Option<DemuxError> {
        let mut state = self
            .state
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        state.probe_finished = true;
        state.first_failure.take().map(DemuxError::Io)
    }

    /// Завершает успешный probe и восстанавливает обычное runtime error mapping.
    pub(crate) fn finish_probe_success(&self) {
        let mut state = self
            .state
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        state.probe_finished = true;
        state.first_failure = None;
    }
}

/// Symphonia-compatible wrapper поверх нейтрального `source-core::ByteSource`.
///
/// `MediaSourceStream` требует `Read + Seek + Send + Sync`, а `ByteSource`
/// намеренно описывает только byte contract. Mutex делает wrapper `Sync` без
/// ужесточения public contract-а `source-core`.
pub(crate) struct ByteSourceMediaSource {
    /// Внутренний source, который может быть local/cache/http adapter-ом.
    source: Mutex<Box<dyn ByteSource>>,

    /// Кэшированная seekability, чтобы Symphonia не делала дорогой probe повторно.
    seekability: Seekability,

    /// Кэшированная длина source-а для `SeekFrom::End` и demux metadata.
    content_length: Option<u64>,

    /// Token отмены будущих blocking reads.
    cancellation: CancellationToken,

    /// Observer есть только у generic eager-probe path-а, который реально маскирует I/O.
    failure_observer: Option<ByteSourceFailureObserver>,
}

impl ByteSourceMediaSource {
    /// Создаёт Symphonia source из уже настроенного byte source-а.
    pub(crate) fn new(source: Box<dyn ByteSource>) -> Self {
        Self::with_failure_observer(source, None)
    }

    /// Создаёт adapter и handle для восстановления source error после eager probe-а.
    pub(crate) fn new_observed(source: Box<dyn ByteSource>) -> (Self, ByteSourceFailureObserver) {
        let failure_observer = ByteSourceFailureObserver::default();
        let media_source = Self::with_failure_observer(source, Some(failure_observer.clone()));
        (media_source, failure_observer)
    }

    /// Собирает adapter с caller-owned observer handle без дублирования snapshots.
    fn with_failure_observer(
        source: Box<dyn ByteSource>,
        failure_observer: Option<ByteSourceFailureObserver>,
    ) -> Self {
        let seekability = source.seekability();
        let content_length = source.content_length();

        Self {
            source: Mutex::new(source),
            seekability,
            content_length,
            cancellation: CancellationToken::never_cancelled(),
            failure_observer,
        }
    }

    /// Сохраняет старый typed I/O chain вне generic probe observer path-а.
    fn map_source_error(&self, error: SourceError) -> io::Error {
        match &self.failure_observer {
            Some(failure_observer) => failure_observer.observe(error),
            None => source_error_to_io_error(error),
        }
    }
}

impl Read for ByteSourceMediaSource {
    fn read(&mut self, output: &mut [u8]) -> io::Result<usize> {
        let mut source = self
            .source
            .lock()
            .map_err(|_| io::Error::other("byte source mutex poisoned"))?;

        source
            .read(output, &self.cancellation)
            .map_err(|error| self.map_source_error(error))
    }
}

impl Seek for ByteSourceMediaSource {
    fn seek(&mut self, position: SeekFrom) -> io::Result<u64> {
        let mut source = self
            .source
            .lock()
            .map_err(|_| io::Error::other("byte source mutex poisoned"))?;
        let current_position = source.position();
        let target_position =
            resolve_seek_position(position, current_position, self.content_length)?;

        source
            .seek(target_position)
            .map_err(|error| self.map_source_error(error))?;

        Ok(target_position)
    }
}

impl MediaSource for ByteSourceMediaSource {
    fn is_seekable(&self) -> bool {
        self.seekability.is_seekable()
    }

    fn byte_len(&self) -> Option<u64> {
        self.content_length
    }
}

/// Разрешает `SeekFrom` в абсолютную позицию без underflow/overflow.
fn resolve_seek_position(
    position: SeekFrom,
    current_position: u64,
    content_length: Option<u64>,
) -> io::Result<u64> {
    let target_position = match position {
        SeekFrom::Start(offset) => i128::from(offset),
        SeekFrom::Current(delta) => i128::from(current_position) + i128::from(delta),
        SeekFrom::End(delta) => {
            let length = content_length.ok_or_else(|| {
                io::Error::new(
                    io::ErrorKind::InvalidInput,
                    "cannot seek from end without known source length",
                )
            })?;
            i128::from(length) + i128::from(delta)
        }
    };

    u64::try_from(target_position).map_err(|_| {
        io::Error::new(
            io::ErrorKind::InvalidInput,
            "seek resolved outside valid byte range",
        )
    })
}

/// Сохраняет source-layer ошибку внутри обычной `io::Error` для Symphonia.
fn source_error_to_io_error(error: SourceError) -> io::Error {
    io::Error::new(source_error_io_kind(&error), error)
}

/// Сохраняет прежнюю mapping semantics отдельно от ownership конкретной ошибки.
fn source_error_io_kind(error: &SourceError) -> io::ErrorKind {
    match error {
        SourceError::Cancelled => io::ErrorKind::Interrupted,
        SourceError::NotSeekable { .. } | SourceError::HttpRangeUnsupported { .. } => {
            io::ErrorKind::Unsupported
        }
        _ => io::ErrorKind::Other,
    }
}

#[cfg(test)]
mod tests {
    use std::io::{Read, Seek, SeekFrom};

    use source_core::{
        ByteSource, CancellationToken, Seekability, SourceError, SourceFingerprint, SourceResult,
        SourceValidators,
    };

    use super::{ByteSourceFailureObserver, ByteSourceMediaSource};

    struct MemoryByteSource {
        bytes: Vec<u8>,
        position: u64,
    }

    impl MemoryByteSource {
        fn new(bytes: &[u8]) -> Self {
            Self {
                bytes: bytes.to_vec(),
                position: 0,
            }
        }
    }

    impl ByteSource for MemoryByteSource {
        fn read(
            &mut self,
            output: &mut [u8],
            _cancellation: &CancellationToken,
        ) -> SourceResult<usize> {
            let start = usize::try_from(self.position).unwrap_or(usize::MAX);
            if start >= self.bytes.len() {
                return Ok(0);
            }

            let available = &self.bytes[start..];
            let bytes_to_copy = available.len().min(output.len());
            output[..bytes_to_copy].copy_from_slice(&available[..bytes_to_copy]);
            self.position = self.position.saturating_add(bytes_to_copy as u64);
            Ok(bytes_to_copy)
        }

        fn seek(&mut self, offset: u64) -> SourceResult<()> {
            self.position = offset;
            Ok(())
        }

        fn position(&self) -> u64 {
            self.position
        }

        fn seekability(&self) -> Seekability {
            Seekability::Seekable
        }

        fn validators(&self) -> SourceValidators {
            SourceValidators::default()
        }

        fn content_length(&self) -> Option<u64> {
            Some(self.bytes.len() as u64)
        }

        fn fingerprint(&self) -> SourceFingerprint {
            SourceFingerprint::new("memory")
        }
    }

    #[test]
    fn byte_source_media_source_reads_and_seeks() {
        let source = MemoryByteSource::new(b"abcdef");
        let mut media_source = ByteSourceMediaSource::new(Box::new(source));
        let mut output = [0_u8; 2];

        media_source.read_exact(&mut output).expect("initial read");
        assert_eq!(&output, b"ab");

        let position = media_source
            .seek(SeekFrom::End(-2))
            .expect("seek from known end");
        assert_eq!(position, 4);

        media_source
            .read_exact(&mut output)
            .expect("read after seek");
        assert_eq!(&output, b"ef");
    }

    #[test]
    fn byte_source_media_source_rejects_negative_seek() {
        let source = MemoryByteSource::new(b"abcdef");
        let mut media_source = ByteSourceMediaSource::new(Box::new(source));

        let error = media_source
            .seek(SeekFrom::Current(-1))
            .expect_err("negative absolute seek is invalid");

        assert_eq!(error.kind(), std::io::ErrorKind::InvalidInput);
    }

    #[test]
    fn byte_source_media_source_reports_cached_seekability() {
        let source = MemoryByteSource::new(b"abcdef");
        let media_source = ByteSourceMediaSource::new(Box::new(source));

        assert!(symphonia::core::io::MediaSource::is_seekable(&media_source));
        assert_eq!(
            symphonia::core::io::MediaSource::byte_len(&media_source),
            Some(6)
        );
    }

    /// Успешный probe отключает observer и не обедняет runtime source error chain.
    #[test]
    fn failure_observer_preserves_typed_runtime_error_after_probe() {
        let observer = ByteSourceFailureObserver::default();
        observer.finish_probe_success();

        let runtime_error = observer.observe(SourceError::UnexpectedEof {
            offset: 4,
            expected_bytes: 8,
            actual_bytes: 2,
        });

        assert!(matches!(
            runtime_error
                .get_ref()
                .and_then(|source| source.downcast_ref::<SourceError>()),
            Some(SourceError::UnexpectedEof {
                offset: 4,
                expected_bytes: 8,
                actual_bytes: 2,
            })
        ));
    }
}
