//! Субтитры страницы в переходнике (протокол 1.5): заголовок и куски документа идут сборщику
//! активного экземпляра, готовый документ — главному циклу уведомлением
//! [`IntakeNotice::PageSubtitleReady`].
//!
//! Вынесено из `translator.rs` (предел 800 строк): там остались только две строки
//! делегирования. Байты документа не касаются ни поколений, ни писателей потока.

use browser_bridge_protocol::PageSessionMessage;
use browser_bridge_session::SessionRef;
use tracing::{debug, warn};

use super::stream_port::StreamFactory;
use super::{InstancePhase, InstanceState, IntakeNotice, ProtocolTranslator};
use crate::browser_session::page_subtitles::assembly::{
    ChunkOutcome, OfferOutcome, PageSubtitleOffer,
};
use crate::browser_session::stream_queue::OwnedPageSubtitleChunk;

impl<F: StreamFactory> ProtocolTranslator<F> {
    /// `subtitle_track_offered` активного экземпляра → сборщику.
    pub(super) fn offer_page_subtitle(&mut self, message: PageSessionMessage) {
        let Some(offer) = PageSubtitleOffer::from_message(message) else {
            // Сюда приходит только `subtitle_track_offered` (так сопоставлен вызов).
            warn!("A page message that is not a subtitle offer reached the subtitle assembly");
            return;
        };
        let Some(instance) = self.open_instance_mut() else {
            return;
        };
        match instance.page_subtitles.offer(offer) {
            OfferOutcome::Opened | OfferOutcome::ReplacedUnfinished => {}
            // Причину сборщик записал в журнал; отказ касается только этого предложения.
            OfferOutcome::RefusedTooManyTracks | OfferOutcome::RefusedOverSessionBudget => {
                self.counters.page_subtitle_refusals += 1;
            }
        }
    }

    /// Кусок документа активного экземпляра → сборщику; готовый документ → главному циклу.
    pub(super) fn apply_page_subtitle_bytes(
        &mut self,
        session: &SessionRef,
        chunk: OwnedPageSubtitleChunk,
        out: &mut Vec<IntakeNotice>,
    ) {
        let Some(instance) = self.open_instance_mut() else {
            return;
        };
        match instance.page_subtitles.accept_chunk(chunk) {
            ChunkOutcome::Incomplete => {}
            ChunkOutcome::Completed(document) => {
                debug!(
                    track = document.track.get(),
                    bytes = document.bytes.len(),
                    "Page subtitle document assembled"
                );
                self.counters.page_subtitle_documents += 1;
                out.push(IntakeNotice::PageSubtitleReady {
                    session: session.clone(),
                    document: Box::new(document),
                });
            }
            // Причину сборщик уже записал в журнал; отказ касается только этой дорожки.
            ChunkOutcome::Refused(_) => self.counters.page_subtitle_refusals += 1,
        }
    }

    /// Активный экземпляр, если он ещё принимает вход (вызывается после `admit_instance`).
    fn open_instance_mut(&mut self) -> Option<&mut InstanceState> {
        self.instance
            .as_mut()
            .filter(|instance| matches!(instance.phase, InstancePhase::Open(_)))
    }
}
