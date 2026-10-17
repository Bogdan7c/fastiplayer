//! Настройки декодера.

pub use crate::spectral::dither::{DitherMode, DitherSeed};
pub use crate::synthesis::dynamic_range::{CompressionScale, DynamicRangeControl};
pub use crate::synthesis::transient_pre_noise::TransientPreNoiseProcessing;

/// Что делать с CRC кадра (A/52 §7.10.1).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Default)]
pub enum CrcPolicy {
    /// Проверять `crc1` и `crc2`; несовпадение — [`crate::Ac3DecodeError::CrcMismatch`],
    /// кадр не декодируется. Значение по умолчанию.
    #[default]
    Verify,
    /// Не проверять. Нужен тестам устойчивости разбора (порча бит внутри
    /// кадра без пересчёта CRC) и потокам, где целостность уже проверил
    /// транспорт.
    Skip,
}

/// Параметры [`crate::Ac3Decoder`].
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Default)]
pub struct DecoderConfig {
    /// Политика CRC.
    pub crc: CrcPolicy,
    /// Заполнение мантисс без бит (§7.3.4).
    pub dither: DitherMode,
    /// Применение `dynrng`/`compr` (§7.7); по умолчанию — полный `dynrng`.
    pub dynamic_range: DynamicRangeControl,
    /// Подавление пред-шума транзиентов E-AC-3 (Annex E §3.7); по умолчанию
    /// применяется.
    pub transient_pre_noise: TransientPreNoiseProcessing,
}
