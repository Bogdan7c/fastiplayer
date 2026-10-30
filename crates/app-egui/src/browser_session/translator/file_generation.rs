//! Поколение-файл в переходнике: `<video src>` по запросам байтов (сессия 03c моста).
//!
//! Что здесь живёт (и только это — остальное в [`super`]):
//! - `file_source_added` превращает текущее поколение в файл: создаётся `pushed_file`
//!   (через [`super::file_port::FileFactory`]), главный цикл получает сторону открытия
//!   и очередь запросов;
//! - рамки байтов вида 2 и итоги запросов (`range_fetch_finished`) идут писателю
//!   текущего файла, а ошибка писателя превращается в ответ странице по классу
//!   ([`super::super::rejection::response_to_file_failure`]);
//! - рамка или итог, когда текущего файла нет, различаются по тому, выдавал ли сеанс
//!   номера запросов вообще: не выдавал — нарушение (4003), выдавал — устарели (запоздалая
//!   рамка прежнего файла после смены поколения);
//! - перевод протокол → зеркальные типы источника — чистые функции
//!   [`range_outcome`] и [`range_request_id`].
//!
//! # Как узнаём «номер, ещё не выданный этим сеансом»
//!
//! Выдаватель номеров [`RangeRequestIds`] общий на сеанс и умеет сказать, выдавался ли
//! номер ([`RangeRequestIds::standing_of`]). Когда текущего файла нет (его сменило
//! MSE-поколение или отказ), рамка или итог с невыданным номером — выдумка страницы
//! (4003, в том числе после первого файла), с выданным — запоздавший ответ прежнего
//! файла (устарел, молча).

use browser_bridge_protocol::session::page::{RangeFetchFailure, RangeFetchOutcome, ResourceSize};
use browser_bridge_protocol::{
    RangeRequestId as WireRangeRequestId, SessionCloseReason, StreamGeneration,
};
use browser_bridge_session::SessionRef;
use pushed_media_source::{
    PushedFileError, RangeBytesVerdict, RangeFailure, RangeIdStanding, RangeOutcome,
    RangeRequestId, RangeRequestIds, ResourceSizeReport,
};
use tracing::warn;

use super::file_port::FileWriterPort;
use super::{
    GenerationState, GenerationStream, InstancePhase, InstanceState, ProtocolTranslator,
    StreamFactory,
};
use crate::browser_session::intake_notice::{GenerationHandoff, GenerationSource, IntakeNotice};
use crate::browser_session::rejection::response_to_file_failure;
use crate::browser_session::stream_queue::OwnedRangeChunk;

/// Номера запросов файла одного экземпляра сеанса.
pub(super) struct SessionFiles {
    /// Выдаватель номеров, общий для всех поколений-файлов сеанса.
    ids: RangeRequestIds,
}

impl SessionFiles {
    /// Чистое состояние: номера не выдавались.
    pub(super) const fn new(ids: RangeRequestIds) -> Self {
        Self { ids }
    }
}

/// Номер запроса провода → номер источника.
pub(super) const fn range_request_id(wire: WireRangeRequestId) -> RangeRequestId {
    RangeRequestId::new(wire.0)
}

/// Итог запроса провода → итог источника.
pub(super) const fn range_outcome(outcome: RangeFetchOutcome) -> RangeOutcome {
    match outcome {
        RangeFetchOutcome::Completed { resource_size } => RangeOutcome::Completed {
            resource_size: match resource_size {
                ResourceSize::Known { size_bytes } => {
                    ResourceSizeReport::Known(size_bytes.as_bytes())
                }
                ResourceSize::Unknown {} => ResourceSizeReport::Unknown,
            },
        },
        RangeFetchOutcome::Failed { failure } => RangeOutcome::Failed(match failure {
            RangeFetchFailure::NetworkError => RangeFailure::Network,
            RangeFetchFailure::HttpError => RangeFailure::Http,
            RangeFetchFailure::ResourceChanged => RangeFailure::ResourceChanged,
            RangeFetchFailure::OutOfRange => RangeFailure::OutOfRange,
        }),
    }
}

/// Что делать с `file_source_added` в текущем состоянии поколения.
enum FileStart {
    /// Поколение ещё сегментное и свежее: заменить файлом.
    Allowed,
    /// Поколение старее текущего.
    Stale,
    /// От поколения уже отказались.
    Rejected,
    /// Поколения нет, оно из будущего или уже файл: нарушение порядка.
    Violation,
}

/// Чем кончился вызов писателя файла.
enum FileCall<T> {
    /// Писатель ответил (успехом или ошибкой) по поколению `generation`.
    Done {
        /// Поколение, которому принадлежит файл.
        generation: StreamGeneration,
        /// Ответ писателя.
        result: Result<T, PushedFileError>,
    },
    /// Поколение отвергнуто — писателя нет.
    GenerationRejected,
    /// Текущего файла нет (поколение сегментов, ещё не начатое или закрытый экземпляр).
    NoFile,
}

impl<F: StreamFactory> ProtocolTranslator<F> {
    /// `file_source_added`: поколение — файл. Писатель сегментов бросается, новый файл
    /// уходит главному циклу.
    pub(super) fn start_file_generation(
        &mut self,
        session: &SessionRef,
        generation: StreamGeneration,
        out: &mut Vec<IntakeNotice>,
    ) {
        let decision = match self.instance.as_ref() {
            Some(InstanceState {
                phase: InstancePhase::Open(current),
                ..
            }) => match current {
                Some(state) if state.number == generation => match state.stream {
                    GenerationStream::Live(_) => FileStart::Allowed,
                    GenerationStream::File(_) => FileStart::Violation,
                    GenerationStream::Rejected => FileStart::Rejected,
                },
                Some(state) if generation < state.number => FileStart::Stale,
                _ => FileStart::Violation,
            },
            _ => return,
        };
        match decision {
            FileStart::Allowed => self.replace_segments_with_file(session, generation, out),
            FileStart::Stale => self.counters.stale_inputs += 1,
            FileStart::Rejected => self.counters.rejected_generation_inputs += 1,
            FileStart::Violation => {
                warn!(
                    generation = generation.get(),
                    "File source announced for a generation that cannot become a file"
                );
                self.request_close(session, SessionCloseReason::ProtocolViolation, out);
            }
        }
    }

    /// Заменяет писателя сегментов только что начатого поколения писателем файла.
    ///
    /// Поток сегментов к этому моменту создан (`generation_started` создаёт его сразу,
    /// потому что MSE-сообщения вроде `duration_changed` могут прийти до первого
    /// SourceBuffer) и уже передан главному циклу как ждущее поколение. Тот же номер
    /// поколения, но уже файлом заменяет ждущее поколение: см. `SessionMedia`.
    fn replace_segments_with_file(
        &mut self,
        session: &SessionRef,
        generation: StreamGeneration,
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
        let started = self.file_factory.start_file(files.ids.clone());
        // Прежний писатель сегментов бросается: его поток у главного цикла заменит файл.
        // У писателя с уже объявленными SourceBuffer файлом стать нельзя: сторож
        // поколений сервера закрывает такой сеанс (`FileSourceAfterSourceBuffer`) раньше,
        // чем сообщение дойдёт сюда, — до приёмников доходит только свежее поколение.
        state.stream = GenerationStream::File(started.writer);
        out.push(IntakeNotice::NewGeneration(Box::new(GenerationHandoff {
            session: session.clone(),
            generation,
            source: GenerationSource::File {
                opener: started.opener,
                requests: started.requests,
                prefetch: started.prefetch,
            },
        })));
    }

    /// Рамка байтов вида 2 → писатель текущего файла.
    pub(super) fn apply_range_bytes(
        &mut self,
        session: &SessionRef,
        chunk: &OwnedRangeChunk,
        out: &mut Vec<IntakeNotice>,
    ) {
        let id = range_request_id(chunk.request_id);
        let call = self
            .call_file_writer(|writer| writer.accept_range_bytes(id, chunk.offset, &chunk.payload));
        match call {
            FileCall::Done {
                result: Ok(RangeBytesVerdict::Accepted),
                ..
            } => {
                self.counters.range_frames_accepted += 1;
                self.counters.range_bytes_accepted = self
                    .counters
                    .range_bytes_accepted
                    .saturating_add(u64::try_from(chunk.payload.len()).unwrap_or(u64::MAX));
                // Открывающийся файл ждёт именно этих байтов: пусть главный цикл спросит
                // сторону открытия сразу, а не по таймеру.
                Self::hint_bytes_appended(session.clone(), out);
            }
            FileCall::Done {
                result: Ok(RangeBytesVerdict::StaleDropped),
                ..
            } => self.counters.range_frames_stale += 1,
            // После `seal` сеанс уже кончился: это не нарушение страницы.
            FileCall::Done {
                result: Err(PushedFileError::WriterSealed),
                ..
            } => self.counters.range_frames_rejected += 1,
            // Отвергнутое поколение не освобождает страницу от выдуманных номеров.
            FileCall::GenerationRejected => match self.standing_of(id) {
                RangeIdStanding::Issued => self.counters.range_frames_rejected += 1,
                RangeIdStanding::NeverIssued => self.invented_range_id(session, out),
            },
            FileCall::Done {
                generation,
                result: Err(error),
            } => self.respond_to_file_failure(session, generation, &error, out),
            FileCall::NoFile => match self.standing_of(id) {
                RangeIdStanding::Issued => self.counters.range_frames_stale += 1,
                RangeIdStanding::NeverIssued => self.invented_range_id(session, out),
            },
        }
    }

    /// `range_fetch_finished` → писатель текущего файла.
    pub(super) fn finish_range_request(
        &mut self,
        session: &SessionRef,
        request_id: WireRangeRequestId,
        outcome: RangeFetchOutcome,
        out: &mut Vec<IntakeNotice>,
    ) {
        let id = range_request_id(request_id);
        let outcome = range_outcome(outcome);
        let call = self.call_file_writer(|writer| writer.finish_request(id, outcome));
        match call {
            // Итог запроса тоже может открыть файл (последний кусок начала): подсказка.
            FileCall::Done { result: Ok(()), .. } => {
                Self::hint_bytes_appended(session.clone(), out);
            }
            FileCall::Done {
                result: Err(PushedFileError::WriterSealed),
                ..
            } => self.counters.stale_inputs += 1,
            FileCall::GenerationRejected => match self.standing_of(id) {
                RangeIdStanding::Issued => self.counters.rejected_generation_inputs += 1,
                RangeIdStanding::NeverIssued => self.invented_range_id(session, out),
            },
            FileCall::Done {
                generation,
                result: Err(error),
            } => self.respond_to_file_failure(session, generation, &error, out),
            FileCall::NoFile => match self.standing_of(id) {
                RangeIdStanding::Issued => self.counters.stale_inputs += 1,
                RangeIdStanding::NeverIssued => self.invented_range_id(session, out),
            },
        }
    }

    /// Конец экземпляра: текущий поток запечатывается, чтобы полученное доигралось.
    ///
    /// - файл: `seal` — ждущие чтения не висят, чтение за полученным — конец файла;
    /// - сегменты: `end_of_stream` — иначе страница, ушедшая посреди потока, оставила
    ///   бы полку «открытой», и плеер ждал бы следующих байтов, которых не будет. Ошибка
    ///   тут не ответ странице (её уже нет): в журнал идёт только её класс.
    pub(super) fn seal_current_stream(&mut self) {
        let Some(InstanceState {
            phase: InstancePhase::Open(Some(state)),
            ..
        }) = self.instance.as_mut()
        else {
            return;
        };
        match &mut state.stream {
            GenerationStream::File(writer) => writer.seal(),
            GenerationStream::Live(writer) => {
                if let Err(error) = writer.end_of_stream() {
                    warn!(
                        failure_class = ?error.failure_class(),
                        "The stream could not be finished at the end of the session"
                    );
                }
            }
            GenerationStream::Rejected => {}
        }
    }

    /// Вызывает писателя текущего файла, если он есть.
    fn call_file_writer<T>(
        &mut self,
        call: impl FnOnce(&mut dyn FileWriterPort) -> Result<T, PushedFileError>,
    ) -> FileCall<T> {
        let Some(InstanceState {
            phase: InstancePhase::Open(current),
            ..
        }) = self.instance.as_mut()
        else {
            // Экземпляр закрывается или его нет: вход туда не доходит (см. admit).
            return FileCall::NoFile;
        };
        match current {
            Some(GenerationState {
                number,
                stream: GenerationStream::File(writer),
            }) => FileCall::Done {
                generation: *number,
                result: call(writer.as_mut()),
            },
            Some(GenerationState {
                stream: GenerationStream::Rejected,
                ..
            }) => FileCall::GenerationRejected,
            Some(GenerationState {
                stream: GenerationStream::Live(_),
                ..
            })
            | None => FileCall::NoFile,
        }
    }

    /// Выдавался ли сеансу номер `id`. Нет экземпляра — отвечать некому, считаем выданным.
    fn standing_of(&self, id: RangeRequestId) -> RangeIdStanding {
        self.instance
            .as_ref()
            .map_or(RangeIdStanding::Issued, |active| {
                active.files.ids.standing_of(id)
            })
    }

    /// Ошибка писателя файла → ответ странице по классу ошибки.
    fn respond_to_file_failure(
        &mut self,
        session: &SessionRef,
        generation: StreamGeneration,
        error: &PushedFileError,
        out: &mut Vec<IntakeNotice>,
    ) {
        // Только класс ошибки (инвариант 7): текст несёт подробности байтов страницы.
        warn!(
            failure_class = ?error.failure_class(),
            generation = generation.get(),
            "Pushed file failed"
        );
        let response = response_to_file_failure(error);
        self.apply_response(session, generation, response, out);
    }

    /// Рамка файла или итог запроса с номером, которого сеанс не выдавал: выдумка
    /// страницы — сеанс закрывается кодом 4003.
    fn invented_range_id(&mut self, session: &SessionRef, out: &mut Vec<IntakeNotice>) {
        warn!("A range frame or result carries a request number that was never issued");
        self.request_close(session, SessionCloseReason::ProtocolViolation, out);
    }
}
