//! Языки, которые заявляет системное лицо, и степень совпадения с запрошенным языком.
//!
//! Источники (считаются один раз при сканировании, хранятся в двух байтах):
//! 1. Таблица OpenType `meta`, запись `dlng` (design languages): `Jpan`, `Hans`, `zh-Hant-HK`...
//!    Это авторитетное объявление шрифта.
//! 2. Если `meta` нет или она ничего не говорит о CJK (у Noto Sans CJK её нет), запасная
//!    эвристика по токенам имени семейства: `JP`/`Japanese`, `KR`/`Korean`, `SC`/`CN`,
//!    `TC`/`TW`, `HK`. Так называют региональные CJK-лица Noto и многих других семейств;
//!    эвристика влияет лишь на порядок среди лиц, ЧЬЁ покрытие содержит символ.

use skrifa::raw::tables::meta::{DLNG, Meta, Metadata};

use super::language::{ChineseRegion, CjkScript, LanguageProfile, parse_language_tag};

/// Насколько лицо подходит запрошенному языку; меньше — лучше (порядок вариантов важен).
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub(super) enum LanguageMatch {
    /// Совпали письменность и регион (`zh-HK` с лицом `HK`).
    Exact,
    /// Совпала письменность/язык (`ja` с `Jpan`, `zh-Hans` с `Hans`).
    Script,
    /// Лицо не заявляет CJK-языков.
    Undeclared,
    /// Лицо заявляет только другие CJK-языки.
    Different,
}

/// Набор CJK-языков лица: маска письменностей и маска регионов традиционного китайского.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub(super) struct FaceLanguages {
    scripts: u8,
    regions: u8,
}

impl FaceLanguages {
    /// Лицо ничего не заявляет.
    pub(super) const fn undeclared() -> Self {
        Self {
            scripts: 0,
            regions: 0,
        }
    }

    /// Заявляет ли лицо хоть один CJK-язык.
    pub(super) const fn is_declared(self) -> bool {
        self.scripts != 0
    }

    /// Добавляет язык в набор.
    fn declare(&mut self, profile: LanguageProfile) {
        self.scripts |= profile.script.bit();
        if let Some(region) = profile.region {
            self.regions |= region.bit();
        }
    }

    /// Запасная эвристика по именам семейств лица (см. описание модуля).
    pub(super) fn from_family_names<'a>(names: impl IntoIterator<Item = &'a str>) -> Self {
        let mut languages = Self::undeclared();
        for name in names {
            let lowered = name.to_lowercase();
            for token in lowered.split(|c: char| !c.is_alphanumeric()) {
                if let Some(profile) = profile_of_name_token(token) {
                    languages.declare(profile);
                }
            }
        }
        languages
    }

    /// Языки из `dlng` таблицы `meta` (пусто, если записи нет).
    pub(super) fn from_meta_table(meta: &Meta<'_>) -> Self {
        let mut languages = Self::undeclared();
        for record in meta.data_maps() {
            if record.tag() != DLNG {
                continue;
            }
            // Битая запись не ошибка шрифта: лицо просто остаётся без объявления из `meta`.
            let Ok(Metadata::ScriptLangTags(tags)) = record.data(meta.offset_data()) else {
                continue;
            };
            for tag in tags.iter().filter_map(Result::ok) {
                languages.declare_tag(tag.as_str());
            }
        }
        languages
    }

    fn declare_tag(&mut self, tag: &str) {
        if let Some(profile) = parse_language_tag(tag) {
            self.declare(profile);
        }
    }

    /// Совпадение лица с запрошенным языком.
    pub(super) fn match_for(self, wanted: LanguageProfile) -> LanguageMatch {
        if !self.is_declared() {
            return LanguageMatch::Undeclared;
        }
        if self.scripts & wanted.script.bit() == 0 {
            return LanguageMatch::Different;
        }
        match wanted.region {
            Some(region) if self.regions & region.bit() != 0 => LanguageMatch::Exact,
            _ => LanguageMatch::Script,
        }
    }
}

/// Профиль по токену имени семейства (`jp`, `tc`, ...); регистр уже снижен.
fn profile_of_name_token(token: &str) -> Option<LanguageProfile> {
    let (script, region) = match token {
        "jp" | "japanese" => (CjkScript::Japanese, None),
        "kr" | "korean" => (CjkScript::Korean, None),
        "sc" | "cn" => (CjkScript::SimplifiedChinese, None),
        "tc" | "tw" => (CjkScript::TraditionalChinese, Some(ChineseRegion::Taiwan)),
        "hk" => (CjkScript::TraditionalChinese, Some(ChineseRegion::HongKong)),
        _ => return None,
    };
    Some(LanguageProfile { script, region })
}

#[cfg(test)]
mod tests {
    use skrifa::raw::{FontData, FontRead};

    use super::*;

    fn wanted(tag: &str) -> LanguageProfile {
        parse_language_tag(tag).expect("CJK tag")
    }

    /// Минимальная таблица `meta` с одной записью `dlng` и данными `value`.
    fn meta_table_bytes(value: &str) -> Vec<u8> {
        let mut bytes = Vec::new();
        bytes.extend_from_slice(&1_u32.to_be_bytes()); // version
        bytes.extend_from_slice(&0_u32.to_be_bytes()); // flags
        bytes.extend_from_slice(&0_u32.to_be_bytes()); // reserved
        bytes.extend_from_slice(&1_u32.to_be_bytes()); // dataMapsCount
        bytes.extend_from_slice(b"dlng");
        bytes.extend_from_slice(&28_u32.to_be_bytes()); // dataOffset (16 + 12)
        bytes.extend_from_slice(&(value.len() as u32).to_be_bytes());
        bytes.extend_from_slice(value.as_bytes());
        bytes
    }

    fn languages_of_meta(value: &str) -> FaceLanguages {
        let bytes = meta_table_bytes(value);
        let meta = Meta::read(FontData::new(&bytes)).expect("meta table");
        FaceLanguages::from_meta_table(&meta)
    }

    #[test]
    fn meta_dlng_declares_scripts_and_regions() {
        let japanese = languages_of_meta("Jpan");
        assert_eq!(japanese.match_for(wanted("ja")), LanguageMatch::Script);
        assert_eq!(
            japanese.match_for(wanted("zh-Hans")),
            LanguageMatch::Different
        );

        let hong_kong = languages_of_meta("zh-Hant-HK, Latn");
        assert_eq!(hong_kong.match_for(wanted("zh-HK")), LanguageMatch::Exact);
        assert_eq!(hong_kong.match_for(wanted("zh-TW")), LanguageMatch::Script);
        assert_eq!(hong_kong.match_for(wanted("ko")), LanguageMatch::Different);
    }

    #[test]
    fn meta_without_cjk_languages_stays_undeclared() {
        let latin = languages_of_meta("Latn, en");
        assert!(!latin.is_declared());
        assert_eq!(latin.match_for(wanted("ja")), LanguageMatch::Undeclared);
    }

    #[test]
    fn family_name_tokens_declare_regional_noto_faces() {
        let declared = |name: &str| FaceLanguages::from_family_names([name]);
        let japanese = declared("Noto Sans CJK JP");
        assert_eq!(japanese.match_for(wanted("ja")), LanguageMatch::Script);
        let hong_kong = declared("Noto Sans CJK HK");
        assert_eq!(hong_kong.match_for(wanted("zh-HK")), LanguageMatch::Exact);
        assert_eq!(hong_kong.match_for(wanted("ja")), LanguageMatch::Different);
        let taiwan = declared("Noto Serif CJK TC");
        assert_eq!(taiwan.match_for(wanted("zh-Hant")), LanguageMatch::Exact);
        assert_eq!(
            declared("Noto Sans CJK SC").match_for(wanted("zh")),
            LanguageMatch::Script
        );
        assert_eq!(
            declared("Noto Sans CJK KR").match_for(wanted("ko-KR")),
            LanguageMatch::Script
        );
        assert!(!declared("Source Code Pro").is_declared());
        assert!(!declared("Noto Sans").is_declared());
    }

    #[test]
    fn match_levels_are_ordered_from_best_to_worst() {
        assert!(LanguageMatch::Exact < LanguageMatch::Script);
        assert!(LanguageMatch::Script < LanguageMatch::Undeclared);
        assert!(LanguageMatch::Undeclared < LanguageMatch::Different);
    }
}
