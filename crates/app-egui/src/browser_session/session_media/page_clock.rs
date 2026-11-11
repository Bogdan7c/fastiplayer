//! Чьи часы видит страница в этот проход (сессия 09a2, дыра Д-4).
//!
//! Часы (`clock`) протокола 1.0 без номера поколения (решение 03b): страница сама не
//! отличит часы прежнего источника от часов нового. Поэтому медиа-часть обязана слать
//! только те часы, которые относятся к источнику, где СЕЙЧАС стоит элемент страницы.
//!
//! **Номер поколения (протокол 1.10, 16a2).** Выбор ниже называет и поколение, которое
//! описывает тик ([`PageClock`]): правила 1–2 — ждущее, правила 3–5 — загруженное (вариант
//! «жду в цели» рождается обеими дорогами, поэтому номер идёт рядом с ним). Странице 1.10+
//! номер уходит полем `generation` (`outbound::clock_generation`), странице 1.9 — нет.
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
//! 3. плеер ещё не ответил на отданную ему перемотку ([`super::seek_handover`]) — «жду» в
//!    её цели: снимок плеера ещё про старое место;
//! 4. плеер показывает загруженное поколение — его собственные часы (пока он садится на
//!    цель, часы уже говорят цель: [`PlaybackObservation::page_clock_position`]);
//! 5. загруженное поколение ещё ставится — «жду» там, куда страница его ждёт: в цели,
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
//! так что ожидание ограничено. По той же причине правило 5 говорит цель, а не ноль:
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

use browser_bridge_protocol::StreamGeneration;

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
    /// ставит медиа.
    StillInstalling(GenerationStart),
    /// Плеер показывает медиа, но ещё не ответил на отданную ему перемотку к этой цели:
    /// наблюдение про старое место, а страница уже на цели.
    SeekNotAnswered(Duration),
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

/// Что ждущее поколение говорит часам (правила 1–2 модуля, первое подходящее).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) enum PendingClockWord {
    /// Правила 1–2 не действуют: ждущего поколения нет, или страница сама назвала ему
    /// старт, а данных у цели не ждут (смена качества до открытия: часы — загруженного).
    Silent,
    /// Правило 1: ждущее поколение ждёт данных у цели.
    AwaitsDataAtTarget {
        /// Номер ждущего поколения (его и описывает тик).
        generation: StreamGeneration,
        /// Где страница его ждёт.
        target: Duration,
    },
    /// Правило 2: новый источник без цели ещё не отдан плееру.
    AwaitsDataWithoutTarget(StreamGeneration),
}

impl PendingClockWord {
    /// Слово ждущего поколения `generation`: цель главнее «нового источника без цели».
    pub(super) const fn of(
        generation: StreamGeneration,
        awaited_target: Option<Duration>,
        new_source: NewSourceStance,
    ) -> Self {
        if let Some(target) = awaited_target {
            return Self::AwaitsDataAtTarget { generation, target };
        }
        match new_source {
            NewSourceStance::AwaitingDataWithoutTarget => Self::AwaitsDataWithoutTarget(generation),
            NewSourceStance::NoneOrTargeted => Self::Silent,
        }
    }
}

/// Загруженное поколение для часов (правила 3–5 модуля).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) struct LoadedClockWord {
    /// Номер загруженного поколения.
    pub(super) generation: StreamGeneration,
    /// Что из него сейчас показывает плеер.
    pub(super) shown: LoadedMediaShown,
}

/// Часы странице на этот проход: что сказать и про какое поколение (номер уходит
/// странице 1.10+ полем `generation`, `PageOutbound::tick_clock`).
///
/// «Жду в цели» рождается и у ждущего поколения (правило 1), и у загруженного (правила 3
/// и 5), поэтому номер идёт рядом с источником, а не выводится из него.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) struct PageClock {
    /// Что сказать.
    pub(super) source: PageClockSource,
    /// Какое поколение описывает тик.
    pub(super) generation: StreamGeneration,
}

impl PageClock {
    /// Выбор по порядку правил модуля: слово ждущего поколения главнее загруженного.
    pub(super) const fn choose(pending: PendingClockWord, loaded: LoadedClockWord) -> Self {
        match pending {
            PendingClockWord::AwaitsDataAtTarget { generation, target } => Self {
                source: PageClockSource::AwaitingDataAtTarget(target),
                generation,
            },
            PendingClockWord::AwaitsDataWithoutTarget(generation) => Self {
                source: PageClockSource::NewSourceAwaitingData,
                generation,
            },
            PendingClockWord::Silent => Self {
                source: Self::loaded_source(loaded.shown),
                generation: loaded.generation,
            },
        }
    }

    /// Правила 3–5: часы загруженного поколения.
    const fn loaded_source(shown: LoadedMediaShown) -> PageClockSource {
        match shown {
            LoadedMediaShown::Shown => PageClockSource::ShownGeneration,
            LoadedMediaShown::StillInstalling(GenerationStart::PageSeekedTo(target))
            | LoadedMediaShown::SeekNotAnswered(target) => {
                PageClockSource::AwaitingDataAtTarget(target)
            }
            LoadedMediaShown::StillInstalling(GenerationStart::FirstCommonData) => {
                PageClockSource::InstallingGeneration
            }
        }
    }

    /// Тик этих часов странице (ритм и номера — у `PageOutbound::tick_clock`).
    pub(super) fn tell(
        self,
        outbound: &mut crate::browser_session::outbound::PageOutbound,
        observation: &PlaybackObservation,
        now: Instant,
    ) {
        outbound.tick_clock(&self.source.page_view_of(observation), self.generation, now);
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
    fn clock_while_nothing_is_loaded(&self) -> Option<PageClock> {
        let pending = self.pending.as_ref()?;
        // Тик описывает ждущее поколение: другого в плеере нет.
        let generation = pending.generation;
        match (pending.placement.named_start(), self.page_start_word) {
            (GenerationStart::PageSeekedTo(target), _) => Some(PageClock {
                source: PageClockSource::AwaitingDataAtTarget(target),
                generation,
            }),
            (GenerationStart::FirstCommonData, PageStartWord::Said) => Some(PageClock {
                source: PageClockSource::NewSourceAwaitingData,
                generation,
            }),
            (GenerationStart::FirstCommonData, PageStartWord::NotSaid) => None,
        }
    }

    /// Слово ждущего поколения часам этого прохода (правила 1–2 модуля).
    pub(super) fn pending_clock_word(&self) -> PendingClockWord {
        match self.pending.as_ref() {
            Some(pending) => PendingClockWord::of(
                pending.generation,
                self.awaited_generation_target(),
                self.new_source_stance(),
            ),
            None => PendingClockWord::Silent,
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
        if let Some(clock) = self.clock_while_nothing_is_loaded() {
            clock.tell(&mut self.outbound, observation, now);
        }
    }

    /// Идут ли часы странице: есть загруженное поколение или открывающемуся есть что
    /// сказать. Владелец срока следующего пробуждения — `next_wake_at`.
    pub(super) fn page_clock_runs(&self) -> bool {
        self.loaded.is_some() || self.clock_while_nothing_is_loaded().is_some()
    }
}

#[cfg(test)]
#[path = "page_clock_tests.rs"]
mod tests;
