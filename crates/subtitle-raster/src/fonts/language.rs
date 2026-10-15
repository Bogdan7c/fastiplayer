//! Языковой профиль CJK: чем отличается японский, корейский и региональные китайские формы.
//!
//! Одни и те же кодпоинты Han (например, U+76F4) рисуются в японском, упрощённом китайском,
//! традиционном (Тайвань, Гонконг) и корейском шрифтах по-разному. Системный fallback
//! (`system_fallback`) выбирает регион по языку реплики, как это делают fontconfig/libass:
//! без языка «HK раньше JP» решал бы алфавит.
//!
//! Модуль нейтрален: он разбирает ТОЛЬКО теги языка (BCP 47) и ничего не читает из окружения.
//! Язык дорожки и язык системы приносит вызывающий.

use subtitle_core::SubtitleLanguage;

/// Письменный вариант, различимый среди CJK-шрифтов.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub(crate) enum CjkScript {
    /// Японский (`ja`, `Jpan`).
    Japanese,
    /// Корейский (`ko`, `Kore`).
    Korean,
    /// Упрощённый китайский (`zh-Hans`, `zh-CN`; голое `zh` — тоже он).
    SimplifiedChinese,
    /// Традиционный китайский (`zh-Hant`, `zh-TW`, `zh-HK`).
    TraditionalChinese,
}

impl CjkScript {
    /// Бит этого варианта в маске [`super::face_language::FaceLanguages`].
    pub(super) const fn bit(self) -> u8 {
        match self {
            Self::Japanese => 1,
            Self::Korean => 2,
            Self::SimplifiedChinese => 4,
            Self::TraditionalChinese => 8,
        }
    }
}

/// Регион традиционного китайского: у Тайваня и Гонконга разные формы части иероглифов.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub(crate) enum ChineseRegion {
    /// Тайвань (`TW`; также форма по умолчанию для `zh-Hant` без региона).
    Taiwan,
    /// Гонконг и Макао (`HK`, `MO`).
    HongKong,
}

impl ChineseRegion {
    /// Бит региона в маске лица.
    pub(super) const fn bit(self) -> u8 {
        match self {
            Self::Taiwan => 1,
            Self::HongKong => 2,
        }
    }
}

/// Разобранный язык: письменный вариант и, для традиционного китайского, регион.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub(crate) struct LanguageProfile {
    pub(crate) script: CjkScript,
    pub(crate) region: Option<ChineseRegion>,
}

/// Разбирает тег языка (`ja`, `ja-JP`, `zh-Hant-HK`, `Jpan`, `jpn`, `zh_TW`).
///
/// `None` — язык не CJK или не распознан: на выбор CJK-лица он не влияет. Правила:
/// - `ja`/`jpn`, `ko`/`kor`: письменность по языку, регион игнорируется;
/// - `zh`/`zho`/`chi`/`cmn`: письменность из подтега (`Hans`/`Hant`), иначе из региона
///   (`TW`/`HK`/`MO` — традиционная, `CN`/`SG` — упрощённая), иначе упрощённая;
/// - `yue` (кантонский) без письменности — традиционная, регион Гонконг;
/// - голый подтег письменности (`Jpan`, `Kore`, `Hans`, `Hant`) — как в `dlng` таблицы `meta`;
/// - традиционная без региона получает Тайвань (как fontconfig для `zh-Hant`).
pub(crate) fn parse_language_tag(tag: &str) -> Option<LanguageProfile> {
    let lowered = tag.trim().to_ascii_lowercase();
    let mut subtags = lowered
        .split(['-', '_'])
        .filter(|subtag| !subtag.is_empty());
    let first = subtags.next()?;
    let rest: Vec<&str> = subtags.collect();
    let explicit_script = rest.iter().find_map(|subtag| script_of_subtag(subtag));
    let region = rest.iter().find_map(|subtag| region_of_subtag(subtag));
    match first {
        "ja" | "jpn" => Some(plain(CjkScript::Japanese)),
        "ko" | "kor" => Some(plain(CjkScript::Korean)),
        "zh" | "zho" | "chi" | "cmn" => Some(chinese(explicit_script, region, None)),
        "yue" => Some(chinese(
            explicit_script,
            region,
            Some(ChineseRegion::HongKong),
        )),
        // Голая письменность: `Jpan`, `Hant`, ...
        _ => script_of_subtag(first).map(|script| chinese_or_plain(script, region)),
    }
}

fn plain(script: CjkScript) -> LanguageProfile {
    LanguageProfile {
        script,
        region: None,
    }
}

/// Китайский: письменность из подтега, иначе из региона (иначе упрощённая).
fn chinese(
    explicit_script: Option<CjkScript>,
    region: Option<ChineseRegion>,
    default_traditional_region: Option<ChineseRegion>,
) -> LanguageProfile {
    let script = explicit_script.unwrap_or(
        if region.is_some() || default_traditional_region.is_some() {
            CjkScript::TraditionalChinese
        } else {
            CjkScript::SimplifiedChinese
        },
    );
    // `zh-CN`/`zh-SG`: регион упрощённого китайского нам не нужен, он не влияет на выбор лица.
    chinese_or_plain(script, region.or(default_traditional_region))
}

/// Для традиционной письменности подставляет регион по умолчанию; у остальных региона нет.
fn chinese_or_plain(script: CjkScript, region: Option<ChineseRegion>) -> LanguageProfile {
    match script {
        CjkScript::TraditionalChinese => LanguageProfile {
            script,
            region: Some(region.unwrap_or(ChineseRegion::Taiwan)),
        },
        other => plain(other),
    }
}

/// Письменность по четырёхбуквенному подтегу ISO 15924.
fn script_of_subtag(subtag: &str) -> Option<CjkScript> {
    match subtag {
        "jpan" | "kana" | "hira" => Some(CjkScript::Japanese),
        "kore" | "hang" => Some(CjkScript::Korean),
        "hans" => Some(CjkScript::SimplifiedChinese),
        "hant" => Some(CjkScript::TraditionalChinese),
        _ => None,
    }
}

/// Регион традиционного китайского по двухбуквенному подтегу региона.
fn region_of_subtag(subtag: &str) -> Option<ChineseRegion> {
    match subtag {
        "tw" => Some(ChineseRegion::Taiwan),
        "hk" | "mo" => Some(ChineseRegion::HongKong),
        _ => None,
    }
}

/// Языковое предпочтение текста для выбора CJK-лица при системном fallback.
///
/// Политика владельца: язык ДОРОЖКИ, а если он неизвестен — язык СИСТЕМЫ (как fontconfig/
/// libass). Политика собрана в одном месте ([`Self::track_then_system`]), чтобы вызывающий не
/// дублировал её. Тип дёшево копируется и хэшируется: он входит в ключ кэша поиска глифов и
/// кэша готовых реплик, поэтому смена языка не отдаёт выбор, сделанный для другого языка.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Default)]
pub struct TextLanguagePreference(Option<LanguageProfile>);

impl TextLanguagePreference {
    /// Язык неизвестен: порядок системных лиц как раньше (вес, семейство, путь).
    pub const fn unknown() -> Self {
        Self(None)
    }

    /// Предпочитать язык `language`. Не-CJK язык (например, `en`) даёт то же, что `unknown`:
    /// среди CJK-лиц ему нечего предпочесть.
    pub fn preferred(language: &SubtitleLanguage) -> Self {
        Self(parse_language_tag(language.as_str()))
    }

    /// CJK-язык дорожки; если дорожка CJK-варианта не задаёт — язык системы.
    ///
    /// Предпочтение влияет только на выбор регионального варианта иероглифов. Не-CJK язык
    /// дорожки (например, `en`) об этом ничего не говорит, поэтому для редких иероглифов в
    /// такой дорожке решает язык системы — как fontconfig выбирает по локали (решение
    /// владельца 15 октября 2026: «по языку дорожки, если неизвестно — по системе»).
    pub fn track_then_system(
        track: Option<&SubtitleLanguage>,
        system: Option<&SubtitleLanguage>,
    ) -> Self {
        let from_track = track.map_or(Self::unknown(), Self::preferred);
        if from_track.profile().is_some() {
            return from_track;
        }
        system.map_or(Self::unknown(), Self::preferred)
    }

    /// Разобранный профиль; `None` — предпочтения нет.
    pub(crate) fn profile(self) -> Option<LanguageProfile> {
        self.0
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn profile(tag: &str) -> Option<(CjkScript, Option<ChineseRegion>)> {
        parse_language_tag(tag).map(|parsed| (parsed.script, parsed.region))
    }

    #[test]
    fn japanese_and_korean_ignore_region() {
        assert_eq!(profile("ja"), Some((CjkScript::Japanese, None)));
        assert_eq!(profile("ja-JP"), Some((CjkScript::Japanese, None)));
        assert_eq!(profile("jpn"), Some((CjkScript::Japanese, None)));
        assert_eq!(profile("Jpan"), Some((CjkScript::Japanese, None)));
        assert_eq!(profile("ko_KR"), Some((CjkScript::Korean, None)));
        assert_eq!(profile("Kore"), Some((CjkScript::Korean, None)));
    }

    #[test]
    fn chinese_variants_follow_script_then_region() {
        let simplified = Some((CjkScript::SimplifiedChinese, None));
        assert_eq!(profile("zh"), simplified);
        assert_eq!(profile("zh-CN"), simplified);
        assert_eq!(profile("zh-Hans"), simplified);
        assert_eq!(profile("Hans"), simplified);
        let taiwan = Some((CjkScript::TraditionalChinese, Some(ChineseRegion::Taiwan)));
        let hong_kong = Some((CjkScript::TraditionalChinese, Some(ChineseRegion::HongKong)));
        assert_eq!(profile("zh-TW"), taiwan);
        assert_eq!(profile("zh-Hant"), taiwan);
        assert_eq!(profile("zh-HK"), hong_kong);
        assert_eq!(profile("zh-Hant-HK"), hong_kong);
        assert_eq!(profile("zh-MO"), hong_kong);
        assert_eq!(profile("yue"), hong_kong);
    }

    #[test]
    fn non_cjk_and_garbage_are_not_a_preference() {
        assert_eq!(profile("en"), None);
        assert_eq!(profile("ru-RU"), None);
        assert_eq!(profile("Latn"), None);
        assert_eq!(profile(""), None);
    }

    #[test]
    fn cjk_track_language_wins_and_non_cjk_or_unknown_track_falls_back_to_system() {
        let japanese = SubtitleLanguage::parse("ja").expect("tag");
        let english = SubtitleLanguage::parse("en").expect("tag");
        let traditional = SubtitleLanguage::parse("zh-TW").expect("tag");
        let from_track =
            TextLanguagePreference::track_then_system(Some(&japanese), Some(&traditional));
        assert_eq!(from_track, TextLanguagePreference::preferred(&japanese));
        let from_system = TextLanguagePreference::track_then_system(None, Some(&traditional));
        assert_eq!(from_system, TextLanguagePreference::preferred(&traditional));
        assert_ne!(from_track, from_system);
        // Не-CJK дорожка не задаёт вариант иероглифов: решает язык системы.
        let english_track =
            TextLanguagePreference::track_then_system(Some(&english), Some(&japanese));
        assert_eq!(english_track, TextLanguagePreference::preferred(&japanese));
        assert_eq!(
            TextLanguagePreference::track_then_system(Some(&english), None),
            TextLanguagePreference::unknown()
        );
        assert_eq!(
            TextLanguagePreference::track_then_system(None, None),
            TextLanguagePreference::unknown()
        );
    }
}
