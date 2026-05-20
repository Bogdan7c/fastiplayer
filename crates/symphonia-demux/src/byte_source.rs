use std::io::{self, Read, Seek, SeekFrom};
use std::sync::Mutex;

use source_core::{ByteSource, CancellationToken, Seekability, SourceError};
use symphonia::core::io::MediaSource;

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
}

impl ByteSourceMediaSource {
    /// Создаёт Symphonia source из уже настроенного byte source-а.
    pub(crate) fn new(source: Box<dyn ByteSource>) -> Self {
        let seekability = source.seekability();
        let content_length = source.content_length();

        Self {
            source: Mutex::new(source),
            seekability,
            content_length,
            cancellation: CancellationToken::never_cancelled(),
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
            .map_err(source_error_to_io_error)
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
            .map_err(source_error_to_io_error)?;

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
    match error {
        SourceError::Cancelled => io::Error::new(io::ErrorKind::Interrupted, error),
        SourceError::NotSeekable { .. } | SourceError::HttpRangeUnsupported { .. } => {
            io::Error::new(io::ErrorKind::Unsupported, error)
        }
        other_error => io::Error::other(other_error),
    }
}

#[cfg(test)]
mod tests {
    use std::io::{Read, Seek, SeekFrom};

    use source_core::{
        ByteSource, CancellationToken, Seekability, SourceFingerprint, SourceResult,
        SourceValidators,
    };

    use super::ByteSourceMediaSource;

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
}
