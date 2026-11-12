//! `play` страницы после конца ролика не повторяет его с начала (сессия 16a2, решение 311).
//!
//! **Корень дефекта.** Серия 1 доиграла (`Ended`), страница берёт следующую серию тем же
//! элементом и говорит `page_playback{play}` — часто ДО того, как объявит новое поколение
//! (у MSE объявление — первое событие журнала, `source_buffer_added`), а если и после, то
//! ждущему поколению `play` не достаётся: его получает загруженное. Ядро плеера исполняет
//! `play` у доигравшего медиа как «повтор с начала» (`Ended` или доигрывание хвоста после
//! EOF — `player-core` `should_replay_from_eof_on_play`): зритель видел и слышал ~0,1 с
//! начала ПРОШЛОЙ серии перед новой. Для обычного плеера это правильно (кнопка «играть»
//! после конца — повтор), поэтому правило живёт здесь, в браузерном сеансе, а не в ядре.
//!
//! **Правило.** `play` СТРАНИЦЫ, пока плеер показывает медиа, доигравшее до конца, только
//! записывает волю «играть» (`page_wants_play`), плееру команды нет. Играть начнёт то, что
//! придёт следом:
//! - новое поколение: загрузка начинается так, как хочет страница (`LoadStart`), а при
//!   показе плеер ещё раз приводится к её воле (`install_reconcile`);
//! - перемотка самой страницы: настоящий повтор тень всегда шлёт явной перемоткой
//!   (`ShadowTimeline.play("page")` при `ended` делает `page_seek{0}` перед `play`), а
//!   перемотка загруженного поколения сама приводит плеер к воле страницы.
//!
//! `play` ЗРИТЕЛЯ в нашем окне (кнопка, хоткей, MPRIS) правило не трогает: для зрителя
//! «играть» после конца — повтор, как в обычном плеере (`viewer_transport`).
//!
//! **Как плеер знает «доиграл до конца».** По наблюдению, которое медиа-часть и так видит
//! (`PlaybackObservation::state` последнего прохода): `Ended` или `Draining` (доигрывание
//! хвоста после EOF; доигрывание в ядре активно ровно в состоянии `Draining`). Внутрь ядра
//! правило не смотрит. Наблюдение — с прошлого прохода, поэтому оно устаревает, если после
//! него плееру уже отдана команда транспорта одной пачкой сообщений страницы
//! ([`TransportSinceObservation`]): «перемотка + play» уводит плеер от конца при любом
//! состоянии (и долг перемотки `seek_handover` — тоже), «пауза + play» — только от
//! доигрывания хвоста (пауза выводит ядро из `Draining`). `Ended` после паузы остаётся
//! концом: правило не опирается на то, что ядро делает с паузой у конца.
//!
//! Наблюдение «у конца» берётся у того медиа, которое плеер показывает СЕЙЧАС, а не только у
//! загруженного поколения: пока новое поколение ставится, команды достаются прежнему медиа,
//! и `play` перезапустил бы доигравшую серию так же.
//!
//! **Перемотка страницы после конца (16a3, находка 3, вариант Б владельца).** Сайты сбрасывают
//! доигравший элемент на ноль ПЕРЕД сменой источника: klonua (Ashdi) в том же такте, что
//! `ended`, ставит `currentTime = 0`, через ~10 мс снимает источник и ставит новый. Тень честно
//! шлёт `page_seek{0}` для старого источника. Раньше плеер исполнял её у доигравшего медиа, а
//! перемотка загруженного поколения приводит плеер к воле страницы — а воля после конца
//! по-прежнему «играть» (браузер ставит доигравший элемент на паузу, но плеер этого не знает).
//! Итог: прошлая серия играла с начала со звуком, пока не приходила следующая (до 5 с живьём).
//!
//! **Правило.** Перемотка СТРАНИЦЫ, пока показанное поколение доиграло (`Ended`), плееру не
//! уходит — откладывается ([`HeldPageSeek`], одна, последняя побеждает). Окно держит последний
//! кадр. Дальше одно из трёх:
//! - страница говорит `play` у того же поколения — это настоящий повтор (тень так и шлёт
//!   повтор: `page_seek{0}` + `play`): перемотка и «играть» уходят плееру;
//! - приходит новое поколение или страница говорит «жду следующую серию» — отложенная перемотка
//!   выбрасывается: она была про доигравшую серию;
//! - плееру уходит любая команда зрителя или любая перемотка — отложенная устарела, выбрасывается.
//!
//! Доигрывание хвоста после EOF (`Draining`) перемотку не откладывает: страница ещё видит
//! играющий ролик, и её перемотка — настоящая.
//!
//! Пока перемотка отложена, часы страницы по-прежнему говорят «конец» (долга перемотки нет —
//! `seek_handover` его не записывал). Живые сайты в этот миг уже снимают источник; страница,
//! которая перемотала доигравший ролик и молчит, увидит его конец снова, а её `play` повторит
//! ролик с отложенной цели.

use std::time::Duration;

use browser_bridge_protocol::StreamGeneration;
use player_core::PlaybackState;
use tracing::{debug, info};

use super::SessionMedia;

/// Какие команды транспорта отданы плееру после последнего наблюдения: от них зависит,
/// правда ли ещё «у конца» этого наблюдения.
///
/// Порядок вариантов — по силе: перемотка сильнее «играть/пауза» и не ослабляется ею
/// (`seek_loaded_generation` отдаёт перемотку, а следом — «играть/пауза»).
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Default)]
pub(super) enum TransportSinceObservation {
    /// Ничего: наблюдение описывает плеер, каким он сейчас и есть.
    #[default]
    Untouched,
    /// «Играть» или «пауза»: пауза выводит ядро из доигрывания хвоста (`Draining` уже
    /// неправда), а `Ended` остаётся концом — `play` у него по-прежнему был бы повтором
    /// (решение 16a2: правило не опирается на то, что ядро делает с паузой у `Ended`).
    PlayStateCommanded,
    /// Перемотка: плеер уходит от конца к цели, «у конца» неправда при любом состоянии.
    SeekCommanded,
}

/// Где плеер относительно конца показываемого медиа (для `play` страницы).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) enum ShownMediaEnd {
    /// Медиа доиграло до конца (`Ended`): `play` ядро исполнило бы повтором с начала, а
    /// перемотку страницы плеер откладывает ([`HeldPageSeek`]).
    Ended,
    /// Медиа доигрывает хвост после EOF (`Draining`): `play` тоже был бы повтором, но
    /// перемотка страницы — настоящая (страница ещё видит играющий ролик).
    DrainingTail,
    /// Не у конца, наблюдения ещё нет, или плеер уже уведён от конца командой после
    /// наблюдения.
    NotAtEnd,
}

impl SessionMedia {
    /// Плееру отдано «играть» или «пауза» (звать при каждой такой команде).
    pub(super) fn note_play_state_commanded(&mut self) {
        self.note_transport(TransportSinceObservation::PlayStateCommanded);
    }

    /// Плееру отдана перемотка (звать при каждой перемотке загруженного поколения).
    pub(super) fn note_seek_commanded(&mut self) {
        self.note_transport(TransportSinceObservation::SeekCommanded);
    }

    /// Запоминает самую сильную команду с последнего наблюдения. Любая перемотка, отданная
    /// плееру, делает отложенную перемотку страницы устаревшей.
    fn note_transport(&mut self, commanded: TransportSinceObservation) {
        self.transport_since_observation = self.transport_since_observation.max(commanded);
        if commanded == TransportSinceObservation::SeekCommanded {
            self.drop_held_page_seek(HeldSeekDrop::PlayerSeekCommanded);
        }
    }

    /// Свежее наблюдение плеера снова описывает его целиком (звать сразу после `pump`).
    pub(super) fn note_fresh_observation(&mut self) {
        self.transport_since_observation = TransportSinceObservation::Untouched;
    }

    /// Стоит ли плеер у конца показываемого медиа (правило — в описании модуля).
    pub(super) fn shown_media_end(&self) -> ShownMediaEnd {
        // Перемотка отдана, плеер на неё ещё не ответил: наблюдение ещё про конец, а плеер
        // уже идёт к цели (и `play` в ядре станет намерением этой перемотки).
        let seek_in_flight = self
            .loaded
            .as_ref()
            .is_some_and(|loaded| loaded.unanswered_seek_target().is_some());
        if seek_in_flight
            || self.transport_since_observation == TransportSinceObservation::SeekCommanded
        {
            return ShownMediaEnd::NotAtEnd;
        }
        let observed_state = self
            .last_observation
            .as_ref()
            .map(|observation| observation.state);
        match (observed_state, self.transport_since_observation) {
            // `Ended` снимает только перемотка (она отсечена выше).
            (Some(PlaybackState::Ended), _) => ShownMediaEnd::Ended,
            // Доигрывание хвоста снимает и «играть/пауза»: пауза выводит ядро из него.
            (Some(PlaybackState::Draining), TransportSinceObservation::Untouched) => {
                ShownMediaEnd::DrainingTail
            }
            _ => ShownMediaEnd::NotAtEnd,
        }
    }

    /// `play` страницы у доигравшего медиа без отложенной перемотки: воля «играть» записана,
    /// плееру команды нет — играть начнёт следующее поколение или повтор (перемотка
    /// страницы + её `play`).
    fn keep_page_play_for_what_comes_next(&mut self) {
        self.page_wants_play = true;
        info!(
            "Page play after the end of the shown media only records the will to play; \
             the next generation or a page seek starts the playback"
        );
    }

    /// Перемотка страницы у доигравшего показанного поколения: отложена (плееру команды
    /// нет) или идёт своей дорогой. Правило — в описании модуля.
    pub(super) fn hold_page_seek_at_the_end(&mut self, target: Duration) -> PageSeekAtEnd {
        // Поколение ещё ставится: «у конца» — про прежнее медиа, а перемотка — про новое
        // (её ждёт показ, `seek_when_shown`).
        if !self.loaded_generation_is_shown() || self.shown_media_end() != ShownMediaEnd::Ended {
            return PageSeekAtEnd::NotAtEnd;
        }
        let Some(generation) = self.loaded.as_ref().map(|loaded| loaded.generation) else {
            return PageSeekAtEnd::NotAtEnd;
        };
        self.held_page_seek = Some(HeldPageSeek { generation, target });
        info!(
            generation = generation.get(),
            target_ms = target.as_millis(),
            "Page seek of the ended media is held until the page plays; the next episode drops it"
        );
        PageSeekAtEnd::Held
    }

    /// `play` страницы у доигравшего медиа: при отложенной перемотке того же поколения —
    /// настоящий повтор (перемотка + «играть»), иначе только воля для следующего поколения.
    pub(super) fn follow_page_play_at_the_end(&mut self, end: ShownMediaEnd) {
        let held_target = match end {
            ShownMediaEnd::Ended => self.take_held_page_seek_of_loaded_generation(),
            ShownMediaEnd::DrainingTail | ShownMediaEnd::NotAtEnd => None,
        };
        match held_target {
            Some(target) => {
                info!(
                    target_ms = target.as_millis(),
                    "Page replays the ended media from its held seek"
                );
                self.page_wants_play = true;
                // Перемотка загруженного поколения приводит плеер к воле страницы — «играть».
                self.seek_loaded_generation(target);
            }
            None => self.keep_page_play_for_what_comes_next(),
        }
    }

    /// Отложенная перемотка больше не про то, что покажет плеер: выброшена.
    pub(super) fn drop_held_page_seek(&mut self, why: HeldSeekDrop) {
        if let Some(held) = self.held_page_seek.take() {
            debug!(
                ?why,
                generation = held.generation.get(),
                target_ms = held.target.as_millis(),
                "Held page seek of the ended media dropped"
            );
        }
    }

    /// Забирает отложенную перемотку, если она про загруженное сейчас поколение.
    fn take_held_page_seek_of_loaded_generation(&mut self) -> Option<Duration> {
        let held = self.held_page_seek.take()?;
        let loaded_generation = self.loaded.as_ref().map(|loaded| loaded.generation);
        (loaded_generation == Some(held.generation)).then_some(held.target)
    }
}

/// Перемотка страницы, отложенная у доигравшего поколения (16a3, вариант Б).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) struct HeldPageSeek {
    /// Поколение, которое доиграло к моменту перемотки: повтор — только его.
    generation: StreamGeneration,
    /// Цель перемотки страницы.
    target: Duration,
}

/// Что стало с перемоткой страницы у конца показанного поколения.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) enum PageSeekAtEnd {
    /// Отложена до `play` страницы (или до выброса).
    Held,
    /// Не у конца: перемотка идёт обычной дорогой.
    NotAtEnd,
}

/// Почему отложенная перемотка страницы выброшена (для журнала).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) enum HeldSeekDrop {
    /// Страница объявила новое поколение.
    NewGeneration,
    /// Страница сняла источник и ждёт следующую серию.
    PageAwaitsNextEpisode,
    /// Зритель нажал «играть» или «пауза» в нашем окне.
    ViewerTransport,
    /// Плееру отдана перемотка (зрителя, повтор клика или сама отложенная).
    PlayerSeekCommanded,
    /// Открыт новый экземпляр сеанса.
    NewSession,
}
