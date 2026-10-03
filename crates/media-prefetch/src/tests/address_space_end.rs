//! Источник, который отдаёт байты за пределом `u64` offset-ов.
//!
//! Такие байты нельзя адресовать ни read-ом, ни seek-ом. Worker обязан считать
//! конец адресного пространства EOF источника: foreground получает ровно
//! адресуемые байты, затем чистый EOF, и при этом ничего не паникует.

use source_core::{
    ByteSource, CancellationToken, Seekability, SourceFingerprint, SourceResult, SourceValidators,
};

use crate::{PrefetchConfig, PrefetchingByteSource};

/// Позиция источника: до конца адресного пространства остаётся ровно 2 байта.
const START_NEAR_ADDRESS_SPACE_END: u64 = u64::MAX - 2;

/// Fake source, стоящий у конца адресного пространства и не знающий собственного EOF.
struct AddressSpaceEndSource {
    /// Текущая позиция; растёт saturating, как у реального счётчика offset-а.
    position: u64,
}

impl ByteSource for AddressSpaceEndSource {
    fn read(
        &mut self,
        output: &mut [u8],
        _cancellation: &CancellationToken,
    ) -> SourceResult<usize> {
        // Источник всегда готов отдать данные: свой конец он «не замечает».
        output.fill(0xab);
        self.position = self.position.saturating_add(output.len() as u64);
        Ok(output.len())
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
        None
    }

    fn fingerprint(&self) -> SourceFingerprint {
        SourceFingerprint::new("address-space-end-source")
    }
}

#[test]
fn bytes_beyond_u64_offsets_end_the_stream_as_clean_eof() {
    let inner = AddressSpaceEndSource {
        position: START_NEAR_ADDRESS_SPACE_END,
    };
    // initial chunk 8 байт, обычный chunk 16 байт, окно 48 байт.
    let config = PrefetchConfig::new(8, 16, 48).expect("valid prefetch config");
    let mut source =
        PrefetchingByteSource::new(Box::new(inner), config).expect("prefetch worker starts");
    let token = CancellationToken::never_cancelled();

    // Первый chunk (8 байт) не помещается в оставшиеся 2 адресуемых offset-а.
    let mut output = [0; 8];
    let first_read = source
        .read(&mut output, &token)
        .expect("addressable prefix must be readable");
    assert_eq!(
        first_read, 0,
        "chunk за пределом u64 не публикуется частично"
    );
    // Повторное чтение остаётся стабильным EOF, а не ошибкой или зависанием.
    assert_eq!(source.read(&mut output, &token).expect("EOF is stable"), 0);
    assert_eq!(source.position(), START_NEAR_ADDRESS_SPACE_END);
    assert_eq!(source.diagnostics().bytes_prefetched, 0);
}
