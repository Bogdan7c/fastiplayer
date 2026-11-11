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

use player_core::PlaybackState;
use tracing::info;

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
    /// Медиа доиграло до конца (или доигрывает хвост после EOF): `play` ядро исполнило бы
    /// повтором с начала.
    PlayedToEnd,
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

    /// Запоминает самую сильную команду с последнего наблюдения.
    fn note_transport(&mut self, commanded: TransportSinceObservation) {
        self.transport_since_observation = self.transport_since_observation.max(commanded);
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
            (Some(PlaybackState::Ended), _)
            // Доигрывание хвоста снимает и «играть/пауза»: пауза выводит ядро из него.
            | (Some(PlaybackState::Draining), TransportSinceObservation::Untouched) => {
                ShownMediaEnd::PlayedToEnd
            }
            _ => ShownMediaEnd::NotAtEnd,
        }
    }

    /// `play` страницы у доигравшего медиа: воля «играть» записана, плееру команды нет —
    /// играть начнёт следующее поколение или перемотка страницы.
    pub(super) fn keep_page_play_for_what_comes_next(&mut self) {
        self.page_wants_play = true;
        info!(
            "Page play after the end of the shown media only records the will to play; \
             the next generation or a page seek starts the playback"
        );
    }
}
