//! Склад дорожек страницы: готовые документы сеанса (решение 274).
//!
//! Инварианты:
//! - жизнь склада = жизнь сеанса: смены поколения (новый `MediaSource`, реклама, качество) его
//!   не трогают, новый сеанс в том же окне начинает новый склад с новым номером
//!   ([`PageSubtitleShelf::start_new_session`]);
//! - по номеру дорожки на складе не больше одного документа: тот же номер заменяет прежний
//!   на его месте (порядок меню = порядок первого появления дорожки), а поступление получает
//!   новый номер — по нему окно и владелец субтитров видят замену;
//! - байты не копируются: склад, снимок для окна и задача разбора делят один `Arc`.
//!
//! Число дорожек ограничено протоколом (сторож сервера, не больше 64 различных номеров за
//! сеанс), поэтому склад их отдельно не считает.

use std::sync::Arc;

use crate::page_subtitles::{
    PageSubtitleArrival, PageSubtitleDocument, PageSubtitleShelfId, PageSubtitleShelfView,
    ShelvedPageSubtitle,
};

/// Что склад сделал с документом.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum ShelfStore {
    /// Новая дорожка встала в конец.
    Added,
    /// Документ заменил прежний документ той же дорожки на его месте.
    Replaced,
}

/// Склад одного сеанса.
#[derive(Debug)]
pub(crate) struct PageSubtitleShelf {
    /// Номер склада (меняется с каждым новым сеансом).
    id: PageSubtitleShelfId,
    /// Номер следующего поступления.
    next_arrival: PageSubtitleArrival,
    /// Снимок для окна: пересобирается только при изменении и раздаётся копией `Arc`.
    view: PageSubtitleShelfView,
}

impl Default for PageSubtitleShelf {
    fn default() -> Self {
        Self {
            id: PageSubtitleShelfId::FIRST,
            next_arrival: PageSubtitleArrival::FIRST,
            view: PageSubtitleShelfView::empty(PageSubtitleShelfId::FIRST),
        }
    }
}

impl PageSubtitleShelf {
    /// Открылся новый сеанс: прежние документы к нему не относятся.
    pub(crate) fn start_new_session(&mut self) {
        self.id = self.id.next();
        self.next_arrival = PageSubtitleArrival::FIRST;
        self.view = PageSubtitleShelfView::empty(self.id);
    }

    /// Кладёт готовый документ: новая дорожка — в конец, тот же номер — на место прежней.
    pub(crate) fn store(&mut self, document: PageSubtitleDocument) -> ShelfStore {
        let arrival = self.next_arrival;
        self.next_arrival = arrival.next();
        let shelved = ShelvedPageSubtitle {
            arrival,
            document: Arc::new(document),
        };
        let mut entries = self.view.entries().to_vec();
        let outcome = match entries
            .iter_mut()
            .find(|entry| entry.document.track == shelved.document.track)
        {
            Some(previous) => {
                *previous = shelved;
                ShelfStore::Replaced
            }
            None => {
                entries.push(shelved);
                ShelfStore::Added
            }
        };
        self.view = PageSubtitleShelfView::new(self.id, entries);
        outcome
    }

    /// Снимок для окна (дешёвая копия).
    pub(crate) fn view(&self) -> PageSubtitleShelfView {
        self.view.clone()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::page_subtitles::{
        PageSubtitleDisplay, PageSubtitleFormatHint, PageSubtitleLabel, PageSubtitleLanguage,
        PageSubtitleTrackNumber,
    };

    fn document(track: u32, bytes: &[u8]) -> PageSubtitleDocument {
        PageSubtitleDocument {
            track: PageSubtitleTrackNumber::new(track),
            language: PageSubtitleLanguage::NotNamed,
            label: PageSubtitleLabel::NotGiven,
            format_hint: PageSubtitleFormatHint::Srt,
            display: PageSubtitleDisplay::NotShowingOnPage,
            bytes: Arc::from(bytes),
        }
    }

    fn tracks_and_bytes(view: &PageSubtitleShelfView) -> Vec<(u32, Vec<u8>)> {
        view.entries()
            .iter()
            .map(|entry| (entry.document.track.get(), entry.document.bytes.to_vec()))
            .collect()
    }

    #[test]
    fn the_same_track_replaces_its_document_in_place_and_keeps_the_order_of_appearance() {
        let mut shelf = PageSubtitleShelf::default();
        assert_eq!(shelf.store(document(3, b"three")), ShelfStore::Added);
        assert_eq!(shelf.store(document(1, b"one")), ShelfStore::Added);
        let before = shelf.view();
        assert_eq!(shelf.store(document(3, b"THREE")), ShelfStore::Replaced);
        let after = shelf.view();

        assert_eq!(
            tracks_and_bytes(&after),
            vec![(3, b"THREE".to_vec()), (1, b"one".to_vec())]
        );
        // Замена видна по новому поступлению: снимок сменился, номер склада прежний.
        assert_ne!(before, after);
        assert_eq!(before.shelf(), after.shelf());
        let arrivals: Vec<_> = after.entries().iter().map(|entry| entry.arrival).collect();
        assert!(
            arrivals[0] > arrivals[1],
            "the replacement is the newest arrival"
        );
    }

    #[test]
    fn a_new_session_starts_an_empty_shelf_with_a_new_identity() {
        let mut shelf = PageSubtitleShelf::default();
        shelf.store(document(1, b"one"));
        let old = shelf.view();
        shelf.start_new_session();
        let fresh = shelf.view();
        assert!(fresh.entries().is_empty());
        assert_ne!(fresh.shelf(), old.shelf());
        // Снимок, выданный раньше, свои документы не теряет (окно могло ещё не прочитать его).
        assert_eq!(tracks_and_bytes(&old), vec![(1, b"one".to_vec())]);
    }

    #[test]
    fn views_share_the_document_bytes_instead_of_copying_them() {
        let mut shelf = PageSubtitleShelf::default();
        shelf.store(document(1, b"shared"));
        let first = shelf.view();
        let second = shelf.view();
        assert!(Arc::ptr_eq(
            &first.entries()[0].document,
            &second.entries()[0].document
        ));
        assert_eq!(first, second);
    }
}
