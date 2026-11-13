//! Эфир HLS по номеркам глазами страницы (сессия 10a3, решение 232): окно эфира
//! странице и перемотки страницы, пришедшие раньше, чем она узнала время эфира.
//!
//! **Время эфира — время плеера.** Плеер ведёт эфир сам: ноль — начало первого
//! плейлиста, который он скачал, вход — у живого края (10a1). У страницы своё время
//! (у Chromium — от начала первого сегмента, увиденного элементом). Тень экстеншена
//! сопоставляет их по первому тику часов, пришедшему ПОСЛЕ первого `live_window`
//! поколения, и только с этого момента переводит перемотки страницы во время плеера
//! (договор пакета C 10a3). Отсюда два правила медиа-части:
//!
//! 1. **Окно эфира** (`live_window`) уходит, только пока часы странице идут от самого
//!    показанного поколения ([`PageClockSource::ShownGeneration`]): тогда окно и часы —
//!    одно время одного поколения. Пока поколение ставится, ждёт данных у цели или
//!    страница уже в новом источнике, часы говорят «чужое» место — окно в этот момент
//!    сбило бы тень. Номер поколения в сообщении — загруженного: окно относится к нему.
//!    Ролик и эфир MSE окна не получают ([`LoadedFeed::is_native_hls`]): своё окно
//!    страница видит в своих `SourceBuffer`. С 16d1 (протокол 1.11, решение 327) окно
//!    шкалы бывает и записью: эфир HLS, получивший `EXT-X-ENDLIST`, или плейлист,
//!    который к открытию плеером уже кончился, — тогда уходит `recording_window`.
//! 2. **Перемотка страницы, пока она не получила окно эфира, не исполняется.** До
//!    первого доставленного `live_window` поколения цель страницы — в её собственном
//!    времени: стартовое `page_seek{T0}` передачи тень шлёт именно так, сдвиг ей ещё
//!    неизвестен. Для плеера это не место эфира: исполненная (сразу или отложенная до
//!    показа в `seek_when_shown`), она увела бы плеер на чужое число или кончилась бы
//!    отказом `SeekTargetExpired` с сообщением зрителю на старте. Поэтому она
//!    отбрасывается с записью в журнал, а плеер остаётся у своего края. Странице ниже
//!    1.2 окно не уходит никогда, и её перемотки эфира не исполняются вовсе: она
//!    времени плеера не знает (live HLS такой странице экстеншен и не отдаёт, решение
//!    232). Клик зрителя (время плеера) — исполняется всегда.
//!
//!    Две ветки одного правила. Загруженное поколение — здесь
//!    ([`SessionMedia::page_seek_in_live_time`]). Ждущее (ещё открывается, в том числе
//!    пока в плеере играет прежнее — ролик или реклама) — цель становится его стартом
//!    (`aim_pending_at_page_seek`): live ли оно, известно только после плейлиста. Когда
//!    открытие говорит «эфир», старт страницы не исполняется (`AfterLoad::Nothing` в
//!    `generation_feed`), плеер встаёт у своего края; часы, пока эфир ставится, эхом
//!    говорят «жду в цели страницы» — это её же время, тень такие тики до первого окна
//!    не берёт.
//! 3. **Новое подключение страницы — окно заново.** Окно считается сказанным, когда
//!    ушло в очередь сервера, а обрыв мог его съесть. На каждом `Attached` учёт окна
//!    забывается (`PageOutbound::forget_live_window_on_attach`): текущее окно уходит
//!    сразу, даже у замёрзшего края, а до его доставки перемотки — в времени страницы.

use std::time::Instant;

use browser_bridge_protocol::StreamGeneration;
use tracing::info;

use super::SessionMedia;
use super::page_clock::PageClockSource;
use crate::browser_session::outbound::LiveWindowTold;
use crate::browser_session::playback::PlaybackObservation;

#[cfg(doc)]
use super::generation_feed::LoadedFeed;

/// Что делать с перемоткой страницы для загруженного поколения.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) enum PageSeekInLiveTime {
    /// Исполнять как обычно.
    Follow,
    /// Отбросить: поколение — эфир, окно которого страница ещё не получила (правило 2).
    DropBeforeLiveWindowTold,
}

impl SessionMedia {
    /// Окно эфира показанного поколения странице (правило 1 модуля).
    ///
    /// `page_clock` — чьи часы страница слышит в этом проходе БЕЗ учёта перемотки, на
    /// которую плеер ещё не ответил: такая перемотка — того же поколения и в том же
    /// времени, окно от неё не меняется.
    pub(super) fn report_live_window(
        &mut self,
        generation: StreamGeneration,
        page_clock: PageClockSource,
        observation: &PlaybackObservation,
        now: Instant,
    ) {
        if page_clock != PageClockSource::ShownGeneration {
            return;
        }
        // Окно шкалы — у любого HLS-поколения: эфир шлёт `live_window`, запись (эфир
        // кончился или плейлист был записью с открытия) — `recording_window` (решение 327).
        // Окно эфира — только у поколения, которое плеер открыл эфиром: у ролика шкала
        // эфиром не бывает, и окно эфира сбило бы тень файла.
        let Some(feed) = self.loaded.as_ref().map(|loaded| &loaded.feed) else {
            return;
        };
        let window_fits_the_feed = if observation.live_window.is_live() {
            feed.is_native_hls_live()
        } else {
            feed.is_native_hls()
        };
        if !window_fits_the_feed {
            return;
        }
        // Итог (поставлено / придержано / версия ниже 1.2) решает очередь; медиа-части
        // он не нужен: следующий проход снова сообщит свежее окно.
        let _note = self
            .outbound
            .note_live_window(generation, observation.live_window, now);
    }

    /// Исполнять ли перемотку страницы к загруженному поколению (правило 2 модуля).
    pub(super) fn page_seek_in_live_time(&self) -> PageSeekInLiveTime {
        let page_time_only = self.loaded.as_ref().is_some_and(|loaded| {
            loaded.feed.is_native_hls_live()
                && self.outbound.live_window_told(loaded.generation) == LiveWindowTold::NotYet
        });
        if page_time_only {
            info!(
                "Page seek before the page got the live window is in page time; the player stays at its live edge"
            );
            PageSeekInLiveTime::DropBeforeLiveWindowTold
        } else {
            PageSeekInLiveTime::Follow
        }
    }
}
