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
//! 4. загруженное поколение ещё ставится — «жду» там, куда страница его ждёт: в цели,
//!    если она названа, иначе в нуле ([`PlaybackObservation::while_media_installs`]).
//!
//! **Пока в плеере нет ни одного поколения сеанса (сессия 09b2).** Пульс тени (решение
//! 180) бьётся только на тиках часов, а фон кончает передачу после 10 с тишины. Раньше
//! часы начинались с первой загрузки, и медленный сервер убивал здоровую передачу
//! кодом 4003: файл открывался больше 10 с, байты шли, а тень молчала. Теперь часы идут
//! с момента, когда странице есть что сказать ([`SessionMedia::tell_page_while_nothing_is_loaded`]):
//! - старт назван — «жду» в названном месте;
//! - старт не назван, но страница в этом сеансе уже говорила, где стоит (новый источник
//!   сменил ждущее поколение), — «0 / жду»;
//! - страница ещё не говорила, где стоит, — молчим.
//!
//! **Почему не «0 / жду» с самого начала.** Первая перемотка `page_seek{T0}` сеанса — это
//! настоящая позиция элемента, и тень при передаче стоит на T0 БЕЗ удержания (hold): она
//! приняла бы тик «0» как правду. Страница увидела бы откат к нулю, а конец передачи
//! вернул бы элемент в 0 вместо T0. Первый тик уходит сразу (`ClockCadence`), поэтому он
//! легко успел бы влезть между `generation_started` и `page_seek{T0}`, хотя страница шлёт
//! их подряд. Страница шлёт `page_seek{T0}` всегда (шаг 3 договора `handoff-controller`),
//! так что ожидание ограничено. По той же причине правило 4 говорит цель, а не ноль:
//! иначе файл с T0 проваливался бы в ноль на время установки.
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
use std::time::{Duration, Instant};

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
    /// Наблюдение ещё не про место, где страница ждёт поколение (внутри — где ждёт): плеер
    /// ставит медиа, или показал его и только что получил перемотку к цели.
    StillInstalling(GenerationStart),
}

/// Назвала ли страница в этом сеансе, где стоит её элемент.
///
/// Первая перемотка сеанса — `page_seek{T0}` договора передачи. До неё плеер не знает
/// позицию страницы, и любые часы могли бы сдвинуть тень с T0 (описание модуля).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) enum PageStartWord {
    /// Ещё не называла.
    NotSaid,
    /// Назвала (цель могла смениться и потеряться со сменой поколения, но слово сказано).
    Said,
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
    /// Загруженное поколение без цели ещё ставится: «0 / жду».
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
            LoadedMediaShown::StillInstalling(GenerationStart::PageSeekedTo(target)) => {
                Self::AwaitingDataAtTarget(target)
            }
            LoadedMediaShown::StillInstalling(GenerationStart::FirstCommonData) => {
                Self::InstallingGeneration
            }
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

    /// Часы, пока в плеере нет ни одного поколения сеанса: `None` — сказать нечего (нет
    /// ждущего поколения, или страница ещё не назвала, где стоит).
    ///
    /// Ждущее поколение — всегда текущего сеанса (`on_session_opened` сбрасывает его, чужие
    /// уведомления отсекаются), поэтому часы чужого сеанса отсюда не уйдут.
    fn clock_source_while_nothing_is_loaded(&self) -> Option<PageClockSource> {
        let pending = self.pending.as_ref()?;
        match (pending.placement.named_start(), self.page_start_word) {
            (GenerationStart::PageSeekedTo(target), _) => {
                Some(PageClockSource::AwaitingDataAtTarget(target))
            }
            (GenerationStart::FirstCommonData, PageStartWord::Said) => {
                Some(PageClockSource::NewSourceAwaitingData)
            }
            (GenerationStart::FirstCommonData, PageStartWord::NotSaid) => None,
        }
    }

    /// Тик часов, пока плееру нечего показать: держит пульс тени, пока поколение
    /// открывается (решение 180). Наблюдение плеера здесь про чужое: `page_view_of`
    /// подменяет позицию и состояние, странице уходят только скорость и «жду».
    /// Ритм и номера — те же, что у всех часов (`PageOutbound::tick_clock`).
    pub(super) fn tell_page_while_nothing_is_loaded(
        &mut self,
        observation: &PlaybackObservation,
        now: Instant,
    ) {
        // Страницы нет (доигрываем) — говорить некому.
        if self.drain.is_some() {
            return;
        }
        if let Some(clock_source) = self.clock_source_while_nothing_is_loaded() {
            self.outbound
                .tick_clock(&clock_source.page_view_of(observation), now);
        }
    }

    /// Идут ли часы странице: есть загруженное поколение или открывающемуся есть что
    /// сказать. Владелец срока следующего пробуждения — `next_wake_at`.
    pub(super) fn page_clock_runs(&self) -> bool {
        self.loaded.is_some() || self.clock_source_while_nothing_is_loaded().is_some()
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
        let without_target = StillInstalling(GenerationStart::FirstCommonData);
        let at_target = StillInstalling(GenerationStart::PageSeekedTo(TARGET));
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
            PageClockSource::choose(None, NoneOrTargeted, without_target),
            PageClockSource::InstallingGeneration
        );
        // Ставящееся поколение с целью: страница ждёт его там, а не в нуле.
        assert_eq!(
            PageClockSource::choose(None, NoneOrTargeted, at_target),
            PageClockSource::AwaitingDataAtTarget(TARGET)
        );
    }
}
