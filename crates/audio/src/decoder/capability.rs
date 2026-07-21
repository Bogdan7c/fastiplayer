//! Read-only снимок concrete Symphonia/Opus decode registry.
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
    )
}

/// Строит snapshot из переданного registry view без decoder construction.
fn audio_decode_capability_snapshot(
    registry: &impl RegisteredAudioDecoderLookup,
    opus_fallback: OpusFallbackAvailability,
) -> AudioDecodeCapabilitySnapshot {
    let mut snapshot = AudioDecodeCapabilitySnapshot::empty();

    for family in AudioDecodeCodecFamily::ALL {
        if family_is_available(registry, family, opus_fallback) {
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
) -> bool {
    if family == AudioDecodeCodecFamily::Opus {
        return opus_fallback == OpusFallbackAvailability::Available;
    }

    required_symphonia_codecs(family)
        .iter()
        .copied()
        .all(|codec_id| registry.has_registered_audio_decoder(codec_id))
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
    }
}

#[cfg(test)]
mod tests {
    use std::cell::Cell;

    use audio_core::{
        AudioDecodeCapability, AudioDecodeCapabilityQueryError, AudioDecodeCodecFamilyQuery,
    };

    use super::{
        AudioDecodeCodecFamily, OpusFallbackAvailability, RegisteredAudioDecoderLookup,
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
        let snapshot =
            audio_decode_capability_snapshot(&registry, OpusFallbackAvailability::Unavailable);

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

        let snapshot =
            audio_decode_capability_snapshot(&registry, OpusFallbackAvailability::Available);

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
}
