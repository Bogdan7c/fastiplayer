//! Переходник протокола: сообщения MSE и байты страницы → вызовы источника
//! `pushed-media-source`, а всё, что решает главный цикл, — уведомления.
//!
//! Чистая логика без потоков: входы приходят по одному из единой очереди
//! ([`super::stream_queue`]) в порядке кадров на проводе, выходы — в
//! `Vec<IntakeNotice>`. Живёт на потоке приёма `browser-stream`
//! ([`super::stream_intake`]); писатель потока существует только там.
//!
//! Что держит переходник:
//! - один активный экземпляр сеанса (ключ + номер); вход нового экземпляра
//!   начинает состояние с нуля, вход старого — выбрасывается со счётом;
//! - в экземпляре — одно активное поколение (новый `MediaSource` страницы) и его
//!   писатель. Новое поколение бросает старый писатель (плеер доигрывает
//!   принятое) и открывает новый поток: главный цикл получает
//!   [`IntakeNotice::NewGeneration`] со стороной открытия и монитором;
//! - ошибки источника — по [`super::rejection`]: отказ от поколения
//!   (`stream_rejected`) не больше одного раза на поколение, после него вход этого
//!   поколения выбрасывается со счётом; нарушение порядка — просьба закрыть сеанс
//!   кодом 4003, после неё весь вход экземпляра выбрасывается до его конца;
//! - поколение бывает «сегменты MSE» (`source_buffer_added`) или «файл `<video src>`»
//!   (`file_source_added`, логика — [`file_generation`]); рамки байтов вида 2 идут
//!   писателю текущего файла, сверку с выданными запросами делает источник;
//! - конец экземпляра запечатывает текущий поток: файл — `seal`, MSE — `end_of_stream`
//!   (принятое доигрывается до конца даже у страницы, не успевшей сама закончить поток).

use browser_bridge_protocol::session::page::{PagePlaybackRequest, SeekCause};
use browser_bridge_protocol::session::player::StreamRejection;
use browser_bridge_protocol::{
    MediaDuration, MediaTime, PageSessionMessage, SessionCloseReason, SourceBufferIndex,
    StreamGeneration,
};
use browser_bridge_session::SessionRef;
use pushed_media_source::RangeRequestIds;
use pushed_media_source::{
    AppendOutcome, LaneId, PushedContainer, PushedDuration, PushedStreamError, TimestampOffset,
};
use tracing::{debug, error, info, warn};

use super::intake_notice::{GenerationHandoff, GenerationSource, IntakeNotice, PageIntent};
use super::rejection::{
    FailureResponse, response_to_stream_failure, response_to_unsupported_media_type,
};
use super::stream_queue::{OwnedChunk, SessionInput};

mod file_generation;
pub(crate) mod file_port;
pub(crate) mod stream_port;

use self::file_port::{FileFactory, FileWriterPort};
use self::stream_port::{StreamFactory, StreamWriterPort};

/// Счётчики переходника (для журнала и тестов).
#[derive(Debug, Default, Clone, Copy, PartialEq, Eq)]
pub(crate) struct TranslatorCounters {
    /// Вход старого экземпляра или старого поколения.
    pub(crate) stale_inputs: u64,
    /// Вход поколения, от которого уже отказались.
    pub(crate) rejected_generation_inputs: u64,
    /// Вход экземпляра, который уже просили закрыть.
    pub(crate) closing_instance_inputs: u64,
    /// Сообщения, которые переходник не исполняет (меню вариантов, `remove()` страницы).
    pub(crate) ignored_messages: u64,
    /// `tab_gone` страницы: переходник его только отмечает — сеанс закрывает сервер
    /// (4002), а доигрывание полученного ведёт главный цикл.
    pub(crate) tab_gone_messages: u64,
    /// Рамки файла `<video src>`, принятые источником.
    pub(crate) range_frames_accepted: u64,
    /// Байты в принятых рамках файла.
    pub(crate) range_bytes_accepted: u64,
    /// Рамки файла, не нужные больше: запрос отменён перемоткой или заменён повтором,
    /// рамка прежнего файла (источник отбросил их молча) либо пришла, когда файла нет,
    /// но номера в этом сеансе уже выдавались.
    pub(crate) range_frames_stale: u64,
    /// Рамки файла, отвергнутые без нарушения: поколение уже отвергнуто либо файл
    /// запечатан (сеанс кончился).
    pub(crate) range_frames_rejected: u64,
    /// Куски, выброшенные источником: плеер уже закрыл поток.
    pub(crate) discarded_after_consumer_closed: u64,
    /// Разбор входа упал с паникой (дефект): экземпляр закрыт.
    pub(crate) internal_failures: u64,
}

/// Поток одного поколения.
enum GenerationStream {
    /// Поток сегментов MSE жив: вход идёт в писатель.
    Live(Box<dyn StreamWriterPort>),
    /// Файл `<video src>` жив: рамки байтов и итоги запросов идут его писателю.
    File(Box<dyn FileWriterPort>),
    /// От поколения отказались: вход выбрасывается.
    Rejected,
}

/// Активное поколение экземпляра.
struct GenerationState {
    /// Номер.
    number: StreamGeneration,
    /// Поток.
    stream: GenerationStream,
}

/// Фаза экземпляра сеанса.
enum InstancePhase {
    /// Принимает вход; поколения может ещё не быть.
    Open(Option<GenerationState>),
    /// Просили закрыть: вход выбрасывается до `Ended`.
    Closing,
}

/// Активный экземпляр сеанса.
struct InstanceState {
    /// Сеанс.
    session: SessionRef,
    /// Фаза.
    phase: InstancePhase,
    /// Номера запросов файла этого сеанса (общие для всех его поколений-файлов).
    files: file_generation::SessionFiles,
}

impl InstanceState {
    /// Экземпляр в фазе `phase` с чистыми номерами запросов.
    fn new(session: SessionRef, phase: InstancePhase) -> Self {
        Self {
            session,
            phase,
            files: file_generation::SessionFiles::new(RangeRequestIds::new()),
        }
    }
}

/// Переходник протокола.
pub(crate) struct ProtocolTranslator<F: StreamFactory> {
    /// Откуда брать потоки сегментов.
    factory: F,
    /// Откуда брать файлы `<video src>`.
    file_factory: Box<dyn FileFactory>,
    /// Активный экземпляр.
    instance: Option<InstanceState>,
    /// Счётчики.
    counters: TranslatorCounters,
}

/// Что делать с входом, привязанным к поколению.
enum GenerationRoute<'state> {
    /// Отдать писателю живого поколения.
    Live(&'state mut Box<dyn StreamWriterPort>),
    /// Поколение — файл: вход MSE ему не адресован (сторож сервера такого не пускает).
    NotSegments,
    /// Поколение отвергнуто — выбросить.
    Rejected,
    /// Вход старого поколения — выбросить.
    Stale,
    /// Поколение не начиналось или из будущего — нарушение порядка.
    Violation,
}

impl<F: StreamFactory> ProtocolTranslator<F> {
    /// Переходник без активного сеанса.
    pub(crate) fn new(factory: F, file_factory: Box<dyn FileFactory>) -> Self {
        Self {
            factory,
            file_factory,
            instance: None,
            counters: TranslatorCounters::default(),
        }
    }

    /// Счётчики.
    pub(crate) const fn counters(&self) -> TranslatorCounters {
        self.counters
    }

    /// Применяет один вход в порядке очереди; уведомления — в `out`.
    pub(crate) fn apply(&mut self, input: SessionInput, out: &mut Vec<IntakeNotice>) {
        match input {
            SessionInput::Ended(session) => self.end_instance(&session, out),
            SessionInput::Message(session, message) => {
                if self.admit_instance(&session) {
                    self.apply_message(message, out);
                }
            }
            SessionInput::Bytes(session, chunk) => {
                if self.admit_instance(&session) {
                    self.apply_bytes(&chunk, out);
                }
            }
            SessionInput::RangeBytes(session, chunk) => {
                if self.admit_instance(&session) {
                    self.apply_range_bytes(&session, &chunk, out);
                }
            }
        }
    }

    /// Разбор входа экземпляра `session` упал с паникой (дефект переходника или
    /// источника): его состояние больше не заслуживает доверия.
    ///
    /// Писатель поколения бросается (поток у плеера заканчивается), экземпляр
    /// переходит в «закрывается» — его дальнейший вход выбрасывается до `Ended`, —
    /// а главный цикл получает просьбу закрыть его. Код — 4003: отдельного кода
    /// «внутренняя ошибка плеера» в протоколе нет, а 4003 — это «данные этого сеанса
    /// дальше не принимаются» (причина видна в журнале процесса). Экземпляр новее
    /// упавшего (так не бывает: старый вход отбрасывается раньше разбора) не трогается.
    pub(crate) fn close_after_internal_failure(
        &mut self,
        session: &SessionRef,
        out: &mut Vec<IntakeNotice>,
    ) {
        self.counters.internal_failures += 1;
        let newer_instance_active = self
            .instance
            .as_ref()
            .is_some_and(|active| active.session.instance > session.instance);
        if newer_instance_active {
            return;
        }
        self.instance = Some(InstanceState::new(session.clone(), InstancePhase::Closing));
        out.push(IntakeNotice::CloseSession {
            session: session.clone(),
            reason: SessionCloseReason::ProtocolViolation,
        });
    }

    // ---- экземпляры -------------------------------------------------------

    /// Делает экземпляр входа активным, если он новее; `false` — вход выбросить.
    fn admit_instance(&mut self, session: &SessionRef) -> bool {
        let verdict = match &self.instance {
            None => InstanceVerdict::StartFresh,
            Some(active) if active.session == *session => match active.phase {
                InstancePhase::Open(_) => InstanceVerdict::Current,
                InstancePhase::Closing => InstanceVerdict::Closing,
            },
            Some(active) if session.instance > active.session.instance => {
                InstanceVerdict::StartFresh
            }
            Some(_) => InstanceVerdict::Stale,
        };
        match verdict {
            InstanceVerdict::Current => true,
            InstanceVerdict::StartFresh => {
                if let Some(previous) = &self.instance {
                    // Конец прежнего экземпляра приходит раньше данных нового (сервер
                    // публикует его под замком до выдачи нового пропуска); сюда
                    // попадаем, только если конец потерялся. Состояние — с нуля.
                    warn!(
                        previous = previous.session.instance.0,
                        next = session.instance.0,
                        "Browser stream input of a newer session instance arrived before the end of the previous one"
                    );
                }
                self.instance = Some(InstanceState::new(
                    session.clone(),
                    InstancePhase::Open(None),
                ));
                true
            }
            InstanceVerdict::Closing => {
                self.counters.closing_instance_inputs += 1;
                false
            }
            InstanceVerdict::Stale => {
                self.counters.stale_inputs += 1;
                false
            }
        }
    }

    /// Экземпляр закончился: текущий поток запечатывается (плеер доигрывает принятое),
    /// писатель бросается.
    fn end_instance(&mut self, session: &SessionRef, out: &mut Vec<IntakeNotice>) {
        if self
            .instance
            .as_ref()
            .is_some_and(|active| active.session == *session)
        {
            self.seal_current_stream();
            info!(
                session = session.key.as_str(),
                instance = session.instance.0,
                "Browser stream session ended; its stream input is closed"
            );
            self.instance = None;
        }
        // Всегда, даже для экземпляра, от которого переходник ничего не видел: главный
        // цикл ждёт это уведомление, прежде чем решать, что доигрывать нечего. Оно идёт
        // следом за уведомлениями данных этого экземпляра, в порядке очереди.
        out.push(IntakeNotice::InstanceSealed {
            session: session.clone(),
        });
    }

    /// Активный сеанс (вызывается только после `admit_instance`).
    fn active_session(&self) -> Option<SessionRef> {
        self.instance.as_ref().map(|active| active.session.clone())
    }

    // ---- сообщения --------------------------------------------------------

    fn apply_message(&mut self, message: PageSessionMessage, out: &mut Vec<IntakeNotice>) {
        let Some(session) = self.active_session() else {
            return;
        };
        match message {
            PageSessionMessage::GenerationStarted { generation } => {
                self.start_generation(&session, generation, out);
            }
            PageSessionMessage::SourceBufferAdded {
                generation,
                source_buffer,
                mime_type,
            } => match PushedContainer::from_mime_type(mime_type.as_str()) {
                Ok(container) => self.apply_to_writer(&session, generation, out, |writer| {
                    writer.add_lane(lane_of(source_buffer), container)
                }),
                Err(unsupported) => {
                    // Сам MIME в журнал не идёт (инвариант 7): строку пишет страница.
                    warn!(supported = false, "Unsupported SourceBuffer type");
                    let response = response_to_unsupported_media_type(&unsupported);
                    self.respond_unless_stale(&session, generation, response, out);
                }
            },
            PageSessionMessage::SourceBufferTypeChanged {
                generation,
                source_buffer,
                mime_type,
            } => match PushedContainer::from_mime_type(mime_type.as_str()) {
                Ok(container) => self.apply_to_writer(&session, generation, out, |writer| {
                    writer.change_container(lane_of(source_buffer), container)
                }),
                Err(unsupported) => {
                    warn!(supported = false, "Unsupported SourceBuffer type change");
                    let response = response_to_unsupported_media_type(&unsupported);
                    self.respond_unless_stale(&session, generation, response, out);
                }
            },
            PageSessionMessage::TimestampOffsetChanged {
                generation,
                source_buffer,
                offset_us,
            } => self.apply_to_writer(&session, generation, out, |writer| {
                writer.set_timestamp_offset(
                    lane_of(source_buffer),
                    TimestampOffset::from_micros(offset_us.as_micros()),
                )
            }),
            PageSessionMessage::SourceBufferAborted {
                generation,
                source_buffer,
            } => self.apply_to_writer(&session, generation, out, |writer| {
                writer.abort(lane_of(source_buffer))
            }),
            PageSessionMessage::DurationChanged {
                generation,
                duration,
            } => self.apply_to_writer(&session, generation, out, |writer| {
                writer.set_duration(pushed_duration(duration));
                Ok(())
            }),
            PageSessionMessage::EndOfStream { generation } => {
                self.apply_to_writer(&session, generation, out, |writer| {
                    writer.end_of_stream().map(|_report| ())
                });
            }
            PageSessionMessage::SourceBufferRemoved { .. } => {
                // Решение владельца D6: `remove()` страницы не исполняем — что
                // выбрасывать с полки, решает окно буфера источника.
                self.counters.ignored_messages += 1;
            }
            PageSessionMessage::SeekStarted { target_us, cause } => {
                self.apply_seek_started(&session, target_us, cause, out);
            }
            PageSessionMessage::PagePlayback { request } => {
                out.push(IntakeNotice::PageIntent {
                    session,
                    intent: page_intent(request),
                });
            }
            PageSessionMessage::FileSourceAdded { generation } => {
                self.start_file_generation(&session, generation, out);
            }
            PageSessionMessage::RangeFetchFinished {
                request_id,
                outcome,
            } => self.finish_range_request(&session, request_id, outcome, out),
            PageSessionMessage::TabGone { reason } => {
                // Сеанс закрывает сервер (4002) СРАЗУ после этого сообщения: за ним в
                // очередь придёт `Ended`, и он запечатает поток. Здесь tab_gone только
                // отмечается; доигрывание полученного ведёт главный цикл.
                debug!(?reason, "tab_gone is handled by the session server");
                self.counters.tab_gone_messages += 1;
            }
            PageSessionMessage::VariantMenuChanged { .. } | PageSessionMessage::Attach { .. } => {
                // Меню вариантов — следующие сессии; `attach` до приёмников не
                // доходит (его съедает сервер).
                debug!("Page message is not consumed by the stream translator yet");
                self.counters.ignored_messages += 1;
            }
        }
    }

    /// Перемотка: страница сама (`page_seek`) или по нашей команде (`player_seek`).
    fn apply_seek_started(
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

    // ---- поколения --------------------------------------------------------

    /// Новое поколение: старый писатель бросается, новый поток — главному циклу.
    fn start_generation(
        &mut self,
        session: &SessionRef,
        generation: StreamGeneration,
        out: &mut Vec<IntakeNotice>,
    ) {
        let Some(InstanceState {
            phase: InstancePhase::Open(current),
            ..
        }) = self.instance.as_mut()
        else {
            return;
        };
        // Старый поток доигрывает принятое: `Drop` писателя без `end_of_stream`
        // заканчивает вход у демуксеров.
        *current = None;
        match self.factory.start_stream() {
            Ok(started) => {
                *current = Some(GenerationState {
                    number: generation,
                    stream: GenerationStream::Live(started.writer),
                });
                out.push(IntakeNotice::NewGeneration(Box::new(GenerationHandoff {
                    session: session.clone(),
                    generation,
                    source: GenerationSource::Segments {
                        opener: started.opener,
                        monitor: started.monitor,
                    },
                })));
            }
            Err(start_error) => {
                // Реестр демуксеров не собрался — дефект сборки, а не данных
                // страницы; странице честнее вернуть видео, чем молчать.
                error!(%start_error, "Pushed stream could not start; rejecting the generation");
                *current = Some(GenerationState {
                    number: generation,
                    stream: GenerationStream::Rejected,
                });
                out.push(IntakeNotice::Reject {
                    session: session.clone(),
                    generation,
                    reason: StreamRejection::UnsupportedCodec,
                });
            }
        }
    }

    /// Куда направить вход поколения `generation`.
    fn route(&mut self, generation: StreamGeneration) -> GenerationRoute<'_> {
        let Some(InstanceState {
            phase: InstancePhase::Open(current),
            ..
        }) = self.instance.as_mut()
        else {
            return GenerationRoute::Stale;
        };
        match current {
            None => GenerationRoute::Violation,
            Some(state) if generation < state.number => GenerationRoute::Stale,
            Some(state) if generation > state.number => GenerationRoute::Violation,
            Some(GenerationState {
                stream: GenerationStream::Rejected,
                ..
            }) => GenerationRoute::Rejected,
            Some(GenerationState {
                stream: GenerationStream::File(_),
                ..
            }) => GenerationRoute::NotSegments,
            Some(GenerationState {
                stream: GenerationStream::Live(writer),
                ..
            }) => GenerationRoute::Live(writer),
        }
    }

    /// Выполняет операцию над писателем живого поколения и разбирает ошибку —
    /// как вернувшуюся из вызова, так и записанную потоком (демуксер эпизода).
    ///
    /// [`WriterOutcome::Applied`] — только если и вызов, и поток после него целы.
    fn with_live_writer(
        &mut self,
        session: &SessionRef,
        generation: StreamGeneration,
        out: &mut Vec<IntakeNotice>,
        operation: impl FnOnce(&mut dyn StreamWriterPort) -> Result<(), PushedStreamError>,
    ) -> WriterOutcome {
        let failure = match self.route(generation) {
            GenerationRoute::Live(writer) => operation(writer.as_mut())
                .err()
                .or_else(|| writer.failure()),
            GenerationRoute::Rejected => {
                self.counters.rejected_generation_inputs += 1;
                return WriterOutcome::Skipped;
            }
            GenerationRoute::Stale => {
                self.counters.stale_inputs += 1;
                return WriterOutcome::Skipped;
            }
            GenerationRoute::NotSegments => {
                warn!(
                    generation = generation.get(),
                    "Media source input for a file generation"
                );
                self.request_close(session, SessionCloseReason::ProtocolViolation, out);
                return WriterOutcome::Skipped;
            }
            GenerationRoute::Violation => {
                warn!(
                    generation = generation.get(),
                    "Stream input for a generation that has not started"
                );
                self.request_close(session, SessionCloseReason::ProtocolViolation, out);
                return WriterOutcome::Skipped;
            }
        };
        let Some(failure) = failure else {
            return WriterOutcome::Applied;
        };
        // Только класс ошибки (инвариант 7): текст несёт подробности байтов страницы.
        warn!(
            failure_class = ?failure.failure_class(),
            generation = generation.get(),
            "Pushed stream failed"
        );
        let response = response_to_stream_failure(&failure);
        self.apply_response(session, generation, response, out);
        WriterOutcome::Failed
    }

    /// Сообщение MSE → вызов писателя. Итог не нужен: ответ на ошибку (отказ или
    /// просьба закрыть) `with_live_writer` уже выдал.
    fn apply_to_writer(
        &mut self,
        session: &SessionRef,
        generation: StreamGeneration,
        out: &mut Vec<IntakeNotice>,
        operation: impl FnOnce(&mut dyn StreamWriterPort) -> Result<(), PushedStreamError>,
    ) {
        let _outcome = self.with_live_writer(session, generation, out, operation);
    }

    /// Байты SourceBuffer → `append`.
    fn apply_bytes(&mut self, chunk: &OwnedChunk, out: &mut Vec<IntakeNotice>) {
        let Some(session) = self.active_session() else {
            return;
        };
        let mut append_outcome = None;
        let writer_outcome = self.with_live_writer(&session, chunk.generation, out, |writer| {
            append_outcome = Some(writer.append(lane_of(chunk.source_buffer), &chunk.payload)?);
            Ok(())
        });
        if append_outcome == Some(AppendOutcome::DiscardedConsumerClosed) {
            self.counters.discarded_after_consumer_closed += 1;
        }
        // Подсказка только о живом потоке: после отказа опрашивать нечего.
        let appended = writer_outcome == WriterOutcome::Applied
            && append_outcome == Some(AppendOutcome::Accepted);
        if appended {
            Self::hint_bytes_appended(session, out);
        }
    }

    /// Подсказка «опроси поток» — одна на серию подряд идущих кусков.
    fn hint_bytes_appended(session: SessionRef, out: &mut Vec<IntakeNotice>) {
        let last_is_same_hint = matches!(
            out.last(),
            Some(IntakeNotice::BytesAppended { session: hinted }) if *hinted == session
        );
        if !last_is_same_hint {
            out.push(IntakeNotice::BytesAppended { session });
        }
    }

    // ---- ответы на ошибки -------------------------------------------------

    /// Ответ на ошибку входа, если вход не устарел (у устаревшего поколения ответа нет).
    fn respond_unless_stale(
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
    fn apply_response(
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
    fn request_close(
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

/// Чем кончилась операция над писателем поколения.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum WriterOutcome {
    /// Вызов прошёл, поток жив.
    Applied,
    /// Вызов или поток после него сообщили ошибку; ответ уже выдан.
    Failed,
    /// Писателя не звали: вход устарел, поколение отвергнуто или нарушен порядок.
    Skipped,
}

/// Решение о входе экземпляра.
enum InstanceVerdict {
    /// Вход активного экземпляра.
    Current,
    /// Новый экземпляр: состояние с нуля.
    StartFresh,
    /// Активный экземпляр закрывается.
    Closing,
    /// Старый экземпляр.
    Stale,
}

/// SourceBuffer протокола → полоса источника (тот же номер).
const fn lane_of(source_buffer: SourceBufferIndex) -> LaneId {
    LaneId::new(source_buffer.get())
}

/// Время протокола (целые мкс) → время источника.
const fn media_time_as_duration(time: MediaTime) -> std::time::Duration {
    std::time::Duration::from_micros(time.as_micros())
}

/// Длительность протокола → длительность источника.
const fn pushed_duration(duration: MediaDuration) -> PushedDuration {
    match duration {
        MediaDuration::Unknown {} => PushedDuration::Unknown,
        MediaDuration::Live {} => PushedDuration::Live,
        MediaDuration::Finite { duration_us } => {
            PushedDuration::Finite(std::time::Duration::from_micros(duration_us.as_micros()))
        }
    }
}

/// `page_playback` → намерение для плеера.
const fn page_intent(request: PagePlaybackRequest) -> PageIntent {
    match request {
        PagePlaybackRequest::Play {} => PageIntent::Play,
        PagePlaybackRequest::Pause {} => PageIntent::Pause,
        PagePlaybackRequest::SetRate { rate } => PageIntent::SetRate(rate),
    }
}

#[cfg(test)]
mod tests;
