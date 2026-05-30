use thiserror::Error;

/// Один мебибайт в bytes для читаемых defaults.
const MIB_BYTES: u64 = 1024 * 1024;

/// Первый prefetch fetch маленький, чтобы demuxer быстро получил заголовки контейнера.
const DEFAULT_INITIAL_CHUNK_BYTES: u64 = 64 * 1024;

/// Размер чанка, который будущий prefetch-worker будет читать за одну операцию.
const DEFAULT_CHUNK_BYTES: u64 = 8 * MIB_BYTES;

/// Размер RAM-окна, которое prefetch-слой старается держать впереди cursor-а.
const DEFAULT_WINDOW_BYTES: u64 = 256 * MIB_BYTES;

/// Настройки политики prefetch без привязки к конкретному `ByteSource`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct PrefetchConfig {
    /// Размер первого чтения после open/seek, с которого worker начинает slow-start.
    initial_chunk_bytes: u64,

    /// Размер одного последовательного чтения из source-а.
    chunk_bytes: u64,

    /// Целевой размер buffered window в RAM.
    window_bytes: u64,
}

impl PrefetchConfig {
    /// Создаёт config только из значений, которые сохраняют инварианты буфера.
    pub fn new(
        initial_chunk_bytes: u64,
        chunk_bytes: u64,
        window_bytes: u64,
    ) -> Result<Self, PrefetchConfigError> {
        if initial_chunk_bytes == 0 {
            return Err(PrefetchConfigError::ZeroInitialChunk);
        }

        if chunk_bytes == 0 {
            return Err(PrefetchConfigError::ZeroChunk);
        }

        if initial_chunk_bytes > chunk_bytes {
            return Err(PrefetchConfigError::InitialChunkLargerThanChunk {
                initial_chunk_bytes,
                chunk_bytes,
            });
        }

        if window_bytes < chunk_bytes {
            return Err(PrefetchConfigError::WindowSmallerThanChunk {
                chunk_bytes,
                window_bytes,
            });
        }

        Ok(Self {
            initial_chunk_bytes,
            chunk_bytes,
            window_bytes,
        })
    }

    /// Возвращает размер первого prefetch-чтения после open/seek в bytes.
    #[must_use]
    pub const fn initial_chunk_bytes(self) -> u64 {
        self.initial_chunk_bytes
    }

    /// Возвращает размер одного prefetch-чтения в bytes.
    #[must_use]
    pub const fn chunk_bytes(self) -> u64 {
        self.chunk_bytes
    }

    /// Возвращает целевой размер RAM-окна в bytes.
    #[must_use]
    pub const fn window_bytes(self) -> u64 {
        self.window_bytes
    }
}

impl Default for PrefetchConfig {
    fn default() -> Self {
        Self {
            initial_chunk_bytes: DEFAULT_INITIAL_CHUNK_BYTES,
            chunk_bytes: DEFAULT_CHUNK_BYTES,
            window_bytes: DEFAULT_WINDOW_BYTES,
        }
    }
}

/// Ошибки validation для `PrefetchConfig`.
#[derive(Debug, Error, PartialEq, Eq)]
pub enum PrefetchConfigError {
    /// Нулевой initial chunk не даёт worker-у опубликовать первый байт после open/seek.
    #[error("размер начального prefetch chunk должен быть больше нуля")]
    ZeroInitialChunk,

    /// Нулевой chunk не даёт prefetch-слою продвинуть source cursor.
    #[error("размер prefetch chunk должен быть больше нуля")]
    ZeroChunk,

    /// Начальный chunk не может быть больше потолка обычного chunk-а.
    #[error(
        "размер начального prefetch chunk ({initial_chunk_bytes} bytes) больше размера chunk ({chunk_bytes} bytes)"
    )]
    InitialChunkLargerThanChunk {
        /// Запрошенный размер первого chunk-а.
        initial_chunk_bytes: u64,

        /// Запрошенный максимальный размер chunk-а.
        chunk_bytes: u64,
    },

    /// Окно меньше чанка делает нормальное чтение одного чанка невозможным.
    #[error(
        "размер prefetch window ({window_bytes} bytes) меньше размера chunk ({chunk_bytes} bytes)"
    )]
    WindowSmallerThanChunk {
        /// Запрошенный размер chunk-а.
        chunk_bytes: u64,

        /// Запрошенный размер window.
        window_bytes: u64,
    },
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn default_values_are_8_and_256_mib() {
        let config = PrefetchConfig::default();

        assert_eq!(config.initial_chunk_bytes(), 64 * 1024);
        assert_eq!(config.chunk_bytes(), 8 * 1024 * 1024);
        assert_eq!(config.window_bytes(), 256 * 1024 * 1024);
    }

    #[test]
    fn new_rejects_zero_initial_chunk() {
        let error =
            PrefetchConfig::new(0, 1024, 2048).expect_err("zero initial chunk должен быть ошибкой");

        assert_eq!(error, PrefetchConfigError::ZeroInitialChunk);
    }

    #[test]
    fn new_rejects_zero_chunk() {
        let error = PrefetchConfig::new(1, 0, 1024).expect_err("zero chunk должен быть ошибкой");

        assert_eq!(error, PrefetchConfigError::ZeroChunk);
    }

    #[test]
    fn new_rejects_initial_chunk_larger_than_chunk() {
        let error = PrefetchConfig::new(4096, 2048, 8192)
            .expect_err("initial больше chunk должен быть ошибкой");

        assert_eq!(
            error,
            PrefetchConfigError::InitialChunkLargerThanChunk {
                initial_chunk_bytes: 4096,
                chunk_bytes: 2048,
            }
        );
    }

    #[test]
    fn new_rejects_window_smaller_than_chunk() {
        let error = PrefetchConfig::new(1024, 4096, 2048)
            .expect_err("window меньше chunk должен быть ошибкой");

        assert_eq!(
            error,
            PrefetchConfigError::WindowSmallerThanChunk {
                chunk_bytes: 4096,
                window_bytes: 2048,
            }
        );
    }

    #[test]
    fn new_accepts_valid_values() {
        let config = PrefetchConfig::new(1024, 4096, 8192)
            .expect("валидный prefetch config должен создаваться");

        assert_eq!(config.initial_chunk_bytes(), 1024);
        assert_eq!(config.chunk_bytes(), 4096);
        assert_eq!(config.window_bytes(), 8192);
    }
}
