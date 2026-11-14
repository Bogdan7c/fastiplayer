//! Склейка HLS-эфира, потерявшего нить окна (браузерный мост 16d3, решение 333).
//!
//! Нить окна — общий сегмент старого и нового плейлиста. Только по нему движок знает,
//! где на шкале плеера лежат новые сегменты (`HlsLiveComponentSnapshot::refreshed`).
//! После сна машины или долгого обрыва сети окно эфира на сервере уходит вперёд
//! целиком, общего сегмента нет (`RefreshGap`), и опоры для времени больше нет.
//!
//! Решение владельца — поступать как встроенный HLS Chromium: шкала плеера идёт
//! дальше без прыжка, а новое окно ставится так, чтобы точка входа (hold-back, три
//! target duration от края — то же правило, что при первом открытии) пришлась ровно
//! туда, докуда демуксер уже дочитал. Плеер доигрывает скачанное и без паузы
//! переходит на «сейчас»: пропущенное за время сна выпадает из содержимого, а не из
//! часов. Ядро плеера, протокол моста и страница при этом ничего особенного не видят.
//!
//! Модуль чистый: он не знает ни потоков, ни координатора, ни сети. Он решает только
//! «как продолжить окно» и считает новые снимки; применяет их координатор под своей
//! блокировкой (`HlsLiveTimelineCoordinator::rejoin_live_edge`).

use std::time::Duration;

use super::open::SelectedLiveResources;
use super::reload_schedule::HlsLiveDueRenditions;
use super::snapshot::HlsLiveSegmentSnapshot;
use super::{HlsLiveComponentKind, HlsLiveComponentSnapshot, HlsLiveRefreshError, hold_back};

/// Насколько свежи плейлисты в одной попытке обновления.
///
/// Цикл обновления качает главный плейлист и отдельный звук по своим расписаниям: в
/// одной попытке один из них может быть взят из прошлого раза. Склеивать по такому
/// плейлисту нельзя — его окно устарело ровно на время сна.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) enum HlsLiveManifestFreshness {
    /// Все выбранные плейлисты скачаны в этой попытке.
    AllFetched,
    /// Хотя бы один плейлист взят из прошлой попытки.
    SomeReused,
}

/// Свежи ли плейлисты попытки: скачаны сейчас (`fetched`) или кончились `ENDLIST`.
///
/// Плейлист с `ENDLIST` больше не качается и не меняется — он свежий по определению.
/// Иначе разрыв у соседа вечно требовал бы перекачать всё (горячая петля).
pub(super) fn manifest_freshness(
    resources: &SelectedLiveResources,
    fetched: HlsLiveDueRenditions,
) -> HlsLiveManifestFreshness {
    let main_fresh = fetched.main || resources.main_media.end_list;
    let audio_fresh = resources
        .audio_media
        .as_ref()
        .is_none_or(|audio| fetched.audio || audio.end_list);
    if main_fresh && audio_fresh {
        HlsLiveManifestFreshness::AllFetched
    } else {
        HlsLiveManifestFreshness::SomeReused
    }
}

/// Как продолжить окно эфира после очередного обновления плейлиста.
#[derive(Debug)]
pub(super) enum HlsLiveRefreshContinuation {
    /// Нить цела (или восстановлена по соседнему компоненту): снимки уже на старой шкале.
    Continued {
        main: HlsLiveComponentSnapshot,
        audio: Option<HlsLiveComponentSnapshot>,
    },
    /// Нить потеряна у всех компонентов: нужна склейка у координатора.
    LostThread,
    /// Нить потеряна, но часть плейлистов в этой попытке не перекачивалась.
    /// Нужно скачать все заново и решить ещё раз.
    NeedsFreshManifests,
}

/// Итог `refreshed` одного компонента до общего решения.
enum ComponentContinuity {
    /// Общий сегмент найден, снимок уже на старой шкале.
    Anchored(HlsLiveComponentSnapshot),
    /// Общего сегмента нет.
    Gap,
}

/// Решает, как продолжить окно: обычное скольжение, выравнивание по соседу или склейка.
///
/// Любая ошибка, кроме `RefreshGap`, остаётся ошибкой: откат нумерации, подмена
/// сегмента и прочие нарушения RFC по-прежнему роковые (перезапуск сервера эфира —
/// решение 335, бэклог).
pub(super) fn plan_refresh_continuation(
    previous_main: &HlsLiveComponentSnapshot,
    previous_audio: Option<&HlsLiveComponentSnapshot>,
    next: &SelectedLiveResources,
    freshness: HlsLiveManifestFreshness,
) -> Result<HlsLiveRefreshContinuation, HlsLiveRefreshError> {
    let main =
        component_continuity(previous_main.refreshed(&next.main_media, next.main_plan.clone()))?;
    let audio = match (
        previous_audio,
        next.audio_media.as_ref(),
        next.audio_plan.as_ref(),
    ) {
        (Some(previous), Some(media), Some(plan)) => Some(component_continuity(
            previous.refreshed(media, plan.clone()),
        )?),
        (None, None, None) => None,
        _ => return Err(HlsLiveRefreshError::ComponentPairingChanged),
    };

    let any_gap =
        matches!(main, ComponentContinuity::Gap) || matches!(audio, Some(ComponentContinuity::Gap));
    if any_gap && freshness == HlsLiveManifestFreshness::SomeReused {
        return Ok(HlsLiveRefreshContinuation::NeedsFreshManifests);
    }

    match (main, audio) {
        (ComponentContinuity::Anchored(main), None) => {
            Ok(HlsLiveRefreshContinuation::Continued { main, audio: None })
        }
        (ComponentContinuity::Anchored(main), Some(ComponentContinuity::Anchored(audio))) => {
            Ok(HlsLiveRefreshContinuation::Continued {
                main,
                audio: Some(audio),
            })
        }
        // Нить потеряна только у звука: его окно ставим краем к краю главного компонента.
        // Это то же предположение, что при первом открытии: края свежих плейлистов
        // разных rendition-ов отвечают одному моменту эфира.
        (ComponentContinuity::Anchored(main), Some(ComponentContinuity::Gap)) => {
            let (Some(media), Some(plan)) = (next.audio_media.as_ref(), next.audio_plan.clone())
            else {
                return Err(HlsLiveRefreshError::ComponentPairingChanged);
            };
            let audio = HlsLiveComponentSnapshot::initial(
                HlsLiveComponentKind::AlternateAudio,
                media,
                plan,
                main.manifest_live_edge,
            )?;
            Ok(HlsLiveRefreshContinuation::Continued {
                main,
                audio: Some(audio),
            })
        }
        // Зеркальный случай: нить потеряна только у главного компонента.
        (ComponentContinuity::Gap, Some(ComponentContinuity::Anchored(audio))) => {
            let main = HlsLiveComponentSnapshot::initial(
                HlsLiveComponentKind::Main,
                &next.main_media,
                next.main_plan.clone(),
                audio.manifest_live_edge,
            )?;
            Ok(HlsLiveRefreshContinuation::Continued {
                main,
                audio: Some(audio),
            })
        }
        (ComponentContinuity::Gap, None | Some(ComponentContinuity::Gap)) => {
            Ok(HlsLiveRefreshContinuation::LostThread)
        }
    }
}

fn component_continuity(
    refreshed: Result<HlsLiveComponentSnapshot, HlsLiveRefreshError>,
) -> Result<ComponentContinuity, HlsLiveRefreshError> {
    match refreshed {
        Ok(snapshot) => Ok(ComponentContinuity::Anchored(snapshot)),
        Err(HlsLiveRefreshError::RefreshGap) => Ok(ComponentContinuity::Gap),
        Err(error) => Err(error),
    }
}

/// Общий край первого открытия: длиннейший из выбранных плейлистов.
///
/// Так ни у одного компонента начало окна не уходит левее нуля шкалы. Этим же правилом
/// пользуется первое открытие (`open.rs`), поэтому вход в эфир и склейка выравнивают
/// компоненты одинаково.
pub(super) fn initial_shared_edge(resources: &SelectedLiveResources) -> Duration {
    resources.main_plan.duration.max(
        resources
            .audio_plan
            .as_ref()
            .map_or(Duration::ZERO, |plan| plan.duration),
    )
}

/// Строит снимки всех компонентов так, чтобы их окна кончались на общем крае.
pub(super) fn edge_aligned_snapshots(
    resources: &SelectedLiveResources,
    shared_edge: Duration,
) -> Result<(HlsLiveComponentSnapshot, Option<HlsLiveComponentSnapshot>), HlsLiveRefreshError> {
    let main = HlsLiveComponentSnapshot::initial(
        HlsLiveComponentKind::Main,
        &resources.main_media,
        resources.main_plan.clone(),
        shared_edge,
    )?;
    let audio = match (resources.audio_media.as_ref(), resources.audio_plan.clone()) {
        (Some(media), Some(plan)) => Some(HlsLiveComponentSnapshot::initial(
            HlsLiveComponentKind::AlternateAudio,
            media,
            plan,
            shared_edge,
        )?),
        (None, None) => None,
        _ => return Err(HlsLiveRefreshError::ComponentPairingChanged),
    };
    Ok((main, audio))
}

/// Новые снимки склейки: точка входа нового окна стоит ровно на `resume_at`.
///
/// Сначала окна выравниваются по краю, как при первом открытии, и по ним считается
/// точка входа (hold-back). Затем всё окно сдвигается целиком так, чтобы эта точка
/// совпала с `resume_at` — местом, докуда демуксер уже дочитал. Сдвиг равномерный,
/// поэтому правило hold-back на сдвинутом окне даёт ту же точку.
///
/// Если `resume_at` раньше точки входа невыровненного окна (эфир моложе своего окна),
/// сдвигать влево некуда: шкала не уходит ниже нуля. Тогда окно остаётся как при первом
/// открытии, и между прочитанным и новым входом будет короткая дыра — плеер её
/// переживает (часы передаются монотонным, как у разрыва звука).
pub(super) fn rejoined_snapshots(
    resources: &SelectedLiveResources,
    resume_at: Duration,
) -> Result<HlsLiveRejoinedSnapshots, HlsLiveRefreshError> {
    let unshifted_edge = initial_shared_edge(resources);
    let (main, audio) = edge_aligned_snapshots(resources, unshifted_edge)?;
    let unshifted_join = hold_back::join_position(&main, audio.as_ref());
    let Some(shift) = resume_at.checked_sub(unshifted_join) else {
        return Ok(HlsLiveRejoinedSnapshots {
            main,
            audio,
            join_position: unshifted_join,
        });
    };
    let shifted_edge = unshifted_edge
        .checked_add(shift)
        .ok_or(HlsLiveRefreshError::TimelineOverflow)?;
    let (main, audio) = edge_aligned_snapshots(resources, shifted_edge)?;
    let join_position = hold_back::join_position(&main, audio.as_ref());
    Ok(HlsLiveRejoinedSnapshots {
        main,
        audio,
        join_position,
    })
}

/// Снимки склейки вместе с точкой, с которой плеер продолжит новое окно.
#[derive(Debug)]
pub(super) struct HlsLiveRejoinedSnapshots {
    pub main: HlsLiveComponentSnapshot,
    pub audio: Option<HlsLiveComponentSnapshot>,
    /// Начало сегмента входа нового окна на шкале плеера.
    pub join_position: Duration,
}

/// Сегмент, с которого продолжать, когда прочитанного сегмента в окне больше нет.
///
/// Сначала — первый сегмент, начинающийся не раньше прочитанного: так метки времени
/// никогда не идут назад. В обычном скольжении окна сегменты стыкуются встык, и это тот
/// же сегмент, что и раньше. После склейки (решение 333) сегменты нового окна стоят на
/// шкале произвольно относительно старого, и без этого правила демуксер мог бы взять
/// сегмент, перекрывающий уже прочитанное. Если такого нет — первый, который кончается
/// позже прочитанного (прежнее правило, «отстали от окна»).
pub(super) fn first_segment_after(
    snapshot: &HlsLiveComponentSnapshot,
    read_until: Duration,
) -> Option<&HlsLiveSegmentSnapshot> {
    snapshot
        .segments
        .iter()
        .find(|segment| segment.timeline_start >= read_until)
        .or_else(|| {
            snapshot
                .segments
                .iter()
                .find(|segment| segment.timeline_end > read_until)
        })
}

#[cfg(test)]
#[path = "rejoin_tests.rs"]
mod tests;
