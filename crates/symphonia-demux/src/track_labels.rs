//! Язык, название и флаги дорожек: перенос из Symphonia в нейтральные `TrackLabels`.
//!
//! Язык и флаги приходят в `Track::language` / `Track::flags`. Название дорожки Symphonia
//! не хранит; его публикует MKV-патч per-track тегом (`FASTIPLAYER_TRACK_NAME_TAG`),
//! а здесь оно читается из metadata log до построения списка дорожек.

use std::collections::HashMap;

use media_core::{TrackDisposition, TrackId, TrackLabels, TrackLanguage};
use symphonia::core::formats::{Track, TrackFlags};
use symphonia::core::meta::RawValue;
use symphonia_format_mkv::FASTIPLAYER_TRACK_NAME_TAG;
use tracing::debug;

use crate::symphonia_api::FormatReaderBox;

/// Код «язык не определён» (ISO 639-2): для потребителя равносилен отсутствию языка.
const UNDETERMINED_LANGUAGE_CODE: &str = "und";

/// Строит метки дорожки из Symphonia-данных и опционального названия из патча.
pub(crate) fn labels_from_track(track: &Track, title: Option<&str>) -> TrackLabels {
    TrackLabels {
        language: language_from_symphonia(track.language.as_deref()),
        title: title
            .map(str::trim)
            .filter(|title| !title.is_empty())
            .map(str::to_owned),
        disposition: disposition_from_symphonia(track.flags),
    }
}

/// Нормализует только пустоту и `und`; остальное остаётся «как в контейнере».
fn language_from_symphonia(language: Option<&str>) -> Option<TrackLanguage> {
    language
        .map(str::trim)
        .filter(|language| {
            !language.is_empty() && !language.eq_ignore_ascii_case(UNDETERMINED_LANGUAGE_CODE)
        })
        .map(TrackLanguage::new)
}

/// Переносит поддерживаемые плеером флаги назначения.
fn disposition_from_symphonia(flags: TrackFlags) -> TrackDisposition {
    TrackDisposition {
        is_default: flags.contains(TrackFlags::DEFAULT),
        is_forced: flags.contains(TrackFlags::FORCED),
        is_hearing_impaired: flags.contains(TrackFlags::HEARING_IMPAIRED),
        is_commentary: flags.contains(TrackFlags::COMMENTARY),
    }
}

/// Читает названия дорожек, опубликованные MKV-патчем в текущей metadata revision.
///
/// Вызывать нужно до `consume_media_metadata`, который вычитывает log до конца. Результат
/// запоминается demuxer-ом: после `ResetRequired` log уже не содержит этой ревизии.
pub(crate) fn track_titles_from_metadata(
    format: &mut FormatReaderBox<'static>,
) -> HashMap<TrackId, String> {
    let mut titles_by_track = HashMap::new();
    let metadata = format.metadata();
    let Some(revision) = metadata.current() else {
        return titles_by_track;
    };

    for per_track_metadata in &revision.per_track {
        let Ok(raw_track_id) = u32::try_from(per_track_metadata.track_id) else {
            debug!(
                track_id = per_track_metadata.track_id,
                "Track title metadata id does not fit media-core TrackId"
            );
            continue;
        };
        for tag in &per_track_metadata.metadata.tags {
            if tag.raw.key != FASTIPLAYER_TRACK_NAME_TAG {
                continue;
            }
            match &tag.raw.value {
                RawValue::String(title) => {
                    titles_by_track.insert(TrackId::new(raw_track_id), title.as_str().to_owned());
                }
                _ => debug!(
                    track_id = per_track_metadata.track_id,
                    "Track title metadata has unsupported value type"
                ),
            }
        }
    }

    titles_by_track
}

#[cfg(test)]
mod tests {
    use super::*;

    fn track_with(language: Option<&str>, flags: TrackFlags) -> Track {
        let mut track = Track::new(1);
        if let Some(language) = language {
            track.with_language(language);
        }
        track.with_flags(flags);
        track
    }

    #[test]
    fn labels_carry_language_title_and_each_disposition_flag() {
        let track = track_with(
            Some("rus"),
            TrackFlags::DEFAULT
                | TrackFlags::FORCED
                | TrackFlags::HEARING_IMPAIRED
                | TrackFlags::COMMENTARY,
        );

        let labels = labels_from_track(&track, Some("  Русский  "));

        assert_eq!(labels.language, Some(TrackLanguage::new("rus")));
        assert_eq!(labels.title.as_deref(), Some("Русский"));
        assert_eq!(
            labels.disposition,
            TrackDisposition {
                is_default: true,
                is_forced: true,
                is_hearing_impaired: true,
                is_commentary: true,
            }
        );
    }

    #[test]
    fn undetermined_empty_or_missing_language_and_blank_title_become_none() {
        for language in [Some("und"), Some("UND"), Some(" "), None] {
            let labels = labels_from_track(&track_with(language, TrackFlags::empty()), Some(" "));
            assert_eq!(labels.language, None, "language {language:?}");
            assert_eq!(labels.title, None);
            assert_eq!(labels.disposition, TrackDisposition::default());
        }
    }

    #[test]
    fn flags_unknown_to_player_do_not_leak_into_disposition() {
        let labels = labels_from_track(
            &track_with(
                None,
                TrackFlags::ORIGINAL_LANGUAGE | TrackFlags::TEXT_DESCRIPTIONS,
            ),
            None,
        );
        assert_eq!(labels.disposition, TrackDisposition::default());
    }
}
