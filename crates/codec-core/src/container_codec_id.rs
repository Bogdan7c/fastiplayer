//! Нормализация container codec id (`V_VP9`, `avc1`, `A_OPUS`, …) в codec enum-ы.
//!
//! Владеет таблицами алиасов и правилом сравнения. Функция вызывается на горячем
//! пути: `symphonia-demux::packet_mapper` определяет codec для каждого video
//! packet-а (проверка keyframe). Поэтому сравнение идёт без heap allocation:
//! `trim` + `eq_ignore_ascii_case` против алиасов, записанных в верхнем регистре.
//! Результат совпадает с прежним `trim().to_ascii_uppercase()` + точным `match`:
//! все алиасы — ASCII, а не-ASCII символы оба способа оставляют без изменений.
//! Cpu-parity 06d: прежний вариант давал одну лишнюю аллокацию на video packet.

use crate::model::{AudioCodec, VideoCodec};

/// Алиасы container codec id для video codec-ов (Matroska `CodecID`, ISO BMFF
/// sample entry, имена сервисов). Записаны в верхнем регистре.
const VIDEO_CODEC_ID_ALIASES: &[(VideoCodec, &[&str])] = &[
    (VideoCodec::Vp9, &["V_VP9", "VP9"]),
    (VideoCodec::Av1, &["V_AV1", "AV1", "AV01"]),
    (
        VideoCodec::H264,
        &["V_MPEG4/ISO/AVC", "AVC1", "H264", "H.264"],
    ),
    (
        VideoCodec::H265,
        &["V_MPEGH/ISO/HEVC", "HEV1", "HVC1", "H265", "H.265"],
    ),
    (VideoCodec::Vp8, &["V_VP8", "VP8"]),
];

/// Алиасы container codec id для audio codec-ов. Записаны в верхнем регистре.
const AUDIO_CODEC_ID_ALIASES: &[(AudioCodec, &[&str])] = &[
    (AudioCodec::Opus, &["A_OPUS", "OPUS"]),
    (
        AudioCodec::Aac,
        &["A_AAC", "A_AAC/MPEG2/LC", "A_AAC/MPEG4/LC", "AAC"],
    ),
    (AudioCodec::Vorbis, &["A_VORBIS", "VORBIS"]),
];

/// Video codec по container codec id; `None` для неизвестного id.
pub(crate) fn video_codec_from_container_id(codec_id: &str) -> Option<VideoCodec> {
    codec_by_alias(codec_id, VIDEO_CODEC_ID_ALIASES)
}

/// Audio codec по container codec id; `None` для неизвестного id.
pub(crate) fn audio_codec_from_container_id(codec_id: &str) -> Option<AudioCodec> {
    codec_by_alias(codec_id, AUDIO_CODEC_ID_ALIASES)
}

/// Ищет codec, у которого есть алиас, равный `codec_id` без учёта ASCII-регистра
/// и окружающих пробелов. Ничего не аллоцирует.
fn codec_by_alias<Codec: Copy>(codec_id: &str, aliases: &[(Codec, &[&str])]) -> Option<Codec> {
    let trimmed_codec_id = codec_id.trim();
    aliases
        .iter()
        .find(|(_, codec_aliases)| {
            codec_aliases
                .iter()
                .any(|alias| alias.eq_ignore_ascii_case(trimmed_codec_id))
        })
        .map(|(codec, _)| *codec)
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Прежняя реализация (до 06d) — эталон для проверки равенства результатов.
    fn uppercase_reference<Codec: Copy>(
        codec_id: &str,
        aliases: &[(Codec, &[&str])],
    ) -> Option<Codec> {
        let normalized_codec_id = codec_id.trim().to_ascii_uppercase();
        aliases
            .iter()
            .find(|(_, codec_aliases)| codec_aliases.contains(&normalized_codec_id.as_str()))
            .map(|(codec, _)| *codec)
    }

    /// Варианты написания одного алиаса: как есть, в нижнем регистре, смешанный, с пробелами.
    fn spellings(alias: &str) -> Vec<String> {
        let mixed_case: String = alias
            .chars()
            .enumerate()
            .map(|(index, character)| {
                if index % 2 == 0 {
                    character.to_ascii_lowercase()
                } else {
                    character
                }
            })
            .collect();
        vec![
            alias.to_owned(),
            alias.to_ascii_lowercase(),
            mixed_case,
            format!("  {alias}\t"),
        ]
    }

    #[test]
    fn every_video_alias_resolves_in_any_case_and_with_whitespace() {
        for (expected_codec, codec_aliases) in VIDEO_CODEC_ID_ALIASES {
            for alias in *codec_aliases {
                for spelling in spellings(alias) {
                    assert_eq!(
                        video_codec_from_container_id(&spelling),
                        Some(*expected_codec),
                        "video alias spelling {spelling:?}"
                    );
                }
            }
        }
    }

    #[test]
    fn every_audio_alias_resolves_in_any_case_and_with_whitespace() {
        for (expected_codec, codec_aliases) in AUDIO_CODEC_ID_ALIASES {
            for alias in *codec_aliases {
                for spelling in spellings(alias) {
                    assert_eq!(
                        audio_codec_from_container_id(&spelling),
                        Some(*expected_codec),
                        "audio alias spelling {spelling:?}"
                    );
                }
            }
        }
    }

    #[test]
    fn lookup_matches_previous_uppercase_normalization() {
        // Неизвестные, пустые, частичные, с внутренними пробелами и не-ASCII символами id.
        let probes = [
            "",
            " ",
            "unknown",
            "V_VP",
            "V_VP9X",
            "V VP9",
            "avc",
            "h.26",
            "hvc1 x",
            "vp9\u{0}",
            "ＶＰ９",
            "V_VP9ı",
            "a_opus",
            "A_AAC/MPEG4",
            " vorbis ",
        ];
        for probe in probes {
            assert_eq!(
                video_codec_from_container_id(probe),
                uppercase_reference(probe, VIDEO_CODEC_ID_ALIASES),
                "video probe {probe:?}"
            );
            assert_eq!(
                audio_codec_from_container_id(probe),
                uppercase_reference(probe, AUDIO_CODEC_ID_ALIASES),
                "audio probe {probe:?}"
            );
        }
    }

    #[test]
    fn unknown_or_partial_ids_are_rejected() {
        for codec_id in ["", "unknown", "V_VP9X", "V VP9", "MP4A"] {
            assert_eq!(
                video_codec_from_container_id(codec_id),
                None,
                "{codec_id:?}"
            );
            assert_eq!(
                audio_codec_from_container_id(codec_id),
                None,
                "{codec_id:?}"
            );
        }
    }
}
