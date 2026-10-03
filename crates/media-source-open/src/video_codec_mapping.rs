//! Отображение кодека из пользовательской конфигурации в runtime vocabulary.
//!
//! `fastiplayer_config::VideoCodec` описывает политику в настройках, а
//! `codec_core::VideoCodec` — нейтральный словарь capability-проверок. Match
//! исчерпывающий: новый вариант в config не скомпилируется без явного решения.

use codec_core::VideoCodec as RuntimeVideoCodec;
use fastiplayer_config::VideoCodec as ConfigVideoCodec;

/// Сопоставляет user-facing codec policy с нейтральным capability vocabulary.
pub const fn runtime_video_codec(codec: ConfigVideoCodec) -> RuntimeVideoCodec {
    match codec {
        ConfigVideoCodec::Vp9 => RuntimeVideoCodec::Vp9,
        ConfigVideoCodec::Av1 => RuntimeVideoCodec::Av1,
        ConfigVideoCodec::H264 => RuntimeVideoCodec::H264,
        ConfigVideoCodec::H265 => RuntimeVideoCodec::H265,
        ConfigVideoCodec::Vp8 => RuntimeVideoCodec::Vp8,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Каждый кодек из настроек попадает в одноимённый runtime-кодек,
    /// и два разных config-кодека никогда не склеиваются в один runtime.
    #[test]
    fn every_config_codec_maps_to_same_named_runtime_codec() {
        let expected_pairs = [
            (ConfigVideoCodec::Vp9, RuntimeVideoCodec::Vp9),
            (ConfigVideoCodec::Av1, RuntimeVideoCodec::Av1),
            (ConfigVideoCodec::H264, RuntimeVideoCodec::H264),
            (ConfigVideoCodec::H265, RuntimeVideoCodec::H265),
            (ConfigVideoCodec::Vp8, RuntimeVideoCodec::Vp8),
        ];
        for (config_codec, runtime_codec) in expected_pairs {
            assert_eq!(runtime_video_codec(config_codec), runtime_codec);
        }
        let distinct_runtime_codecs = expected_pairs
            .iter()
            .map(|(config_codec, _)| runtime_video_codec(*config_codec))
            .collect::<std::collections::HashSet<_>>();
        assert_eq!(distinct_runtime_codecs.len(), expected_pairs.len());
    }
}
