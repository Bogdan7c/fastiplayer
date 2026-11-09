//! Сборка документов субтитров страницы из заголовка и кусков (протокол 1.5).
//!
//! Страница шлёт `subtitle_track_offered{track, …, byte_length}`, затем байты документа
//! рамками вида 3 подряд, по смещениям. Документ готов, когда пришло ровно `byte_length`
//! байт (отдельного «готово» в протоколе нет).
//!
//! Сторож протокола (`StreamGate` сервера) уже не пускает рамку без предложения, разрыв,
//! перебор длины и больше 64 дорожек — но страница враг, а сборщик не должен падать от
//! чужой ошибки. Поэтому он проверяет всё сам и при несоответствии отказывает ОДНОЙ дорожке
//! (её недокачанный документ выбрасывается, строка в журнал), остальные дорожки не трогает.
//!
//! Повторное предложение того же номера заменяет недокачанный документ; уже готовый
//! документ сборщик не хранит — его замену делает склад, когда новый документ соберётся.
//!
//! Память: под документ не резервируется объявленная длина (страница могла объявить 8 МиБ
//! и прислать байт) — буфер растёт по мере прихода кусков.
//!
//! Бюджет сеанса ([`MAX_PAGE_SUBTITLE_SESSION_BYTES`], 32 МиБ): последний готовый документ
//! каждой дорожки (то, что лежит на складе сеанса) плюс объявленные длины недокачанных — не
//! больше бюджета. Сборщик видит каждое завершение своего экземпляра, а склад живёт ровно
//! столько же (сеанс = экземпляр), поэтому учёт склада ведётся здесь и сверяется уже при
//! предложении: документ, который не поместится, не буферизуется вовсе. Замена дорожки
//! считает новый размер вместо прежнего (прежний документ остаётся на складе, пока новый не
//! докачан, — краткий перебор не больше прежних документов заменяемых дорожек). Отказ
//! касается одного предложения: ничего не выселяется.

use std::collections::HashMap;
use std::sync::Arc;

use browser_bridge_protocol::PageSessionMessage;
use browser_bridge_protocol::limits::{MAX_PAGE_SUBTITLE_SESSION_BYTES, MAX_PAGE_SUBTITLE_TRACKS};
use browser_bridge_protocol::session::{
    PageSubtitleDisplay as WireDisplay, PageSubtitleLabel as WireLabel,
    PageSubtitleLanguage as WireLanguage, SubtitleFormatHint as WireFormatHint,
};
use tracing::{debug, warn};

use crate::browser_session::stream_queue::OwnedPageSubtitleChunk;
use crate::page_subtitles::{
    PageSubtitleDisplay, PageSubtitleDocument, PageSubtitleFormatHint, PageSubtitleLabel,
    PageSubtitleLanguage, PageSubtitleTrackNumber,
};

/// Предложение дорожки: всё из `subtitle_track_offered`, что нужно плееру.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct PageSubtitleOffer {
    /// Номер дорожки.
    pub(crate) track: PageSubtitleTrackNumber,
    /// Язык.
    pub(crate) language: PageSubtitleLanguage,
    /// Подпись.
    pub(crate) label: PageSubtitleLabel,
    /// Подсказка формата.
    pub(crate) format_hint: PageSubtitleFormatHint,
    /// Показывалась ли на странице.
    pub(crate) display: PageSubtitleDisplay,
    /// Сколько байт документа объявлено.
    pub(crate) byte_length: u64,
}

impl PageSubtitleOffer {
    /// Предложение из сообщения страницы; `None` — это не `subtitle_track_offered`.
    ///
    /// Откуда экстеншен взял документ (`origin`), плееру не важно: формат и показ решает
    /// содержимое и `page_display`.
    pub(crate) fn from_message(message: PageSessionMessage) -> Option<Self> {
        let PageSessionMessage::SubtitleTrackOffered {
            track,
            language,
            label,
            format_hint,
            origin: _,
            page_display,
            byte_length,
        } = message
        else {
            return None;
        };
        Some(Self {
            track: PageSubtitleTrackNumber::new(track.get()),
            language: match language {
                WireLanguage::Unknown {} => PageSubtitleLanguage::NotNamed,
                WireLanguage::Known { tag } => {
                    PageSubtitleLanguage::Tagged(tag.as_str().to_owned())
                }
            },
            label: match label {
                WireLabel::None {} => PageSubtitleLabel::NotGiven,
                WireLabel::Text { text } => PageSubtitleLabel::Text(text.as_str().to_owned()),
            },
            format_hint: match format_hint {
                WireFormatHint::WebVtt => PageSubtitleFormatHint::WebVtt,
                WireFormatHint::Srt => PageSubtitleFormatHint::Srt,
                WireFormatHint::Ttml => PageSubtitleFormatHint::Ttml,
                WireFormatHint::Srv3 => PageSubtitleFormatHint::Srv3,
                WireFormatHint::Json3 => PageSubtitleFormatHint::Json3,
                WireFormatHint::Unknown => PageSubtitleFormatHint::Unknown,
            },
            display: match page_display {
                WireDisplay::Showing => PageSubtitleDisplay::ShowingOnPage,
                WireDisplay::NotShowing => PageSubtitleDisplay::NotShowingOnPage,
            },
            byte_length: byte_length.as_bytes(),
        })
    }
}

/// Что стало с предложением.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum OfferOutcome {
    /// Дорожка ждёт байты.
    Opened,
    /// Недокачанный документ того же номера выброшен, ждём байты нового.
    ReplacedUnfinished,
    /// Отказ: открытых дорожек уже столько, сколько разрешает протокол.
    RefusedTooManyTracks,
    /// Отказ: документ не помещается в бюджет байт сеанса (недокачанный прежний документ
    /// того же номера выброшен, готовый прежний остаётся на складе).
    RefusedOverSessionBudget,
}

/// Почему сборщик отказал дорожке на куске.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum ChunkRefusal {
    /// Для этого номера нет недокачанного предложения (не предлагали или документ уже готов).
    NotOffered,
    /// Кусок начинается не там, где кончились принятые байты.
    OffsetMismatch {
        /// Сколько байт уже принято.
        expected: u64,
        /// Откуда начинается кусок.
        received: u64,
    },
    /// Кусок выходит за объявленную длину документа.
    PastDeclaredLength,
    /// Пустой кусок (протокол такого не пропускает).
    Empty,
}

impl ChunkRefusal {
    /// Стабильная метка для журнала.
    const fn log_label(self) -> &'static str {
        match self {
            Self::NotOffered => "not_offered",
            Self::OffsetMismatch { .. } => "offset_mismatch",
            Self::PastDeclaredLength => "past_declared_length",
            Self::Empty => "empty",
        }
    }
}

/// Итог куска.
#[derive(Debug, PartialEq, Eq)]
pub(crate) enum ChunkOutcome {
    /// Кусок принят, документ ещё не весь.
    Incomplete,
    /// Кусок был последним: документ готов (сборщик его больше не держит).
    Completed(PageSubtitleDocument),
    /// Отказ этой дорожке: её недокачанный документ (если был) выброшен.
    Refused(ChunkRefusal),
}

/// Документ, который ещё качается.
struct OpenDocument {
    /// Предложение (метаданные и объявленная длина).
    offer: PageSubtitleOffer,
    /// Принятые байты (растут по мере прихода, не резервируются заранее).
    bytes: Vec<u8>,
}

/// Сборщик документов одного экземпляра сеанса.
pub(crate) struct PageSubtitleAssembly {
    /// Недокачанные документы по номеру дорожки.
    open: HashMap<PageSubtitleTrackNumber, OpenDocument>,
    /// Размер последнего готового документа каждой дорожки (то, что держит склад сеанса).
    shelved_bytes: HashMap<PageSubtitleTrackNumber, u64>,
    /// Бюджет байт сеанса (склад + недокачанное).
    session_budget_bytes: u64,
}

impl Default for PageSubtitleAssembly {
    fn default() -> Self {
        Self::with_session_budget(MAX_PAGE_SUBTITLE_SESSION_BYTES)
    }
}

impl PageSubtitleAssembly {
    /// Сборщик с бюджетом байт сеанса `session_budget_bytes` (продукт — договорной
    /// [`MAX_PAGE_SUBTITLE_SESSION_BYTES`]; тесты берут маленький).
    pub(crate) fn with_session_budget(session_budget_bytes: u64) -> Self {
        Self {
            open: HashMap::new(),
            shelved_bytes: HashMap::new(),
            session_budget_bytes,
        }
    }

    /// Новое предложение дорожки. Тот же номер заменяет недокачанный документ.
    pub(crate) fn offer(&mut self, offer: PageSubtitleOffer) -> OfferOutcome {
        let track = offer.track;
        let replaced = self.open.contains_key(&track);
        if !replaced && self.open.len() >= MAX_PAGE_SUBTITLE_TRACKS {
            warn!(
                track = track.get(),
                "Page subtitle offer refused: too many tracks are downloading at once"
            );
            return OfferOutcome::RefusedTooManyTracks;
        }
        let charged_after = self
            .charged_bytes_except(track)
            .saturating_add(offer.byte_length);
        if charged_after > self.session_budget_bytes {
            // Прежнее недокачанное этого номера страница уже бросила новым предложением.
            self.open.remove(&track);
            warn!(
                track = track.get(),
                bytes = offer.byte_length,
                budget_bytes = self.session_budget_bytes,
                "Page subtitle offer refused: the session subtitle budget is exhausted"
            );
            return OfferOutcome::RefusedOverSessionBudget;
        }
        debug!(
            track = track.get(),
            bytes = offer.byte_length,
            format_hint = offer.format_hint.log_label(),
            "Page subtitle track offered"
        );
        self.open.insert(
            track,
            OpenDocument {
                offer,
                bytes: Vec::new(),
            },
        );
        if replaced {
            OfferOutcome::ReplacedUnfinished
        } else {
            OfferOutcome::Opened
        }
    }

    /// Следующий кусок документа.
    pub(crate) fn accept_chunk(&mut self, chunk: OwnedPageSubtitleChunk) -> ChunkOutcome {
        let track = PageSubtitleTrackNumber::new(chunk.track.get());
        let Some(open) = self.open.get_mut(&track) else {
            return Self::refuse(track, ChunkRefusal::NotOffered);
        };
        let received = open.bytes.len() as u64;
        let declared = open.offer.byte_length;
        let refusal = if chunk.payload.is_empty() {
            Some(ChunkRefusal::Empty)
        } else if chunk.offset != received {
            Some(ChunkRefusal::OffsetMismatch {
                expected: received,
                received: chunk.offset,
            })
        } else if received.saturating_add(chunk.payload.len() as u64) > declared {
            Some(ChunkRefusal::PastDeclaredLength)
        } else {
            None
        };
        if let Some(refusal) = refusal {
            // Отказ дорожке: недокачанное дальше не собрать, ждать его смысла нет.
            self.open.remove(&track);
            return Self::refuse(track, refusal);
        }
        open.bytes.extend_from_slice(&chunk.payload);
        if (open.bytes.len() as u64) < declared {
            return ChunkOutcome::Incomplete;
        }
        match self.open.remove(&track) {
            Some(finished) => {
                // Готовый документ ложится на склад и заменяет там прежний этой дорожки.
                self.shelved_bytes
                    .insert(track, finished.bytes.len() as u64);
                ChunkOutcome::Completed(finished.into_document())
            }
            // Запись только что была найдена по этому ключу: сюда не попасть.
            None => ChunkOutcome::Refused(ChunkRefusal::NotOffered),
        }
    }

    /// Сколько байт сеанса занимают все дорожки, кроме `track`: недокачанная дорожка — своей
    /// объявленной длиной, остальные — последним готовым документом.
    fn charged_bytes_except(&self, track: PageSubtitleTrackNumber) -> u64 {
        let open = self
            .open
            .iter()
            .filter(|(number, _)| **number != track)
            .map(|(_, document)| document.offer.byte_length);
        let shelved_only = self
            .shelved_bytes
            .iter()
            .filter(|(number, _)| **number != track && !self.open.contains_key(number))
            .map(|(_, bytes)| *bytes);
        open.chain(shelved_only)
            .fold(0_u64, |total, bytes| total.saturating_add(bytes))
    }

    /// Сколько дорожек сейчас качается (для тестов).
    #[cfg(test)]
    pub(crate) fn open_track_count(&self) -> usize {
        self.open.len()
    }

    fn refuse(track: PageSubtitleTrackNumber, refusal: ChunkRefusal) -> ChunkOutcome {
        warn!(
            track = track.get(),
            reason = refusal.log_label(),
            "Page subtitle track refused: its bytes do not follow the offer"
        );
        ChunkOutcome::Refused(refusal)
    }
}

impl OpenDocument {
    /// Готовый документ: метаданные предложения и все байты.
    fn into_document(self) -> PageSubtitleDocument {
        PageSubtitleDocument {
            track: self.offer.track,
            language: self.offer.language,
            label: self.offer.label,
            format_hint: self.offer.format_hint,
            display: self.offer.display,
            bytes: Arc::from(self.bytes),
        }
    }
}

#[cfg(test)]
#[path = "assembly_tests.rs"]
mod tests;
