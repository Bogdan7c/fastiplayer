//! Документы субтитров страницы у сторожа потока (протокол 1.5, решения 270–274).
//!
//! Модуль владеет учётом предложений дорожек: сколько байт документа каждой дорожки уже
//! пришло и сколько объявлено. Версию сеанса и присутствие страницы проверяет сам сторож
//! (`StreamGate::admit_page_subtitle_bytes`) до обращения сюда; поколений потока документы
//! не касаются вовсе. Договор — `crate::session::page_subtitles`.

use std::collections::BTreeMap;

use super::{GateDecision, StreamViolation};
use crate::data_frame::PageSubtitleBytesHeader;
use crate::limits::MAX_PAGE_SUBTITLE_TRACKS;
use crate::session::page_subtitles::{PageSubtitleByteLength, PageSubtitleTrackId};

/// Сколько байт текущего предложения дорожки уже пришло и сколько обещано.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct DocumentProgress {
    /// Объявленная длина документа (`byte_length`).
    declared_bytes: u64,
    /// Сколько байт этого предложения уже принято; оно же — ожидаемое смещение следующей рамки.
    received_bytes: u64,
}

impl DocumentProgress {
    /// Документ готов: пришло ровно объявленное число байт.
    const fn is_complete(self) -> bool {
        self.received_bytes == self.declared_bytes
    }
}

/// Предложения дорожек субтитров одного сеанса.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(super) struct PageSubtitleOffers {
    /// Последнее предложение каждой дорожки; ключей не больше [`MAX_PAGE_SUBTITLE_TRACKS`].
    by_track: BTreeMap<PageSubtitleTrackId, DocumentProgress>,
}

impl PageSubtitleOffers {
    /// Ни одной дорожки ещё не предлагали.
    pub(super) const fn new() -> Self {
        Self {
            by_track: BTreeMap::new(),
        }
    }

    /// Новое предложение дорожки. Тот же номер — замена: прежний документ (в том числе
    /// недокачанный) забывается, байты нового идут снова с 0 и места не занимают.
    ///
    /// # Errors
    /// [`StreamViolation::TooManyPageSubtitleTracks`] — новая дорожка сверх предела сеанса;
    /// учёт тогда не меняется.
    pub(super) fn offer(
        &mut self,
        track: PageSubtitleTrackId,
        byte_length: PageSubtitleByteLength,
    ) -> Result<GateDecision, StreamViolation> {
        let is_new_track = !self.by_track.contains_key(&track);
        if is_new_track && self.by_track.len() >= MAX_PAGE_SUBTITLE_TRACKS {
            return Err(StreamViolation::TooManyPageSubtitleTracks);
        }
        self.by_track.insert(
            track,
            DocumentProgress {
                declared_bytes: byte_length.as_bytes(),
                received_bytes: 0,
            },
        );
        Ok(GateDecision::Accept)
    }

    /// Рамка вида 3: продолжает ли она открытое предложение своей дорожки ровно с того байта,
    /// где оно остановилось, и не выходит ли за объявленную длину.
    ///
    /// # Errors
    /// Дорожку не предлагали, документ уже готов, смещение не то или рамка длиннее остатка
    /// документа. Отвергнутая рамка учёт не меняет.
    pub(super) fn admit_bytes(
        &mut self,
        header: &PageSubtitleBytesHeader,
        payload_len: usize,
    ) -> Result<GateDecision, StreamViolation> {
        let progress = self
            .by_track
            .get_mut(&header.track)
            .ok_or(StreamViolation::PageSubtitleTrackNotOffered)?;
        if progress.is_complete() {
            return Err(StreamViolation::PageSubtitleDocumentComplete);
        }
        let offset = header.offset.as_bytes();
        if offset != progress.received_bytes {
            return Err(StreamViolation::PageSubtitleOffsetGap {
                expected: progress.received_bytes,
                received: offset,
            });
        }
        // Груз рамки ≤ 1 МиБ, а документ ≤ 8 МиБ: сложение не переполняется, но без паники —
        // переполнение тоже значит «за концом документа».
        let end = u64::try_from(payload_len)
            .ok()
            .and_then(|length| offset.checked_add(length))
            .filter(|end| *end <= progress.declared_bytes)
            .ok_or(StreamViolation::PageSubtitleBeyondByteLength)?;
        progress.received_bytes = end;
        Ok(GateDecision::Accept)
    }
}
