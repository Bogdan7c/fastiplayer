//! Дорожки субтитров MPD: группировка текстовых наборов по Period и описание для меню.
//!
//! Одна «дорожка» для пользователя - это один и тот же текст (язык, название, forced) во всех
//! Period, где он есть. Внутри Period из набора берётся первая Representation, которую удалось
//! спланировать.

use std::collections::HashMap;
use std::num::NonZeroUsize;

use dash_mpd_core::{DashMpd, DashTextRole};
use source_core::HttpRequestTarget;

use crate::plan::{DashTextPeriodPlan, plan_text_period};

/// Что делает две записи в разных Period одной дорожкой.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub(crate) struct DashTextTrackIdentity {
    /// Язык в нижнем регистре; `None`, если не задан.
    pub(crate) language: Option<String>,
    /// Название из `Label`.
    pub(crate) label: Option<String>,
    /// Роль `forced-subtitle`.
    pub(crate) forced: bool,
    /// Номер среди одинаковых по трём полям наборов ОДНОГО Period (0 для первого).
    pub(crate) ordinal: u32,
}

/// Спланированная дорожка: идентичность, роли и планы по Period.
#[derive(Clone)]
pub(crate) struct DashTextTrackPlan {
    pub(crate) identity: DashTextTrackIdentity,
    /// Язык как в манифесте (для показа).
    pub(crate) language: Option<String>,
    /// Объединение ролей всех наборов дорожки.
    pub(crate) roles: Vec<DashTextRole>,
    /// Планы по Period в порядке следования; Period, где дорожки нет или она не планируется,
    /// отсутствуют.
    pub(crate) periods: Vec<DashTextPeriodPlan>,
}

/// Все дорожки одного снимка MPD.
#[derive(Clone, Default)]
pub(crate) struct DashTextPlanSet {
    pub(crate) tracks: Vec<DashTextTrackPlan>,
}

impl DashTextPlanSet {
    /// Дорожка по идентичности (в свежем снимке live она могла исчезнуть).
    pub(crate) fn find(&self, identity: &DashTextTrackIdentity) -> Option<&DashTextTrackPlan> {
        self.tracks.iter().find(|track| &track.identity == identity)
    }
}

/// Строит план всех текстовых дорожек MPD; неспланированные наборы молча пропускаются
/// (субтитры никогда не мешают воспроизведению).
pub(crate) fn build_text_plan_set(
    mpd: &DashMpd,
    manifest_base: &HttpRequestTarget,
    maximum_segments: NonZeroUsize,
) -> DashTextPlanSet {
    let mut tracks: Vec<DashTextTrackPlan> = Vec::new();
    for period in &mpd.periods {
        let mut ordinals: HashMap<(Option<String>, Option<String>, bool), u32> = HashMap::new();
        for set in &period.text_adaptation_sets {
            let planned = set.representations.iter().find_map(|representation| {
                plan_text_period(
                    mpd,
                    manifest_base,
                    period,
                    set,
                    representation,
                    maximum_segments,
                )
                .ok()
                .map(|plan| (representation, plan))
            });
            let Some((representation, plan)) = planned else {
                continue;
            };
            let forced = set.roles.contains(&DashTextRole::ForcedSubtitle);
            let language = normalized(representation.language.as_deref());
            let label = normalized(representation.label.as_deref());
            let key = (
                language.as_ref().map(|value| value.to_lowercase()),
                label.clone(),
                forced,
            );
            let ordinal = ordinals.entry(key.clone()).or_insert(0);
            let identity = DashTextTrackIdentity {
                language: key.0,
                label: key.1,
                forced,
                ordinal: *ordinal,
            };
            *ordinal = ordinal.saturating_add(1);
            match tracks.iter_mut().find(|track| track.identity == identity) {
                Some(track) => {
                    track.periods.push(plan);
                    for role in set.roles.iter() {
                        if !track.roles.contains(role) {
                            track.roles.push(*role);
                        }
                    }
                }
                None => tracks.push(DashTextTrackPlan {
                    identity,
                    language,
                    roles: set.roles.to_vec(),
                    periods: vec![plan],
                }),
            }
        }
    }
    DashTextPlanSet { tracks }
}

/// Пустая строка и пробелы по краям - «не задано».
fn normalized(value: Option<&str>) -> Option<String> {
    value
        .map(str::trim)
        .filter(|trimmed| !trimmed.is_empty())
        .map(str::to_owned)
}

/// Описание дорожки для меню (без идентификатора).
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct DashTextTrackDescription {
    pub(crate) language: Option<String>,
    pub(crate) name: String,
    pub(crate) forced: bool,
    pub(crate) default: bool,
}

/// Описывает дорожки: название (Label, иначе по роли), уникальность имён, признаки.
///
/// `default` - роль `main` у не-forced дорожки; если такой нет, первая не-forced дорожка.
pub(crate) fn describe_tracks(plan: &DashTextPlanSet) -> Vec<DashTextTrackDescription> {
    let mut descriptions: Vec<DashTextTrackDescription> = plan
        .tracks
        .iter()
        .map(|track| DashTextTrackDescription {
            language: track.language.clone(),
            name: base_name(track),
            forced: track.identity.forced,
            default: !track.identity.forced && track.roles.contains(&DashTextRole::Main),
        })
        .collect();
    if !descriptions.iter().any(|description| description.default)
        && let Some(first) = descriptions
            .iter_mut()
            .find(|description| !description.forced)
    {
        first.default = true;
    }
    make_names_unique(&mut descriptions);
    descriptions
}

/// Название без учёта повторов: Label, иначе по роли.
fn base_name(track: &DashTextTrackPlan) -> String {
    if let Some(label) = &track.identity.label {
        return label.clone();
    }
    if track.identity.forced {
        "Forced".to_owned()
    } else if track.roles.contains(&DashTextRole::Caption) {
        "Captions".to_owned()
    } else {
        "Subtitles".to_owned()
    }
}

/// Одинаковые (язык, название) получают номер: «Subtitles», «Subtitles (2)».
fn make_names_unique(descriptions: &mut [DashTextTrackDescription]) {
    let mut seen: HashMap<(Option<String>, String), u32> = HashMap::new();
    for description in descriptions.iter_mut() {
        let key = (
            description
                .language
                .as_ref()
                .map(|value| value.to_lowercase()),
            description.name.clone(),
        );
        let count = seen.entry(key).or_insert(0);
        *count = count.saturating_add(1);
        if *count > 1 {
            description.name = format!("{} ({})", description.name, count);
        }
    }
}
