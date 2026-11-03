//! Чьи часы видит страница в этот проход (сессия 09a2, дыра Д-4).
//!
//! Часы (`clock`) протокола 1.0 без номера поколения (решение 03b): страница сама не
//! отличит часы прежнего источника от часов нового. Поэтому медиа-часть обязана слать
//! только те часы, которые относятся к источнику, где СЕЙЧАС стоит элемент страницы.
//!
//! **Корень дефекта, который закрывает модуль.** Новый `MediaSource` страницы без её
//! перемотки (реклама посреди фильма, пересоздание плеера) — новое поколение без цели:
//! оно начнётся с первого момента, где есть данные всех полос (правило 03e). Пока его
//! данных нет, оно ждёт, а плеер всё ещё играет ПРЕЖНЕЕ поколение. Элемент страницы при
//! этом уже в новом источнике в нуле. Раньше странице шли часы прежнего поколения
//! (позиция фильма, `playing`, а после отсечки заменённого — `ended`): тень через ~2 с
//! сдавалась и принимала их, и реклама «кончалась», не начавшись (нарушение решения 84).
//!
//! **Правило.** Источник часов выбирается так (первое подходящее):
//! 1. новое поколение ждёт данных у цели (ворота 04d) — «жду данных» в самой цели;
//! 2. новый источник без цели ещё не отдан плееру — «0 / жду», как у ещё не
//!    показанного загруженного поколения (элемент страницы стоит в нуле нового
//!    источника и ждёт данных; тень по такому тику понимает, что плеер признал новый
//!    источник);
//! 3. плеер показывает загруженное поколение — его собственные часы;
//! 4. загруженное поколение ещё ставится — «0 / жду» ([`PlaybackObservation::while_media_installs`]).
//!
//! Перемотка самой страницы до открытия нового поколения (обычная смена качества,
//! цель страницы) правилом 2 НЕ накрывается намеренно: элемент уже на позиции страницы,
//! прежнее видео — то же место того же фильма, и мигать «0» странице незачем (03e,
//! `generation_placement::awaited_generation_target`).
//!
//! **Согласованные правила.** Пока действует правило 2, позиции для прощания
//! `return_to_page` тоже нет (`farewell::farewell_position` → `Unknown`): позиция фильма в
//! новом источнике отмотала бы страницу не туда. Доигрывание видит ждущее поколение как
//! `DrainContent::Opening` — то же «новое ещё не показано».

use std::borrow::Cow;
use std::time::Duration;

use super::SessionMedia;
use super::generation_placement::clock_while_generation_awaits;
use super::generation_start::GenerationStart;
use crate::browser_session::playback::PlaybackObservation;

/// Что сейчас показывает загруженное поколение (наблюдение плеера про его медиа или ещё
/// про прежнее).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) enum LoadedMediaShown {
    /// Плеер показывает медиа загруженного поколения.
    Shown,
    /// Плеер ещё ставит загруженное поколение: наблюдение про прежнее медиа.
    StillInstalling,
}

/// Источник часов странице на этот проход (правило — в описании модуля).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) enum PageClockSource {
    /// Новое поколение ждёт данных у цели: «жду данных» в цели.
    AwaitingDataAtTarget(Duration),
    /// Новый источник без цели ещё не отдан плееру: «0 / жду».
    NewSourceAwaitingData,
    /// Часы показанного поколения как есть.
    ShownGeneration,
    /// Загруженное поколение ещё ставится: «0 / жду».
    InstallingGeneration,
}

impl PageClockSource {
    /// Выбор по порядку правил модуля.
    pub(super) const fn choose(
        awaited_target: Option<Duration>,
        new_source: NewSourceStance,
        loaded: LoadedMediaShown,
    ) -> Self {
        if let Some(target) = awaited_target {
            return Self::AwaitingDataAtTarget(target);
        }
        if matches!(new_source, NewSourceStance::AwaitingDataWithoutTarget) {
            return Self::NewSourceAwaitingData;
        }
        match loaded {
            LoadedMediaShown::Shown => Self::ShownGeneration,
            LoadedMediaShown::StillInstalling => Self::InstallingGeneration,
        }
    }

    /// Наблюдение, которое уйдёт странице часами (показанное — как есть, без копии).
    pub(super) fn page_view_of(
        self,
        observation: &PlaybackObservation,
    ) -> Cow<'_, PlaybackObservation> {
        match self {
            Self::AwaitingDataAtTarget(target) => {
                Cow::Owned(clock_while_generation_awaits(observation, target))
            }
            Self::NewSourceAwaitingData | Self::InstallingGeneration => {
                Cow::Owned(observation.while_media_installs())
            }
            Self::ShownGeneration => Cow::Borrowed(observation),
        }
    }
}

/// Есть ли новый источник страницы без цели, ещё не отданный плееру.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) enum NewSourceStance {
    /// Нет: ждущего поколения нет, или у него названа цель (её часы — у ворот 04d или,
    /// для перемотки самой страницы до открытия, часы прежнего видео того же места).
    NoneOrTargeted,
    /// Да: элемент страницы стоит в нуле нового источника и ждёт данных.
    AwaitingDataWithoutTarget,
}

impl SessionMedia {
    /// Ждёт ли данных новый источник страницы без цели (правило 2 модуля).
    pub(super) fn new_source_stance(&self) -> NewSourceStance {
        let without_target = self.pending.as_ref().is_some_and(|pending| {
            pending.placement.named_start() == GenerationStart::FirstCommonData
        });
        if without_target {
            NewSourceStance::AwaitingDataWithoutTarget
        } else {
            NewSourceStance::NoneOrTargeted
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const TARGET: Duration = Duration::from_secs(12);

    /// Порядок правил: цель главнее нового источника, новый источник главнее показанного.
    #[test]
    fn the_rules_apply_in_order() {
        use LoadedMediaShown::{Shown, StillInstalling};
        use NewSourceStance::{AwaitingDataWithoutTarget, NoneOrTargeted};
        assert_eq!(
            PageClockSource::choose(Some(TARGET), AwaitingDataWithoutTarget, Shown),
            PageClockSource::AwaitingDataAtTarget(TARGET)
        );
        assert_eq!(
            PageClockSource::choose(None, AwaitingDataWithoutTarget, Shown),
            PageClockSource::NewSourceAwaitingData
        );
        assert_eq!(
            PageClockSource::choose(None, NoneOrTargeted, Shown),
            PageClockSource::ShownGeneration
        );
        assert_eq!(
            PageClockSource::choose(None, NoneOrTargeted, StillInstalling),
            PageClockSource::InstallingGeneration
        );
    }
}
