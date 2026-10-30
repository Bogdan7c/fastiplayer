//! Перемотка, о которой нужно договориться со страницей.
//!
//! Источник сам байтов не качает: если цели перемотки нет на полке, перемотать
//! должна страница (её плеер скачает сегменты с T и пришлёт их). Здесь — журнал
//! этого разговора, без потоков и замков (им владеет `stream_buffer`):
//! - **исходящий запрос** «перемотай на T» ([`PageSeekRequest`]): последний
//!   побеждает — новая перемотка заменяет ещё не забранную приложением старую;
//! - **перемотка самой страницы** (её кнопки, смена озвучки у Ashdi): приложение
//!   отмечает цель (`note_page_seek`), и следующая перемотка плеера к той же цели
//!   (с допуском [`PageSeekBook::new`]) запроса не шлёт — страница уже идёт туда.

use std::time::Duration;

use crate::framing::MediaTime;

/// Просьба к странице перемотаться (приложение переводит её в протокол моста:
/// `seek{seek_id, target_us}`).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct PageSeekRequest {
    /// Куда перемотать (время показа, как у пакетов демуксера).
    pub target: Duration,
}

/// Нужно ли просить страницу после перемотки плеера.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum PageSeekNeed {
    /// Цель есть на полке всех полос: страницу не трогаем.
    OnShelf,
    /// Цели на полке нет: страница должна прислать байты с цели.
    OffShelf,
}

/// Чем закончилась запись перемотки плеера в журнал.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum PageSeekDecision {
    /// Страницу не трогаем: цель на полке.
    NotNeeded,
    /// Страница сама перематывается к этой цели — ждём её байтов без запроса.
    PageAlreadySeeking,
    /// Запрос поставлен (заменил незабранный старый, если был).
    RequestQueued,
}

/// Журнал разговора со страницей о перемотке.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) struct PageSeekBook {
    /// Незабранный запрос странице.
    pending_request: Option<PageSeekRequest>,
    /// Цель перемотки, которую начала сама страница (ещё не встреченная
    /// перемоткой плеера).
    page_target: Option<MediaTime>,
    /// Насколько цель плеера может отличаться от цели страницы, чтобы считаться
    /// той же перемоткой, мкс.
    match_tolerance_micros: i64,
}

impl PageSeekBook {
    /// Пустой журнал с допуском совпадения целей.
    pub(crate) fn new(match_tolerance: Duration) -> Self {
        Self {
            pending_request: None,
            page_target: None,
            match_tolerance_micros: i64::try_from(match_tolerance.as_micros()).unwrap_or(i64::MAX),
        }
    }

    /// Страница начала перемотку сама. Незабранный запрос источника устарел:
    /// страница уже ушла к своей цели, а плеер следом перемотается туда же.
    pub(crate) fn note_page_seek(&mut self, target: MediaTime) {
        self.pending_request = None;
        self.page_target = Some(target);
    }

    /// Перемотка плеера к `target`: решает, просить ли страницу.
    ///
    /// Отметка страницы расходуется любой перемоткой плеера: совпала — это та же
    /// перемотка, не совпала — плеер ушёл в другое место, и старая отметка
    /// больше ничего не значит.
    pub(crate) fn record_player_seek(
        &mut self,
        target: MediaTime,
        need: PageSeekNeed,
    ) -> PageSeekDecision {
        let page_target = self.page_target.take();
        if need == PageSeekNeed::OnShelf {
            // Цель на полке: незабранный старый запрос тоже больше не нужен.
            self.pending_request = None;
            return PageSeekDecision::NotNeeded;
        }
        if page_target.is_some_and(|page| self.same_target(page, target)) {
            self.pending_request = None;
            return PageSeekDecision::PageAlreadySeeking;
        }
        self.pending_request = Some(PageSeekRequest {
            target: target.to_player_time().as_duration(),
        });
        PageSeekDecision::RequestQueued
    }

    /// Забирает запрос для страницы (не более одного раза).
    pub(crate) fn take_request(&mut self) -> Option<PageSeekRequest> {
        self.pending_request.take()
    }

    /// Совпадают ли цели в пределах допуска.
    fn same_target(&self, page: MediaTime, player: MediaTime) -> bool {
        page.as_micros().abs_diff(player.as_micros()) <= self.match_tolerance_micros.unsigned_abs()
    }
}

#[cfg(test)]
mod tests {
    use std::time::Duration;

    use super::{PageSeekBook, PageSeekDecision, PageSeekNeed, PageSeekRequest};
    use crate::framing::MediaTime;

    const TOLERANCE: Duration = Duration::from_millis(250);

    fn at_ms(milliseconds: i64) -> MediaTime {
        MediaTime::from_micros(milliseconds * 1_000)
    }

    fn request_ms(milliseconds: u64) -> Option<PageSeekRequest> {
        Some(PageSeekRequest {
            target: Duration::from_millis(milliseconds),
        })
    }

    #[test]
    fn on_shelf_seek_never_asks_the_page() {
        let mut book = PageSeekBook::new(TOLERANCE);
        assert_eq!(
            book.record_player_seek(at_ms(1_000), PageSeekNeed::OnShelf),
            PageSeekDecision::NotNeeded
        );
        assert_eq!(book.take_request(), None);
    }

    #[test]
    fn latest_off_shelf_seek_wins_and_is_taken_once() {
        let mut book = PageSeekBook::new(TOLERANCE);
        assert_eq!(
            book.record_player_seek(at_ms(5_000), PageSeekNeed::OffShelf),
            PageSeekDecision::RequestQueued
        );
        book.record_player_seek(at_ms(9_000), PageSeekNeed::OffShelf);
        assert_eq!(book.take_request(), request_ms(9_000));
        assert_eq!(book.take_request(), None);
    }

    #[test]
    fn on_shelf_seek_cancels_an_untaken_request() {
        let mut book = PageSeekBook::new(TOLERANCE);
        book.record_player_seek(at_ms(5_000), PageSeekNeed::OffShelf);
        book.record_player_seek(at_ms(1_000), PageSeekNeed::OnShelf);
        assert_eq!(book.take_request(), None);
    }

    #[test]
    fn player_seek_to_the_page_target_within_tolerance_sends_nothing() {
        let mut book = PageSeekBook::new(TOLERANCE);
        book.note_page_seek(at_ms(0));
        assert_eq!(
            book.record_player_seek(at_ms(250), PageSeekNeed::OffShelf),
            PageSeekDecision::PageAlreadySeeking
        );
        assert_eq!(book.take_request(), None);
    }

    #[test]
    fn player_seek_outside_tolerance_asks_and_the_note_is_spent() {
        let mut book = PageSeekBook::new(TOLERANCE);
        book.note_page_seek(at_ms(0));
        assert_eq!(
            book.record_player_seek(at_ms(251), PageSeekNeed::OffShelf),
            PageSeekDecision::RequestQueued
        );
        assert_eq!(book.take_request(), request_ms(251));
        // Отметка израсходована: следующая перемотка к 0 снова спрашивает страницу.
        assert_eq!(
            book.record_player_seek(at_ms(0), PageSeekNeed::OffShelf),
            PageSeekDecision::RequestQueued
        );
    }

    #[test]
    fn page_seek_supersedes_an_untaken_player_request() {
        let mut book = PageSeekBook::new(TOLERANCE);
        book.record_player_seek(at_ms(5_000), PageSeekNeed::OffShelf);
        book.note_page_seek(at_ms(0));
        assert_eq!(book.take_request(), None);
    }
}
