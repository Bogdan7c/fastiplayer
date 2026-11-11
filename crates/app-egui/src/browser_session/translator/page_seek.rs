//! Перемотка в переходнике: `seek_started` страницы (`page_seek`) и подтверждение нашей
//! команды (`player_seek`).
//!
//! Вынесено из `translator.rs` (предел 800 строк) без изменения поведения.

use browser_bridge_protocol::MediaTime;
use browser_bridge_protocol::session::page::SeekCause;
use browser_bridge_session::SessionRef;
use tracing::debug;

use super::stream_port::StreamFactory;
use super::wire_convert::media_time_as_duration;
use super::{
    GenerationState, GenerationStream, InstancePhase, InstanceState, IntakeNotice, PageIntent,
    ProtocolTranslator,
};

impl<F: StreamFactory> ProtocolTranslator<F> {
    /// Перемотка: страница сама (`page_seek`) или по нашей команде (`player_seek`).
    pub(super) fn apply_seek_started(
        &mut self,
        session: &SessionRef,
        target: MediaTime,
        cause: SeekCause,
        out: &mut Vec<IntakeNotice>,
    ) {
        match cause {
            SeekCause::PageSeek {} => {
                self.note_page_seek_in_wire_order(target);
                out.push(IntakeNotice::PageIntent {
                    session: session.clone(),
                    intent: PageIntent::FollowPageSeek(target),
                });
            }
            SeekCause::PlayerSeek { seek_id } => out.push(IntakeNotice::PlayerSeekAcknowledged {
                session: session.clone(),
                seek_id,
                target,
            }),
        }
    }

    /// Перемотка страницей → источнику, строго в порядке кадров на проводе.
    ///
    /// Вызов обязан остаться ЗДЕСЬ, на потоке приёма: источник должен узнать о
    /// перемотке раньше байтов новой точки, которые идут следом в той же очереди,
    /// и раньше, чем главный цикл скомандует плееру `seek` к той же цели (тогда
    /// источник не попросит страницу перемотаться второй раз). Поколения нет или от
    /// него отказались — сообщать некому, перемотка плеера всё равно пройдёт.
    fn note_page_seek_in_wire_order(&mut self, target: MediaTime) {
        let Some(InstanceState {
            phase:
                InstancePhase::Open(Some(GenerationState {
                    stream: GenerationStream::Live(writer),
                    ..
                })),
            ..
        }) = self.instance.as_mut()
        else {
            debug!(
                target_us = target.as_micros(),
                "Page seek without a live stream; nothing to tell the source"
            );
            return;
        };
        writer.note_page_seek(media_time_as_duration(target));
    }
}
