//! Read-only снимок concrete Symphonia/Opus/SWF/AC-3 decode paths.
//!
//! Этот модуль владеет соответствием neutral family и production backend-ов.
//! Selection получает только `audio-core` snapshot и не видит Symphonia types.
//! Snapshot не превращает family в wildcard: exact raw codec identity сначала
//! обязан пройти versioned static compatibility profile.

use audio_core::{AudioDecodeCapabilitySnapshot, AudioDecodeCodecFamily};
use symphonia::core::codecs::audio::AudioCodecId;
use symphonia::core::codecs::audio::well_known as codec;
use symphonia::core::codecs::registry::CodecRegistry;

/// Production Opus fallback собран в crate и доступен независимо от Symphonia registry.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) enum OpusFallbackAvailability {
    /// Concrete `opus` adapter доступен composition layer-у.
    Available,
    /// Тестовый/runtime variant без fallback decoder-а.
    #[cfg(test)]
    Unavailable,
}

/// Project-owned SWF ADPCM fallback доступен независимо от Symphonia registry.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum SwfAdpcmFallbackAvailability {
    /// Exact `A_ADPCM_SWF` decoder собран в production factory.
    Available,
    /// Тестовый variant без project-owned SWF decoder-а.
    #[cfg(test)]
    Unavailable,
}

/// Project-owned AC-3 / E-AC-3 adapter (собственный `ac3-decoder`) не зависит от Symphonia registry.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Ac3FamilyAvailability {
    /// Exact `A_AC3` / `A_EAC3` adapter собран в production factory.
    Available,
    /// Тестовый variant без AC-3 adapter-а.
    #[cfg(test)]
    Unavailable,
}

/// Минимальная read-only граница над concrete registry.
///
/// В trait намеренно отсутствует decoder factory method: capability scan физически
/// не может создать decoder через эту зависимость.
trait RegisteredAudioDecoderLookup {
    /// Проверяет наличие registration для exact Symphonia codec id.
    fn has_registered_audio_decoder(&self, codec_id: AudioCodecId) -> bool;
}

impl RegisteredAudioDecoderLookup for CodecRegistry {
    /// Использует immutable registry lookup без вызова factory function.
    fn has_registered_audio_decoder(&self, codec_id: AudioCodecId) -> bool {
        self.get_audio_decoder(codec_id).is_some()
    }
}

/// Строит production snapshot из default Symphonia registry и Opus fallback-а.
pub(super) fn production_audio_decode_capability_snapshot() -> AudioDecodeCapabilitySnapshot {
    audio_decode_capability_snapshot(
        symphonia::default::get_codecs(),
        OpusFallbackAvailability::Available,
        SwfAdpcmFallbackAvailability::Available,
        Ac3FamilyAvailability::Available,
    )
}

/// Строит snapshot из переданного registry view без decoder construction.
fn audio_decode_capability_snapshot(
    registry: &impl RegisteredAudioDecoderLookup,
    opus_fallback: OpusFallbackAvailability,
    swf_adpcm_fallback: SwfAdpcmFallbackAvailability,
    ac3_family: Ac3FamilyAvailability,
) -> AudioDecodeCapabilitySnapshot {
    let mut snapshot = AudioDecodeCapabilitySnapshot::empty();

    for family in AudioDecodeCodecFamily::ALL {
        if family_is_available(
            registry,
            family,
            opus_fallback,
            swf_adpcm_fallback,
            ac3_family,
        ) {
            snapshot = snapshot.with_available_family(family);
        }
    }

    snapshot
}

/// Проверяет доказанный profile decoder set, не смешивая его с другой family.
fn family_is_available(
    registry: &impl RegisteredAudioDecoderLookup,
    family: AudioDecodeCodecFamily,
    opus_fallback: OpusFallbackAvailability,
    swf_adpcm_fallback: SwfAdpcmFallbackAvailability,
    ac3_family: Ac3FamilyAvailability,
) -> bool {
    if family == AudioDecodeCodecFamily::Opus {
        return opus_fallback == OpusFallbackAvailability::Available;
    }
    // AC-3 и E-AC-3 обслуживает один adapter, но family остаются раздельными.
    if matches!(
        family,
        AudioDecodeCodecFamily::Ac3 | AudioDecodeCodecFamily::Eac3
    ) {
        return ac3_family == Ac3FamilyAvailability::Available;
    }

    let symphonia_set_available = required_symphonia_codecs(family)
        .iter()
        .copied()
        .all(|codec_id| registry.has_registered_audio_decoder(codec_id));
    if family == AudioDecodeCodecFamily::Adpcm {
        return symphonia_set_available
            && swf_adpcm_fallback == SwfAdpcmFallbackAvailability::Available;
    }
    symphonia_set_available
}

/// Возвращает exact Symphonia registrations, доказанные current compatibility profile.
fn required_symphonia_codecs(family: AudioDecodeCodecFamily) -> &'static [AudioCodecId] {
    match family {
        AudioDecodeCodecFamily::Aac => &[codec::CODEC_ID_AAC],
        AudioDecodeCodecFamily::Adpcm => &[
            codec::CODEC_ID_ADPCM_MS,
            codec::CODEC_ID_ADPCM_IMA_WAV,
            codec::CODEC_ID_ADPCM_IMA_QT,
        ],
        AudioDecodeCodecFamily::Alac => &[codec::CODEC_ID_ALAC],
        AudioDecodeCodecFamily::Flac => &[codec::CODEC_ID_FLAC],
        AudioDecodeCodecFamily::Mp1 => &[codec::CODEC_ID_MP1],
        AudioDecodeCodecFamily::Mp2 => &[codec::CODEC_ID_MP2],
        AudioDecodeCodecFamily::Mp3 => &[codec::CODEC_ID_MP3],
        AudioDecodeCodecFamily::Opus => &[],
        AudioDecodeCodecFamily::Pcm => &[
            codec::CODEC_ID_PCM_S32LE,
            codec::CODEC_ID_PCM_S32BE,
            codec::CODEC_ID_PCM_S24LE,
            codec::CODEC_ID_PCM_S24BE,
            codec::CODEC_ID_PCM_S16LE,
            codec::CODEC_ID_PCM_S16BE,
            codec::CODEC_ID_PCM_S8,
            codec::CODEC_ID_PCM_U32LE,
            codec::CODEC_ID_PCM_U32BE,
            codec::CODEC_ID_PCM_U24LE,
            codec::CODEC_ID_PCM_U24BE,
            codec::CODEC_ID_PCM_U16LE,
            codec::CODEC_ID_PCM_U16BE,
            codec::CODEC_ID_PCM_U8,
            codec::CODEC_ID_PCM_F32LE,
            codec::CODEC_ID_PCM_F32BE,
            codec::CODEC_ID_PCM_F64LE,
            codec::CODEC_ID_PCM_F64BE,
            codec::CODEC_ID_PCM_ALAW,
            codec::CODEC_ID_PCM_MULAW,
        ],
        AudioDecodeCodecFamily::Vorbis => &[codec::CODEC_ID_VORBIS],
        // Symphonia 0.6 знает id AC-3/E-AC-3, но декодера не регистрирует;
        // доступность определяет только project-owned adapter.
        AudioDecodeCodecFamily::Ac3 | AudioDecodeCodecFamily::Eac3 => &[],
    }
}

#[cfg(test)]
mod tests {
    use std::cell::Cell;

    use audio_core::{
        AudioDecodeCapability, AudioDecodeCapabilityQueryError, AudioDecodeCodecFamilyQuery,
    };

    use super::{
        Ac3FamilyAvailability, AudioDecodeCodecFamily, OpusFallbackAvailability,
        RegisteredAudioDecoderLookup, SwfAdpcmFallbackAvailability,
        audio_decode_capability_snapshot, production_audio_decode_capability_snapshot,
        required_symphonia_codecs,
    };

    /// Read-only fake registry считает lookup-и и не имеет decoder-construction API.
    struct CountingRegistryLookup {
        /// Количество выполненных immutable registration lookup-ов.
        query_count: Cell<usize>,
        /// Нужно ли считать каждый запрошенный codec зарегистрированным.
        all_registered: bool,
    }

    impl RegisteredAudioDecoderLookup for CountingRegistryLookup {
        /// Считает только read-only lookup и возвращает настроенный результат.
        fn has_registered_audio_decoder(
            &self,
            _codec_id: symphonia::core::codecs::audio::AudioCodecId,
        ) -> bool {
            self.query_count.set(self.query_count.get() + 1);
            self.all_registered
        }
    }

    /// Current production registry обязан покрывать весь доказанный S20 matrix.
    #[test]
    fn production_snapshot_matches_current_proven_codec_registry() {
        let snapshot = production_audio_decode_capability_snapshot();

        for family in AudioDecodeCodecFamily::ALL {
            assert_eq!(
                snapshot.query(AudioDecodeCodecFamilyQuery::Known(family)),
                Ok(AudioDecodeCapability::Available),
                "production audio codec family {family:?} disappeared from registry"
            );
        }
    }

    /// Empty registry и disabled Opus fallback дают честный empty snapshot.
    #[test]
    fn absent_registry_and_disabled_fallback_report_every_family_unavailable() {
        let registry = symphonia::core::codecs::registry::CodecRegistry::new();
        let snapshot = audio_decode_capability_snapshot(
            &registry,
            OpusFallbackAvailability::Unavailable,
            SwfAdpcmFallbackAvailability::Unavailable,
            Ac3FamilyAvailability::Unavailable,
        );

        assert_eq!(snapshot.available_families().count(), 0);
        assert_eq!(
            snapshot.query(AudioDecodeCodecFamilyQuery::Known(
                AudioDecodeCodecFamily::Opus,
            )),
            Ok(AudioDecodeCapability::Unavailable)
        );
    }

    /// Scan использует только read-only lookup; decoder state отсутствует на boundary.
    #[test]
    fn capability_scan_does_not_construct_or_mutate_decoder_state() {
        let registry = CountingRegistryLookup {
            query_count: Cell::new(0),
            all_registered: true,
        };

        let snapshot = audio_decode_capability_snapshot(
            &registry,
            OpusFallbackAvailability::Available,
            SwfAdpcmFallbackAvailability::Available,
            Ac3FamilyAvailability::Available,
        );

        let expected_registry_queries = AudioDecodeCodecFamily::ALL
            .into_iter()
            .map(required_symphonia_codecs)
            .map(<[symphonia::core::codecs::audio::AudioCodecId]>::len)
            .sum::<usize>();
        assert_eq!(registry.query_count.get(), expected_registry_queries);
        assert_eq!(
            snapshot.query(AudioDecodeCodecFamilyQuery::Unknown),
            Err(AudioDecodeCapabilityQueryError::UnknownCodecFamily)
        );
    }

    /// ADPCM family не считается полной без exact SWF fallback-а.
    #[test]
    fn adpcm_family_requires_project_owned_swf_fallback() {
        let registry = CountingRegistryLookup {
            query_count: Cell::new(0),
            all_registered: true,
        };
        let snapshot = audio_decode_capability_snapshot(
            &registry,
            OpusFallbackAvailability::Available,
            SwfAdpcmFallbackAvailability::Unavailable,
            Ac3FamilyAvailability::Available,
        );

        assert_eq!(
            snapshot.query(AudioDecodeCodecFamilyQuery::Known(
                AudioDecodeCodecFamily::Adpcm,
            )),
            Ok(AudioDecodeCapability::Unavailable)
        );
        assert_eq!(
            snapshot.query(AudioDecodeCodecFamilyQuery::Known(
                AudioDecodeCodecFamily::Aac,
            )),
            Ok(AudioDecodeCapability::Available)
        );
    }

    /// AC-3 и E-AC-3 доступны в production без Symphonia registration и
    /// пропадают вместе с adapter-ом, не затрагивая остальные family.
    #[test]
    fn ac3_families_follow_project_owned_adapter_not_symphonia_registry() {
        let empty_registry = symphonia::core::codecs::registry::CodecRegistry::new();
        let with_adapter = audio_decode_capability_snapshot(
            &empty_registry,
            OpusFallbackAvailability::Unavailable,
            SwfAdpcmFallbackAvailability::Unavailable,
            Ac3FamilyAvailability::Available,
        );
        let registry = CountingRegistryLookup {
            query_count: Cell::new(0),
            all_registered: true,
        };
        let without_adapter = audio_decode_capability_snapshot(
            &registry,
            OpusFallbackAvailability::Available,
            SwfAdpcmFallbackAvailability::Available,
            Ac3FamilyAvailability::Unavailable,
        );

        assert_eq!(
            with_adapter.available_families().collect::<Vec<_>>(),
            vec![AudioDecodeCodecFamily::Ac3, AudioDecodeCodecFamily::Eac3]
        );
        for family in [AudioDecodeCodecFamily::Ac3, AudioDecodeCodecFamily::Eac3] {
            assert_eq!(
                without_adapter.query(AudioDecodeCodecFamilyQuery::Known(family)),
                Ok(AudioDecodeCapability::Unavailable)
            );
            assert!(required_symphonia_codecs(family).is_empty());
        }
        assert_eq!(
            without_adapter.query(AudioDecodeCodecFamilyQuery::Known(
                AudioDecodeCodecFamily::Aac,
            )),
            Ok(AudioDecodeCapability::Available)
        );
    }
}
