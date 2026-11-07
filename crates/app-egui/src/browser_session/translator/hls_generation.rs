//! HLS-поколение в переводчике: нативный HLS по номеркам (протокол 1.1, сессия 10a1).
//!
//! Что здесь живёт (и только это — остальное в [`super`] и [`super::file_generation`]):
//! - `hls_source_added{generation, root}` превращает только что начатое поколение в
//!   HLS: создаётся очередь ресурсов и открыватель native HLS (через
//!   [`HlsFactory`](crate::browser_session::hls_bridge::factory::HlsFactory)), главный
//!   цикл получает открыватель и очередь запросов — так же, как файл заменяет ждущий
//!   поток сегментов того же поколения;
//! - **выбор адресата рамки вида 2.** Номера запросов у файла и у очереди ресурсов общие
//!   на сеанс, а рамка не говорит, кому она. Если текущее поколение — HLS и его очередь
//!   говорит «номер мой» (`claim_of == Ours`), рамка идёт ей; иначе — прежним путём
//!   файла, где невыданный сеансом номер — нарушение (4003), а выданный — устаревший;
//! - `resource_fetch_finished` — итог в ту же очередь по тому же правилу;
//! - нарушение договора страницей (рамки не подряд, тело больше потолка, второй итог)
//!   — сеанс закрывается кодом 4003.
//!
//! Приватность: в журнал идут только числа и виды ошибок, ни номерков, ни адресов.

use browser_bridge_protocol::{
    RangeRequestId as WireRangeRequestId, ResourceFetchFailure as WireResourceFetchFailure,
    ResourceFetchOutcome as WireResourceFetchOutcome, ResourceHandle, SessionCloseReason,
    StreamGeneration,
};
use browser_bridge_session::SessionRef;
use pushed_media_source::{
    ExternalResourceHandle, RangeIdStanding, RangeRequestId, ResourceFetchFailure,
    ResourceFetchOutcome, ResourceFetchViolation, ResourceFrameVerdict, ResourceRequestClaim,
};
use tracing::warn;

use super::file_generation::range_request_id;
use super::{
    GenerationState, GenerationStream, InstancePhase, InstanceState, ProtocolTranslator,
    StreamFactory,
};
use crate::browser_session::hls_bridge::factory::HlsWriterPort;
use crate::browser_session::intake_notice::{GenerationHandoff, GenerationSource, IntakeNotice};
use crate::browser_session::stream_queue::OwnedRangeChunk;

/// Что делать с `hls_source_added` в текущем состоянии поколения.
enum HlsStart {
    /// Поколение ещё сегментное и свежее: заменить HLS.
    Allowed,
    /// Поколение старее текущего.
    Stale,
    /// От поколения уже отказались.
    Rejected,
    /// Поколения нет, оно из будущего или уже файл/HLS: нарушение порядка.
    Violation,
}

/// Чей номер у рамки или итога.
enum ResourceRoute {
    /// Номер выдала очередь текущего HLS-поколения.
    CurrentHls,
    /// Не её (файл, прежнее поколение, выдумка) или HLS-поколения нет.
    Elsewhere,
}

impl<F: StreamFactory> ProtocolTranslator<F> {
    /// `hls_source_added`: поколение — HLS. Писатель сегментов бросается, открыватель и
    /// очередь запросов уходят главному циклу.
    pub(super) fn start_hls_generation(
        &mut self,
        session: &SessionRef,
        generation: StreamGeneration,
        root: ResourceHandle,
        out: &mut Vec<IntakeNotice>,
    ) {
        let decision = match self.instance.as_ref() {
            Some(InstanceState {
                phase: InstancePhase::Open(current),
                ..
            }) => match current {
                Some(state) if state.number == generation => match state.stream {
                    GenerationStream::Live(_) => HlsStart::Allowed,
                    GenerationStream::File(_) | GenerationStream::Hls(_) => HlsStart::Violation,
                    GenerationStream::Rejected => HlsStart::Rejected,
                },
                Some(state) if generation < state.number => HlsStart::Stale,
                _ => HlsStart::Violation,
            },
            _ => return,
        };
        match decision {
            HlsStart::Allowed => self.replace_segments_with_hls(session, generation, root, out),
            HlsStart::Stale => self.counters.stale_inputs += 1,
            HlsStart::Rejected => self.counters.rejected_generation_inputs += 1,
            HlsStart::Violation => {
                warn!(
                    generation = generation.get(),
                    "HLS source announced for a generation that cannot become HLS"
                );
                self.request_close(session, SessionCloseReason::ProtocolViolation, out);
            }
        }
    }

    /// Заменяет писателя сегментов только что начатого поколения очередью ресурсов HLS
    /// (тот же приём, что у файла: см. `replace_segments_with_file`).
    fn replace_segments_with_hls(
        &mut self,
        session: &SessionRef,
        generation: StreamGeneration,
        root: ResourceHandle,
        out: &mut Vec<IntakeNotice>,
    ) {
        let Some(InstanceState {
            phase: InstancePhase::Open(Some(state)),
            files,
            ..
        }) = self.instance.as_mut()
        else {
            return;
        };
        // Тот же выдаватель номеров, что у файлов сеанса: рамки и итоги разводит `claim_of`.
        let started = self
            .hls_factory
            .start_hls(files.session_ids(), ExternalResourceHandle::new(root.0));
        state.stream = GenerationStream::Hls(started.writer);
        out.push(IntakeNotice::NewGeneration(Box::new(GenerationHandoff {
            session: session.clone(),
            generation,
            source: GenerationSource::Hls {
                opener: started.opener,
                requests: started.requests,
            },
        })));
    }

    /// Рамка байтов вида 2: очереди текущего HLS-поколения, если номер её, иначе —
    /// прежним путём файла (там же правило «невыданный номер — 4003»).
    pub(super) fn apply_data_frame(
        &mut self,
        session: &SessionRef,
        chunk: &OwnedRangeChunk,
        out: &mut Vec<IntakeNotice>,
    ) {
        let id = range_request_id(chunk.request_id);
        if matches!(self.resource_route(id), ResourceRoute::Elsewhere) {
            self.apply_range_bytes(session, chunk, out);
            return;
        }
        let Some(writer) = self.current_hls_writer() else {
            return;
        };
        match writer.accept_bytes(id, chunk.offset, &chunk.payload) {
            Ok(ResourceFrameVerdict::Accepted) => {
                let counters = &mut self.counters.resources;
                counters.frames_accepted += 1;
                counters.bytes_accepted = counters
                    .bytes_accepted
                    .saturating_add(u64::try_from(chunk.payload.len()).unwrap_or(u64::MAX));
            }
            Ok(ResourceFrameVerdict::StaleDropped) => self.counters.resources.frames_stale += 1,
            Err(violation) => self.resource_contract_broken(session, &violation, out),
        }
    }

    /// `resource_fetch_finished` → очередь текущего HLS-поколения (то же правило адресата).
    pub(super) fn finish_resource_request(
        &mut self,
        session: &SessionRef,
        request_id: WireRangeRequestId,
        outcome: WireResourceFetchOutcome,
        out: &mut Vec<IntakeNotice>,
    ) {
        let id = range_request_id(request_id);
        let Some(writer) = self
            .current_hls_writer()
            .filter(|writer| writer.claim_of(id) == ResourceRequestClaim::Ours)
        else {
            // Не номер текущей очереди: выданный сеансом — запоздалый итог прежнего
            // поколения (или чужого вида), невыданный — выдумка страницы.
            match self.standing_of(id) {
                RangeIdStanding::Issued => self.counters.resources.frames_stale += 1,
                RangeIdStanding::NeverIssued => self.invented_range_id(session, out),
            }
            return;
        };
        match writer.finish(id, resource_outcome(outcome)) {
            Ok(ResourceFrameVerdict::Accepted) => {}
            Ok(ResourceFrameVerdict::StaleDropped) => self.counters.resources.frames_stale += 1,
            Err(violation) => self.resource_contract_broken(session, &violation, out),
        }
    }

    /// Чей номер `id`: очереди текущего HLS-поколения или нет.
    fn resource_route(&mut self, id: RangeRequestId) -> ResourceRoute {
        match self.current_hls_writer() {
            Some(writer) if writer.claim_of(id) == ResourceRequestClaim::Ours => {
                ResourceRoute::CurrentHls
            }
            _ => ResourceRoute::Elsewhere,
        }
    }

    /// Писатель очереди текущего поколения, если оно — живой HLS.
    fn current_hls_writer(&mut self) -> Option<&mut Box<dyn HlsWriterPort>> {
        match self.instance.as_mut() {
            Some(InstanceState {
                phase:
                    InstancePhase::Open(Some(GenerationState {
                        stream: GenerationStream::Hls(writer),
                        ..
                    })),
                ..
            }) => Some(writer),
            _ => None,
        }
    }

    /// Страница нарушила договор очереди ресурсов: сеанс закрывается кодом 4003.
    fn resource_contract_broken(
        &mut self,
        session: &SessionRef,
        violation: &ResourceFetchViolation,
        out: &mut Vec<IntakeNotice>,
    ) {
        // Только вид нарушения: номер запроса и смещения в журнал не идут.
        warn!(
            violation = violation_kind(violation),
            "The page broke the resource fetch contract"
        );
        self.request_close(session, SessionCloseReason::ProtocolViolation, out);
    }
}

/// Итог провода → итог очереди.
pub(super) const fn resource_outcome(outcome: WireResourceFetchOutcome) -> ResourceFetchOutcome {
    match outcome {
        WireResourceFetchOutcome::Completed { size_bytes } => ResourceFetchOutcome::Completed {
            size_bytes: size_bytes.as_bytes(),
        },
        WireResourceFetchOutcome::Failed { failure } => {
            ResourceFetchOutcome::Failed(match failure {
                WireResourceFetchFailure::NetworkError {} => ResourceFetchFailure::Network,
                WireResourceFetchFailure::HttpError { status } => ResourceFetchFailure::Http {
                    status: status.get(),
                },
                WireResourceFetchFailure::Refused {} => ResourceFetchFailure::Refused,
                WireResourceFetchFailure::TooLarge {} => ResourceFetchFailure::TooLarge,
            })
        }
    }
}

/// Вид нарушения словом для журнала (без номеров и смещений).
const fn violation_kind(violation: &ResourceFetchViolation) -> &'static str {
    match violation {
        ResourceFetchViolation::UnissuedRequest { .. } => "unissued_request",
        ResourceFetchViolation::RequestNotOpen { .. } => "request_not_open",
        ResourceFetchViolation::NonContiguousBytes { .. } => "non_contiguous_bytes",
        ResourceFetchViolation::BytesBeyondLimit { .. } => "bytes_beyond_limit",
        ResourceFetchViolation::SizeMismatch { .. } => "size_mismatch",
        ResourceFetchViolation::AlreadyFinished { .. } => "already_finished",
    }
}
