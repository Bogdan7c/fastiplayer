//! Автовыбор дорожки субтитров (решение владельца 14): чистая функция без побочных эффектов.
//!
//! # Правила
//! Включено (`for_enabled`):
//! 1. Кандидаты — дорожки, которые можно показать сейчас (`is_renderable`). ASS/SSA до
//!    сессии 08 не выбираются никогда.
//! 2. Принудительные (forced) дорожки содержат только реплики на иностранную речь, поэтому
//!    для «хочу субтитры» они годятся, лишь когда полных дорожек нет вовсе.
//! 3. Первый язык из списка предпочтений, для которого есть дорожка, побеждает (порядок
//!    списка — приоритет). Язык сравнивается по основному подтегу без учёта регистра в канонической
//!    форме: `ru` подходит к `ru-RU` и к `rus`.
//! 4. Внутри одного языка обычная дорожка лучше дорожки для слабослышащих (SDH); при
//!    равенстве — раньше в порядке кандидатов.
//! 5. При равном языке и «слышимости» файл рядом (sidecar) лучше встроенной дорожки (решение
//!    владельца сессии 06); затем дорожка с флагом default контейнера; затем порядок списка.
//! 6. Если ни один язык не совпал, берётся: файл рядом, затем встроенная дорожка с флагом
//!    default, затем первая подходящая в порядке списка (пользователь хочет субтитры хоть какие-то).
//!
//! Выключено (`forced_fallback`): показывается только forced-дорожка, язык которой совпал с
//! предпочтениями (первый совпавший язык из списка); нет совпадения — ничего.
//!
//! Пустой список предпочтений заменяется языком системы (если он известен).

use subtitle_core::SubtitleLanguage;

/// Откуда дорожка. Порядок вариантов = приоритет при равенстве остальных признаков.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub(super) enum TrackOrigin {
    /// Файл рядом с видео либо добавленный пользователем.
    Sidecar,
    /// Дорожка внутри контейнера.
    Embedded,
}

/// Признаки дорожки, нужные для автовыбора (без путей и содержимого).
#[derive(Debug, Clone, Copy)]
pub(super) struct TrackTraits<'a> {
    /// Файл или встроенная дорожка (файл важнее при равенстве).
    pub(super) origin: TrackOrigin,
    /// Контейнер пометил дорожку как выбираемую по умолчанию (у файлов всегда `false`).
    pub(super) is_default: bool,
    /// Язык дорожки; `None` — не объявлен (с предпочтениями не совпадает никогда).
    pub(super) language: Option<&'a SubtitleLanguage>,
    /// Дорожка принудительная (только иностранная речь).
    pub(super) is_forced: bool,
    /// Дорожка для слабослышащих (SDH/CC/HI).
    pub(super) is_hearing_impaired: bool,
    /// Формат можно показать уже сейчас.
    pub(super) is_renderable: bool,
}

/// Предпочитаемые языки в порядке приоритета: основные подтеги в нижнем регистре.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub(super) struct PreferredLanguages {
    primary_subtags: Vec<String>,
}

impl PreferredLanguages {
    /// Строит список из кодов конфига; невалидные коды пропускаются с записью в лог. Если
    /// итоговый список пуст (пустой конфиг или всё невалидно), берётся язык системы.
    pub(super) fn resolve(
        configured_codes: &[String],
        system_language: Option<&SubtitleLanguage>,
    ) -> Self {
        let mut primary_subtags: Vec<String> = Vec::new();
        for code in configured_codes {
            match SubtitleLanguage::parse(code) {
                Ok(language) => {
                    let subtag = language.canonical_primary_language();
                    // Повтор языка в списке не меняет приоритет первого вхождения.
                    if !primary_subtags.contains(&subtag) {
                        primary_subtags.push(subtag);
                    }
                }
                Err(error) => {
                    tracing::warn!(%error, "Ignoring an invalid preferred subtitle language");
                }
            }
        }
        if primary_subtags.is_empty()
            && let Some(language) = system_language
        {
            primary_subtags.push(language.canonical_primary_language());
        }
        Self { primary_subtags }
    }

    /// Позиция языка дорожки в списке предпочтений (меньше — важнее); `None` — не совпал.
    fn rank_of(&self, language: Option<&SubtitleLanguage>) -> Option<usize> {
        // Дорожка из файла рядом приходит как `rus`, а предпочтение — `ru`: сравниваем
        // канонические формы (3-буквенный код приводится к 2-буквенному).
        let subtag = language?.canonical_primary_language();
        self.primary_subtags
            .iter()
            .position(|preferred| *preferred == subtag)
    }
}

/// Результат автовыбора: индексы во входном списке дорожек.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub(super) struct AutoSelection {
    /// Дорожка, если субтитры включены.
    pub(super) for_enabled: Option<usize>,
    /// Принудительная дорожка, которую показываем даже при выключенных субтитрах.
    pub(super) forced_fallback: Option<usize>,
}

/// Выбирает дорожки по правилам из документации модуля.
pub(super) fn auto_select(
    tracks: &[TrackTraits<'_>],
    preferred: &PreferredLanguages,
) -> AutoSelection {
    AutoSelection {
        for_enabled: select_for_enabled(tracks, preferred),
        forced_fallback: select_forced_fallback(tracks, preferred),
    }
}

/// Выбор при включённых субтитрах (правила 1–5).
fn select_for_enabled(tracks: &[TrackTraits<'_>], preferred: &PreferredLanguages) -> Option<usize> {
    let renderable = || {
        tracks
            .iter()
            .enumerate()
            .filter(|(_, track)| track.is_renderable)
    };
    // Полные дорожки; forced участвуют только если полных нет совсем.
    let has_full_track = renderable().any(|(_, track)| !track.is_forced);
    let pool = || renderable().filter(move |(_, track)| !has_full_track || !track.is_forced);

    let best_preferred = pool()
        .filter_map(|(index, track)| {
            preferred.rank_of(track.language).map(|language_rank| {
                (
                    (
                        language_rank,
                        track.is_hearing_impaired,
                        track.origin,
                        !track.is_default,
                    ),
                    index,
                )
            })
        })
        // `min_by_key` при равных ключах оставляет первый: это и есть «раньше в порядке».
        .min_by_key(|(sort_key, _)| *sort_key)
        .map(|(_, index)| index);
    // Язык не совпал: файл рядом, затем default контейнера, затем порядок списка.
    best_preferred.or_else(|| {
        pool()
            .min_by_key(|(_, track)| (track.origin, !track.is_default))
            .map(|(index, _)| index)
    })
}

/// Принудительная дорожка нужного языка для режима «выключено».
fn select_forced_fallback(
    tracks: &[TrackTraits<'_>],
    preferred: &PreferredLanguages,
) -> Option<usize> {
    tracks
        .iter()
        .enumerate()
        .filter(|(_, track)| track.is_renderable && track.is_forced)
        .filter_map(|(index, track)| {
            preferred
                .rank_of(track.language)
                .map(|language_rank| (language_rank, index))
        })
        .min_by_key(|(language_rank, _)| *language_rank)
        .map(|(_, index)| index)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn language(tag: &str) -> SubtitleLanguage {
        SubtitleLanguage::parse(tag).expect("valid test tag")
    }

    /// Дорожка с языком; флаги по умолчанию «обычная, показываемая».
    fn track(language: Option<&SubtitleLanguage>) -> TrackTraits<'_> {
        TrackTraits {
            origin: TrackOrigin::Sidecar,
            is_default: false,
            language,
            is_forced: false,
            is_hearing_impaired: false,
            is_renderable: true,
        }
    }

    fn preferred(codes: &[&str]) -> PreferredLanguages {
        let owned: Vec<String> = codes.iter().map(|code| (*code).to_owned()).collect();
        PreferredLanguages::resolve(&owned, None)
    }

    #[test]
    fn first_preferred_language_with_a_track_wins_in_list_order() {
        let (en, ru) = (language("en"), language("ru"));
        let tracks = [track(Some(&en)), track(Some(&ru))];
        let selection = auto_select(&tracks, &preferred(&["uk", "ru"]));
        assert_eq!(selection.for_enabled, Some(1), "uk has no track, ru does");
    }

    #[test]
    fn three_letter_track_code_matches_two_letter_preference_and_vice_versa() {
        let (eng, rus) = (language("eng"), language("rus"));
        let tracks = [track(Some(&eng)), track(Some(&rus))];
        assert_eq!(
            auto_select(&tracks, &preferred(&["ru"])).for_enabled,
            Some(1),
            "preference ru must pick the rus sidecar track"
        );
        let (en, ru) = (language("en"), language("ru"));
        let tracks = [track(Some(&en)), track(Some(&ru))];
        assert_eq!(
            auto_select(&tracks, &preferred(&["rus"])).for_enabled,
            Some(1),
            "preference rus must pick the ru track"
        );
    }

    #[test]
    fn list_order_beats_track_order() {
        let (ru, en) = (language("ru"), language("en"));
        let tracks = [track(Some(&ru)), track(Some(&en))];
        let selection = auto_select(&tracks, &preferred(&["en", "ru"]));
        assert_eq!(selection.for_enabled, Some(1));
    }

    #[test]
    fn region_subtag_and_letter_case_do_not_matter() {
        let ru_ru = language("ru-RU");
        let tracks = [track(Some(&ru_ru))];
        assert_eq!(
            auto_select(&tracks, &preferred(&["RU"])).for_enabled,
            Some(0)
        );
    }

    #[test]
    fn no_language_match_falls_back_to_the_first_track() {
        let (en, de) = (language("en"), language("de"));
        let tracks = [track(Some(&en)), track(Some(&de))];
        assert_eq!(
            auto_select(&tracks, &preferred(&["ru"])).for_enabled,
            Some(0)
        );
        // Дорожка без языка тоже годится как запасная.
        let untagged = [track(None)];
        assert_eq!(
            auto_select(&untagged, &preferred(&["ru"])).for_enabled,
            Some(0)
        );
    }

    #[test]
    fn ordinary_track_beats_sdh_of_the_same_language_regardless_of_order() {
        let ru = language("ru");
        let sdh = TrackTraits {
            is_hearing_impaired: true,
            ..track(Some(&ru))
        };
        let tracks = [sdh, track(Some(&ru))];
        assert_eq!(
            auto_select(&tracks, &preferred(&["ru"])).for_enabled,
            Some(1)
        );
        // Единственная SDH-дорожка языка всё равно выбирается.
        assert_eq!(
            auto_select(&[sdh], &preferred(&["ru"])).for_enabled,
            Some(0)
        );
    }

    #[test]
    fn unrenderable_tracks_are_never_selected() {
        let ru = language("ru");
        let ass = TrackTraits {
            is_renderable: false,
            ..track(Some(&ru))
        };
        assert_eq!(
            auto_select(&[ass], &preferred(&["ru"])),
            AutoSelection::default(),
            "a lone ASS track yields no selection"
        );
        let en = language("en");
        let tracks = [ass, track(Some(&en))];
        assert_eq!(
            auto_select(&tracks, &preferred(&["ru"])).for_enabled,
            Some(1),
            "the ASS track is skipped even though its language matches"
        );
    }

    #[test]
    fn forced_track_is_only_the_choice_when_no_full_track_exists() {
        let (ru, en) = (language("ru"), language("en"));
        let forced_ru = TrackTraits {
            is_forced: true,
            ..track(Some(&ru))
        };
        let tracks = [forced_ru, track(Some(&en))];
        assert_eq!(
            auto_select(&tracks, &preferred(&["ru", "en"])).for_enabled,
            Some(1),
            "a full English track beats a forced Russian one"
        );
        assert_eq!(
            auto_select(&[forced_ru], &preferred(&["ru"])).for_enabled,
            Some(0)
        );
    }

    #[test]
    fn disabled_mode_shows_only_a_forced_track_of_a_preferred_language() {
        let (ru, en) = (language("ru"), language("en"));
        let forced_ru = TrackTraits {
            is_forced: true,
            ..track(Some(&ru))
        };
        let forced_en = TrackTraits {
            is_forced: true,
            ..track(Some(&en))
        };
        let tracks = [track(Some(&ru)), forced_en, forced_ru];
        let selection = auto_select(&tracks, &preferred(&["uk", "ru"]));
        assert_eq!(
            selection.forced_fallback,
            Some(2),
            "forced ru, not forced en"
        );

        // Ни одна forced-дорожка не совпала с языком — не показываем ничего.
        let no_match = auto_select(&[track(Some(&ru)), forced_en], &preferred(&["ru"]));
        assert_eq!(no_match.forced_fallback, None);
        // Нет forced вовсе.
        assert_eq!(
            auto_select(&[track(Some(&ru))], &preferred(&["ru"])).forced_fallback,
            None
        );
    }

    #[test]
    fn empty_preference_list_uses_the_system_language() {
        let (en, ru) = (language("en"), language("ru"));
        let tracks = [track(Some(&en)), track(Some(&ru))];
        let system = language("ru-RU");
        let preferred = PreferredLanguages::resolve(&[], Some(&system));
        assert_eq!(auto_select(&tracks, &preferred).for_enabled, Some(1));
        // Без языка системы и без списка срабатывает «первая дорожка».
        let nothing = PreferredLanguages::resolve(&[], None);
        assert_eq!(auto_select(&tracks, &nothing).for_enabled, Some(0));
    }

    #[test]
    fn invalid_codes_are_skipped_without_losing_the_valid_ones() {
        let owned = vec!["???".to_owned(), "ru".to_owned(), "ru".to_owned()];
        let resolved = PreferredLanguages::resolve(&owned, None);
        assert_eq!(resolved.primary_subtags, vec!["ru".to_owned()]);
    }

    #[test]
    fn no_tracks_means_no_selection() {
        assert_eq!(
            auto_select(&[], &preferred(&["ru"])),
            AutoSelection::default()
        );
    }

    #[test]
    fn sidecar_beats_embedded_and_default_flag_breaks_remaining_ties() {
        let en = language("en");
        let embedded_default = TrackTraits {
            origin: TrackOrigin::Embedded,
            is_default: true,
            ..track(Some(&en))
        };
        let embedded_plain = TrackTraits {
            origin: TrackOrigin::Embedded,
            ..track(Some(&en))
        };
        let sidecar = track(Some(&en));

        let tracks = [embedded_plain, embedded_default, sidecar];
        assert_eq!(
            auto_select(&tracks, &preferred(&["en"])).for_enabled,
            Some(2),
            "sidecar first"
        );
        assert_eq!(
            auto_select(&tracks[..2], &preferred(&["en"])).for_enabled,
            Some(1),
            "then the container default"
        );
        assert_eq!(
            auto_select(&tracks[..1], &preferred(&["en"])).for_enabled,
            Some(0)
        );
    }

    #[test]
    fn non_sdh_beats_origin_for_the_same_language() {
        let en = language("en");
        let sdh_sidecar = TrackTraits {
            is_hearing_impaired: true,
            ..track(Some(&en))
        };
        let plain_embedded = TrackTraits {
            origin: TrackOrigin::Embedded,
            ..track(Some(&en))
        };
        assert_eq!(
            auto_select(&[sdh_sidecar, plain_embedded], &preferred(&["en"])).for_enabled,
            Some(1)
        );
    }
}
