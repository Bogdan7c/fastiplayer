//! Read-only capability-контракт audio decoder composition.
//!
//! Snapshot описывает только доказанные codec family, которые доступны в уже
//! собранном runtime. Он не создаёт decoder, не владеет decoder state и не
//! подменяет operational error, возникающую при фактическом открытии track-а.

use thiserror::Error;

/// Нейтральные audio codec family, доказанные current compatibility profile.
///
/// Наличие family не является wildcard для любого будущего raw codec id с тем
/// же prefix. До query caller обязан пройти static profile validation exact
/// identity; snapshot отвечает только за runtime availability доказанного set-а.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
#[repr(u8)]
pub enum AudioDecodeCodecFamily {
    /// Advanced Audio Coding.
    Aac = 0,
    /// Adaptive Differential Pulse-Code Modulation family.
    Adpcm = 1,
    /// Apple Lossless Audio Codec.
    Alac = 2,
    /// Free Lossless Audio Codec.
    Flac = 3,
    /// MPEG Audio Layer I.
    Mp1 = 4,
    /// MPEG Audio Layer II.
    Mp2 = 5,
    /// MPEG Audio Layer III.
    Mp3 = 6,
    /// Opus.
    Opus = 7,
    /// Pulse-Code Modulation family.
    Pcm = 8,
    /// Vorbis.
    Vorbis = 9,
}

impl AudioDecodeCodecFamily {
    /// Полный стабильный порядок family, который используют snapshot iteration и tests.
    pub const ALL: [Self; 10] = [
        Self::Aac,
        Self::Adpcm,
        Self::Alac,
        Self::Flac,
        Self::Mp1,
        Self::Mp2,
        Self::Mp3,
        Self::Opus,
        Self::Pcm,
        Self::Vorbis,
    ];

    /// Возвращает отдельный bit для compact immutable snapshot-а.
    const fn capability_bit(self) -> u16 {
        1_u16 << self as u8
    }
}

/// Typed identity на входе capability query.
///
/// `Unknown` сохраняет различие между неизвестной family и известной, но
/// недоступной в конкретном runtime family.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum AudioDecodeCodecFamilyQuery {
    /// Известная нейтральная codec family.
    Known(AudioDecodeCodecFamily),
    /// Raw codec identity не удалось доказанно связать с известной family.
    Unknown,
}

/// Read-only ответ capability query без смешивания unavailable и unknown.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum AudioDecodeCapability {
    /// Runtime имеет доказанный production decode path для profile-approved set-а family.
    Available,
    /// Family известна, но decoder path отсутствует или отключён в этом runtime.
    Unavailable,
}

/// Ошибка нейтрального capability query.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Error)]
pub enum AudioDecodeCapabilityQueryError {
    /// Неизвестную codec family нельзя молча считать playable или unavailable.
    #[error("Unknown audio codec family cannot be queried for decode capability")]
    UnknownCodecFamily,
}

/// Immutable runtime snapshot доказанных audio decode capabilities.
///
/// Bitset делает clone/query allocation-free и не предоставляет interior
/// mutability, поэтому selection не может задеть decoder lifecycle.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct AudioDecodeCapabilitySnapshot {
    /// Один bit на `AudioDecodeCodecFamily`.
    available_family_bits: u16,
}

impl AudioDecodeCapabilitySnapshot {
    /// Создаёт snapshot runtime-а без доступного audio decoder-а.
    #[must_use]
    pub const fn empty() -> Self {
        Self {
            available_family_bits: 0,
        }
    }

    /// Возвращает новый snapshot с одной дополнительно доступной family.
    #[must_use]
    pub const fn with_available_family(mut self, family: AudioDecodeCodecFamily) -> Self {
        self.available_family_bits |= family.capability_bit();
        self
    }

    /// Отвечает на typed query, не создавая decoder и не изменяя snapshot.
    pub const fn query(
        self,
        family: AudioDecodeCodecFamilyQuery,
    ) -> Result<AudioDecodeCapability, AudioDecodeCapabilityQueryError> {
        let AudioDecodeCodecFamilyQuery::Known(family) = family else {
            return Err(AudioDecodeCapabilityQueryError::UnknownCodecFamily);
        };

        if self.available_family_bits & family.capability_bit() != 0 {
            Ok(AudioDecodeCapability::Available)
        } else {
            Ok(AudioDecodeCapability::Unavailable)
        }
    }

    /// Итерирует доступные family в стабильном нейтральном порядке без allocation.
    pub fn available_families(self) -> impl Iterator<Item = AudioDecodeCodecFamily> {
        AudioDecodeCodecFamily::ALL
            .into_iter()
            .filter(move |family| self.available_family_bits & family.capability_bit() != 0)
    }
}

/// Read-only provider immutable runtime audio capability snapshot-а.
pub trait AudioDecodeCapabilityProvider: Send + Sync {
    /// Возвращает snapshot без decoder construction или decoder-state mutation.
    fn audio_decode_capability_snapshot(&self) -> AudioDecodeCapabilitySnapshot;
}

#[cfg(test)]
mod tests {
    use super::{
        AudioDecodeCapability, AudioDecodeCapabilityProvider, AudioDecodeCapabilityQueryError,
        AudioDecodeCapabilitySnapshot, AudioDecodeCodecFamily, AudioDecodeCodecFamilyQuery,
    };

    /// Fake provider доказывает, что consumer зависит только от neutral trait-а.
    #[derive(Debug)]
    struct FakeAudioDecodeCapabilityProvider {
        /// Заранее подготовленный immutable ответ fake runtime-а.
        snapshot: AudioDecodeCapabilitySnapshot,
    }

    impl AudioDecodeCapabilityProvider for FakeAudioDecodeCapabilityProvider {
        /// Возвращает value snapshot без concrete decoder dependency.
        fn audio_decode_capability_snapshot(&self) -> AudioDecodeCapabilitySnapshot {
            self.snapshot
        }
    }

    /// Fake provider сохраняет различие между available и unavailable family.
    #[test]
    fn fake_provider_exposes_only_declared_runtime_family() {
        let provider = FakeAudioDecodeCapabilityProvider {
            snapshot: AudioDecodeCapabilitySnapshot::empty()
                .with_available_family(AudioDecodeCodecFamily::Opus),
        };
        let snapshot = provider.audio_decode_capability_snapshot();

        assert_eq!(
            snapshot.query(AudioDecodeCodecFamilyQuery::Known(
                AudioDecodeCodecFamily::Opus,
            )),
            Ok(AudioDecodeCapability::Available)
        );
        assert_eq!(
            snapshot.query(AudioDecodeCodecFamilyQuery::Known(
                AudioDecodeCodecFamily::Aac,
            )),
            Ok(AudioDecodeCapability::Unavailable)
        );
    }

    /// Unknown family остаётся typed ошибкой, а не generic unavailable.
    #[test]
    fn unknown_codec_family_is_rejected_typed() {
        assert_eq!(
            AudioDecodeCapabilitySnapshot::empty().query(AudioDecodeCodecFamilyQuery::Unknown),
            Err(AudioDecodeCapabilityQueryError::UnknownCodecFamily)
        );
    }

    /// Stable iterator не выделяет промежуточную collection и не раскрывает bitset.
    #[test]
    fn available_family_iteration_preserves_neutral_order() {
        let snapshot = AudioDecodeCapabilitySnapshot::empty()
            .with_available_family(AudioDecodeCodecFamily::Vorbis)
            .with_available_family(AudioDecodeCodecFamily::Aac);

        assert_eq!(
            snapshot.available_families().collect::<Vec<_>>(),
            vec![AudioDecodeCodecFamily::Aac, AudioDecodeCodecFamily::Vorbis,]
        );
    }
}
