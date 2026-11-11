//! Ответы переходника на ошибки входа: отказ от поколения (ровно один на поколение) или
//! просьба закрыть сеанс; после просьбы вход экземпляра выбрасывается до `Ended`.
//!
//! Вынесено из `translator.rs` (предел 800 строк) без изменения поведения.

use browser_bridge_protocol::{SessionCloseReason, StreamGeneration};
use browser_bridge_session::SessionRef;

use super::stream_port::StreamFactory;
use super::{
    GenerationRoute, GenerationStream, InstancePhase, InstanceState, IntakeNotice,
    ProtocolTranslator,
};
use crate::browser_session::rejection::FailureResponse;

impl<F: StreamFactory> ProtocolTranslator<F> {
    /// Ответ на ошибку входа, если вход не устарел (у устаревшего поколения ответа нет).
    pub(super) fn respond_unless_stale(
        &mut self,
        session: &SessionRef,
        generation: StreamGeneration,
        response: FailureResponse,
        out: &mut Vec<IntakeNotice>,
    ) {
        match self.route(generation) {
            GenerationRoute::Live(_) => self.apply_response(session, generation, response, out),
            GenerationRoute::Rejected => self.counters.rejected_generation_inputs += 1,
            GenerationRoute::Stale => self.counters.stale_inputs += 1,
            GenerationRoute::Violation | GenerationRoute::NotSegments => {
                self.request_close(session, SessionCloseReason::ProtocolViolation, out);
            }
        }
    }

    /// Исполняет ответ: отказ от поколения (ровно один) или закрытие сеанса.
    pub(super) fn apply_response(
        &mut self,
        session: &SessionRef,
        generation: StreamGeneration,
        response: FailureResponse,
        out: &mut Vec<IntakeNotice>,
    ) {
        match response {
            FailureResponse::RejectGeneration(reason) => {
                if let Some(InstanceState {
                    phase: InstancePhase::Open(Some(state)),
                    ..
                }) = self.instance.as_mut()
                    && state.number == generation
                    && !matches!(state.stream, GenerationStream::Rejected)
                {
                    // Писатель бросается: мёртвый поток ничего больше не примет.
                    state.stream = GenerationStream::Rejected;
                    out.push(IntakeNotice::Reject {
                        session: session.clone(),
                        generation,
                        reason,
                    });
                }
            }
            FailureResponse::CloseSession(reason) => self.request_close(session, reason, out),
        }
    }

    /// Просит закрыть экземпляр; дальше его вход выбрасывается до `Ended`.
    pub(super) fn request_close(
        &mut self,
        session: &SessionRef,
        reason: SessionCloseReason,
        out: &mut Vec<IntakeNotice>,
    ) {
        if let Some(active) = self.instance.as_mut()
            && matches!(active.phase, InstancePhase::Open(_))
        {
            active.phase = InstancePhase::Closing;
            out.push(IntakeNotice::CloseSession {
                session: session.clone(),
                reason,
            });
        }
    }
}
