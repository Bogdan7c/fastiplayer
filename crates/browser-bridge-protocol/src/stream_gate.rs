//! Сторож потока на стороне приёмника: какие байты и сообщения принять, какие
//! молча выбросить как устаревшие, а какие — нарушение протокола (инвариант 6).
//!
//! Чистая логика без I/O. Плеер (сессия 03) пропускает через сторожа каждое
//! сообщение сеанса и каждую двоичную рамку **до** того, как отдать байты
//! демуксеру, а экстеншен в тестах проверяет на тех же правилах свою отправку.
//!
//! Правила:
//! - поколения идут подряд: первое — 1, каждое следующее — ровно на 1 больше,
//!   и о нём объявляют (`generation_started`) до первого его сообщения;
//! - всё, что про поколение старше текущего, — устаревшее: выбросить молча
//!   (реклама YouTube дописывает хвост, когда фильм уже начался);
//! - всё, что про ещё не объявленное поколение, — нарушение: так не бывает
//!   при правильном экстеншене;
//! - содержимое поколения выбирается один раз: либо MSE (`source_buffer_added`),
//!   либо файл `<video src>` (`file_source_added`), либо нативный HLS
//!   (`hls_source_added`, протокол 1.1), и смешивать их нельзя; в поколении-файле
//!   и HLS-поколении нет ничего MSE-шного (SourceBuffer, байты рамок вида 1,
//!   `duration_changed`, `end_of_stream`): длительность и конец плеер знает
//!   из самого файла или плейлиста;
//! - сообщение новее общей версии сеанса (`PageSessionMessage::required_version` —
//!   версия типа и его значений) — нарушение: при общей 1.0 страница не вправе слать
//!   `hls_source_added` и `resource_fetch_finished`, при общей ниже 1.3 —
//!   `tab_gone{page_player_failed}` (`crate::version`, правило 5);
//! - присутствие страницы (протокол 1.4, `page_presence`): после `page_left` страница
//!   молчит — любое сообщение, кроме `page_returned`, `page_reopen_failed` и `tab_gone`,
//!   и любые байты — нарушение; `page_returned` и `page_reopen_failed` при странице на
//!   месте — тоже нарушение; поколение, ушедшее со страницей, навсегда «устаревшее»;
//! - субтитры страницы (протокол 1.5, `page_subtitles`): рамка вида 3 продолжает открытое
//!   предложение своей дорожки (`subtitle_track_offered`) ровно с того байта, где оно
//!   остановилось, и не выходит за `byte_length`; рамка для непредложенной или уже готовой
//!   дорожки — нарушение; разных дорожек за сеанс не больше 64, повторное предложение —
//!   замена. Поколений и SourceBuffer-ов документы не трогают. При общей версии ниже 1.5
//!   и сообщение, и рамка вида 3 — нарушение;
//! - SourceBuffer объявляют (`source_buffer_added`) до его байтов и только раз;
//! - порядковые номера рамок SourceBuffer идут подряд с 0: пропуск значит
//!   потерю куска, и склеивать байты дальше нельзя.

mod page_presence;
mod page_subtitles;

use crate::data_frame::{DataFrameHeader, PageSubtitleBytesHeader, SourceBufferBytesHeader};
use crate::limits::MAX_SOURCE_BUFFERS_PER_GENERATION;
use crate::session::page::{PageSessionMessage, StreamScope};
use crate::stream::{AppendSequence, SourceBufferIndex, StreamGeneration};
use crate::version::{CURRENT_PROTOCOL_VERSION, ProtocolVersion};

use self::page_presence::{PagePresence, PresenceReaction};
use self::page_subtitles::PageSubtitleOffers;

/// Что делать с сообщением или рамкой.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum GateDecision {
    /// Принять: про текущее поколение и по правилам.
    Accept,
    /// Молча выбросить: про старое поколение.
    DropStale,
}

/// Нарушение протокола потока: сеанс закрывается с
/// `SessionCloseReason::ProtocolViolation`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, thiserror::Error)]
pub enum StreamViolation {
    /// Новое поколение не на 1 больше текущего (или первое не 1).
    #[error("stream generation is out of order")]
    GenerationOutOfOrder,
    /// Сообщение или рамка про поколение, о котором не объявляли.
    #[error("stream generation was not announced")]
    GenerationNotAnnounced,
    /// Байты или сообщение про SourceBuffer, о котором не объявляли.
    #[error("source buffer was not announced")]
    SourceBufferNotAnnounced,
    /// SourceBuffer объявлен второй раз в том же поколении.
    #[error("source buffer was already announced")]
    SourceBufferAlreadyAnnounced,
    /// `file_source_added` повторён в том же поколении.
    #[error("file source was already announced")]
    FileSourceAlreadyAnnounced,
    /// `file_source_added` пришёл в поколении, где уже есть SourceBuffer.
    #[error("file source announced in a generation that already has source buffers")]
    FileSourceAfterSourceBuffer,
    /// `source_buffer_added` пришёл в поколении, объявленном файлом.
    #[error("source buffer announced in a file generation")]
    SourceBufferInFileGeneration,
    /// Сообщение или рамка, имеющие смысл только для MSE (SourceBuffer,
    /// байты рамок вида 1, длительность, конец потока), пришли в поколении-файле.
    #[error("media source message in a file generation")]
    MediaSourceMessageInFileGeneration,
    /// `hls_source_added` повторён в том же поколении.
    #[error("hls source was already announced")]
    HlsSourceAlreadyAnnounced,
    /// `hls_source_added` пришёл в поколении, где уже есть SourceBuffer.
    #[error("hls source announced in a generation that already has source buffers")]
    HlsSourceAfterSourceBuffer,
    /// `hls_source_added` пришёл в поколении, объявленном файлом.
    #[error("hls source announced in a file generation")]
    HlsSourceInFileGeneration,
    /// `file_source_added` пришёл в поколении, объявленном HLS.
    #[error("file source announced in an hls generation")]
    FileSourceInHlsGeneration,
    /// `source_buffer_added` пришёл в поколении, объявленном HLS.
    #[error("source buffer announced in an hls generation")]
    SourceBufferInHlsGeneration,
    /// MSE-сообщение или рамка вида 1 пришли в HLS-поколении.
    #[error("media source message in an hls generation")]
    MediaSourceMessageInHlsGeneration,
    /// `page_returned` или `page_reopen_failed` пришли, пока страница на месте (протокол 1.4).
    #[error("page_returned or page_reopen_failed while the page is present")]
    PageIsPresent,
    /// Сообщение или байты пришли, пока страница ушла или не вернулась (протокол 1.4):
    /// после `page_left` разрешены только `page_returned`, `page_reopen_failed` и `tab_gone`.
    #[error("message or bytes while the page is away")]
    PageIsAway,
    /// Сообщение появилось в версии протокола новее общей версии сеанса.
    #[error("message is not part of the agreed protocol version")]
    MessageNotInAgreedVersion {
        /// Общая версия сеанса.
        agreed: ProtocolVersion,
        /// С какой версии сообщение можно слать: версия его типа или, если новее, его
        /// значения (`PageSessionMessage::required_version`).
        introduced_in: ProtocolVersion,
    },
    /// Рамка новее общей версии сеанса (1.5: вид 3 при общей 1.4 и ниже).
    #[error("data frame kind is not part of the agreed protocol version")]
    DataFrameNotInAgreedVersion {
        /// Общая версия сеанса.
        agreed: ProtocolVersion,
        /// С какой версии есть такой вид рамки (`DataFrameHeader::introduced_in`).
        introduced_in: ProtocolVersion,
    },
    /// Новая дорожка субтитров сверх предела сеанса (`MAX_PAGE_SUBTITLE_TRACKS`, протокол 1.5).
    #[error("too many page subtitle tracks in one session")]
    TooManyPageSubtitleTracks,
    /// Байты субтитров для дорожки, которую не предлагали (`subtitle_track_offered`).
    #[error("page subtitle bytes for a track that was not offered")]
    PageSubtitleTrackNotOffered,
    /// Байты субтитров для дорожки, чей документ уже пришёл целиком.
    #[error("page subtitle bytes for a document that is already complete")]
    PageSubtitleDocumentComplete,
    /// Смещение рамки субтитров не продолжает документ: кусок потерян или повторён.
    #[error("page subtitle data frame offset has a gap")]
    PageSubtitleOffsetGap {
        /// Какое смещение ожидалось (сколько байт предложения уже пришло).
        expected: u64,
        /// Какое пришло.
        received: u64,
    },
    /// Рамка субтитров выходит за объявленную длину документа (`byte_length`).
    #[error("page subtitle bytes exceed the declared document length")]
    PageSubtitleBeyondByteLength,
    /// Порядковый номер рамки не тот, что ожидался: кусок потерян или повторён.
    #[error("data frame sequence has a gap")]
    SequenceGap {
        /// Какой номер ожидался.
        expected: AppendSequence,
        /// Какой пришёл.
        received: AppendSequence,
    },
}

/// Сколько рамок SourceBuffer уже принято.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct SourceBufferProgress {
    next_sequence: AppendSequence,
}

/// Чем оказалось содержимое поколения.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum GenerationContent {
    /// Ещё не решено: после `generation_started` ничего не объявляли.
    Undecided,
    /// MSE: объявлен хотя бы один SourceBuffer.
    MediaSource,
    /// Файл `<video src>`: объявлен `file_source_added`.
    File,
    /// Нативный HLS: объявлен `hls_source_added` (протокол 1.1).
    Hls,
}

/// Чем поколение объявили целиком одним сообщением (не MSE).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum WholeSource {
    /// `file_source_added`.
    File,
    /// `hls_source_added`.
    Hls,
}

impl WholeSource {
    /// Каким становится содержимое поколения после объявления.
    const fn content(self) -> GenerationContent {
        match self {
            Self::File => GenerationContent::File,
            Self::Hls => GenerationContent::Hls,
        }
    }

    /// Нарушение, если содержимое поколения уже решено: повтор, MSE или другой вид.
    const fn conflict_with(self, decided: GenerationContent) -> Option<StreamViolation> {
        match (self, decided) {
            (_, GenerationContent::Undecided) => None,
            (Self::File, GenerationContent::File) => {
                Some(StreamViolation::FileSourceAlreadyAnnounced)
            }
            (Self::File, GenerationContent::MediaSource) => {
                Some(StreamViolation::FileSourceAfterSourceBuffer)
            }
            (Self::File, GenerationContent::Hls) => {
                Some(StreamViolation::FileSourceInHlsGeneration)
            }
            (Self::Hls, GenerationContent::Hls) => Some(StreamViolation::HlsSourceAlreadyAnnounced),
            (Self::Hls, GenerationContent::MediaSource) => {
                Some(StreamViolation::HlsSourceAfterSourceBuffer)
            }
            (Self::Hls, GenerationContent::File) => {
                Some(StreamViolation::HlsSourceInFileGeneration)
            }
        }
    }
}

impl GenerationContent {
    /// Нарушение для MSE-сообщения или рамки вида 1 в этом поколении; `None` — можно.
    const fn media_source_violation(self) -> Option<StreamViolation> {
        match self {
            Self::Undecided | Self::MediaSource => None,
            Self::File => Some(StreamViolation::MediaSourceMessageInFileGeneration),
            Self::Hls => Some(StreamViolation::MediaSourceMessageInHlsGeneration),
        }
    }

    /// Нарушение для `source_buffer_added` в этом поколении; `None` — можно.
    const fn source_buffer_violation(self) -> Option<StreamViolation> {
        match self {
            Self::Undecided | Self::MediaSource => None,
            Self::File => Some(StreamViolation::SourceBufferInFileGeneration),
            Self::Hls => Some(StreamViolation::SourceBufferInHlsGeneration),
        }
    }
}

/// Состояние текущего поколения.
#[derive(Debug, Clone, PartialEq, Eq)]
struct GenerationState {
    generation: StreamGeneration,
    content: GenerationContent,
    /// Объявленные SourceBuffer по индексу; `None` — не объявлен.
    source_buffers: [Option<SourceBufferProgress>; MAX_SOURCE_BUFFERS_PER_GENERATION as usize],
}

impl GenerationState {
    fn new(generation: StreamGeneration) -> Self {
        Self {
            generation,
            content: GenerationContent::Undecided,
            source_buffers: [None; MAX_SOURCE_BUFFERS_PER_GENERATION as usize],
        }
    }

    fn progress_mut(
        &mut self,
        index: SourceBufferIndex,
    ) -> Option<&mut Option<SourceBufferProgress>> {
        self.source_buffers.get_mut(index.as_usize())
    }

    fn is_announced(&self, index: SourceBufferIndex) -> bool {
        self.source_buffers
            .get(index.as_usize())
            .is_some_and(Option::is_some)
    }
}

/// Как поколение сообщения соотносится с текущим.
enum GenerationRelation {
    Stale,
    Current,
    NotAnnounced,
}

/// Сторож потока одного сеанса.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct StreamGate {
    /// Общая версия сеанса (`hello`): сообщения новее неё — нарушение.
    agreed_version: ProtocolVersion,
    /// Текущее поколение; `None` — ещё ни одного не объявили.
    current: Option<GenerationState>,
    /// Где страница-источник (протокол 1.4); начинается «на месте».
    presence: PagePresence,
    /// Последнее поколение, ушедшее со страницей: всё про него и старше — устаревшее,
    /// даже когда страница вернулась (инвариант 6). Следующее поколение после возврата —
    /// ровно на 1 больше текущего, как и раньше.
    left_with_page: Option<StreamGeneration>,
    /// Предложения дорожек субтитров страницы (протокол 1.5): живут весь сеанс, поколений
    /// не касаются.
    page_subtitles: PageSubtitleOffers,
}

impl Default for StreamGate {
    fn default() -> Self {
        Self::new()
    }
}

impl StreamGate {
    /// Сторож нового сеанса на версии этого crate ([`CURRENT_PROTOCOL_VERSION`]):
    /// поколений ещё нет.
    ///
    /// Для проигрывания записанных потоков и тестов. Настоящий сеанс знает свою
    /// общую версию из `hello` и создаёт сторожа через [`Self::for_agreed_version`].
    #[must_use]
    pub const fn new() -> Self {
        Self::for_agreed_version(CURRENT_PROTOCOL_VERSION)
    }

    /// Сторож нового сеанса, договорившегося о версии `agreed` (итог
    /// `negotiate_protocol_version`): поколений ещё нет.
    #[must_use]
    pub const fn for_agreed_version(agreed: ProtocolVersion) -> Self {
        Self {
            agreed_version: agreed,
            current: None,
            presence: PagePresence::Present,
            left_with_page: None,
            page_subtitles: PageSubtitleOffers::new(),
        }
    }

    /// Общая версия сеанса, по которой сторож пропускает сообщения.
    #[must_use]
    pub const fn agreed_version(&self) -> ProtocolVersion {
        self.agreed_version
    }

    /// Текущее поколение, если уже объявлено.
    #[must_use]
    pub fn current_generation(&self) -> Option<StreamGeneration> {
        self.current.as_ref().map(|state| state.generation)
    }

    /// Пропускает текстовое сообщение сеанса через правила потока.
    ///
    /// Сообщения не про поток (`attach`, перемотка, меню) принимаются всегда,
    /// если они есть в общей версии сеанса.
    ///
    /// # Errors
    /// [`StreamViolation`] — сеанс надо закрыть. Отвергнутое сообщение состояние
    /// сторожа не меняет.
    pub fn apply_message(
        &mut self,
        message: &PageSessionMessage,
    ) -> Result<GateDecision, StreamViolation> {
        // Версия — раньше всех правил потока: сообщения новее договора нет вовсе,
        // даже про устаревшее поколение (иначе нарушение пряталось бы за «устарело»).
        // Сверяется версия сообщения со всеми значениями, а не только типа (1.3).
        let introduced_in = message.required_version();
        if !self.agreed_version.includes(introduced_in) {
            return Err(StreamViolation::MessageNotInAgreedVersion {
                agreed: self.agreed_version,
                introduced_in,
            });
        }
        // Присутствие страницы — раньше правил потока: пока страница ушла, потоковых
        // сообщений нет вовсе, даже про устаревшее поколение.
        match self.presence.react_to(message)? {
            PresenceReaction::AcceptAndBecome(next) => {
                if self.presence == PagePresence::Present && next == PagePresence::Left {
                    self.leave_page();
                }
                self.presence = next;
                return Ok(GateDecision::Accept);
            }
            PresenceReaction::ContinueWithStreamRules => {}
        }
        match message {
            PageSessionMessage::GenerationStarted { generation } => {
                self.start_generation(*generation)
            }
            PageSessionMessage::SourceBufferAdded {
                generation,
                source_buffer,
                ..
            } => self.announce_source_buffer(*generation, *source_buffer),
            PageSessionMessage::FileSourceAdded { generation } => {
                self.announce_whole_source(*generation, WholeSource::File)
            }
            PageSessionMessage::HlsSourceAdded { generation, .. } => {
                self.announce_whole_source(*generation, WholeSource::Hls)
            }
            PageSessionMessage::SubtitleTrackOffered {
                track, byte_length, ..
            } => self.page_subtitles.offer(*track, *byte_length),
            other => self.check_scope(other.stream_scope()),
        }
    }

    /// Пропускает двоичную рамку байтов SourceBuffer.
    ///
    /// Принятая рамка сдвигает ожидаемый порядковый номер; выброшенная как
    /// устаревшая ничего не меняет.
    ///
    /// # Errors
    /// [`StreamViolation`] — сеанс надо закрыть.
    pub fn admit_source_buffer_bytes(
        &mut self,
        header: &SourceBufferBytesHeader,
    ) -> Result<GateDecision, StreamViolation> {
        // Страница ушла — байтов быть не может, даже «устаревших».
        self.presence.admit_bytes()?;
        match self.relation_to(header.generation) {
            GenerationRelation::Stale => return Ok(GateDecision::DropStale),
            GenerationRelation::NotAnnounced => {
                return Err(StreamViolation::GenerationNotAnnounced);
            }
            GenerationRelation::Current => {}
        }
        if let Some(violation) = self.current_content().media_source_violation() {
            return Err(violation);
        }
        let slot = self
            .current
            .as_mut()
            .and_then(|state| state.progress_mut(header.source_buffer))
            .ok_or(StreamViolation::SourceBufferNotAnnounced)?;
        let progress = slot
            .as_mut()
            .ok_or(StreamViolation::SourceBufferNotAnnounced)?;
        if header.sequence != progress.next_sequence {
            return Err(StreamViolation::SequenceGap {
                expected: progress.next_sequence,
                received: header.sequence,
            });
        }
        // u64 рамок не переполнить за жизнь сеанса; на всякий случай — без паники:
        // номер «застынет», и следующая рамка честно даст SequenceGap.
        progress.next_sequence = AppendSequence(progress.next_sequence.0.saturating_add(1));
        Ok(GateDecision::Accept)
    }

    /// Пропускает байты `<video src>` (рамки вида 2), которых сторож иначе не разбирает.
    ///
    /// Сервер ОБЯЗАН вызывать это для каждой такой рамки: пока страница на месте — принять,
    /// иначе (ушла, не вернулась) — нарушение. Состояние сторожа не меняется.
    ///
    /// # Errors
    /// [`StreamViolation::PageIsAway`] — страница ушла или не вернулась.
    pub fn admit_range_bytes(&self) -> Result<GateDecision, StreamViolation> {
        self.presence.admit_bytes().map(|()| GateDecision::Accept)
    }

    /// Пропускает рамку вида 3 — байты документа субтитров страницы (протокол 1.5).
    ///
    /// Сервер ОБЯЗАН вызывать это для каждой такой рамки. Принятая рамка сдвигает ожидаемое
    /// смещение документа своей дорожки; поколений и SourceBuffer-ов она не трогает.
    /// «Устаревших» байтов субтитров не бывает: замену документа страница объявляет новым
    /// предложением, и после него рамки снова идут с 0.
    ///
    /// # Errors
    /// [`StreamViolation::DataFrameNotInAgreedVersion`] — общая версия ниже 1.5;
    /// [`StreamViolation::PageIsAway`] — страница ушла; нарушения порядка документа
    /// (`PageSubtitle*`). Отвергнутая рамка состояние сторожа не меняет.
    pub fn admit_page_subtitle_bytes(
        &mut self,
        header: &PageSubtitleBytesHeader,
        payload: &[u8],
    ) -> Result<GateDecision, StreamViolation> {
        let introduced_in = DataFrameHeader::PageSubtitleBytes(*header).introduced_in();
        if !self.agreed_version.includes(introduced_in) {
            return Err(StreamViolation::DataFrameNotInAgreedVersion {
                agreed: self.agreed_version,
                introduced_in,
            });
        }
        // Страница ушла — байтов быть не может никаких, и субтитров тоже (протокол 1.4).
        self.presence.admit_bytes()?;
        self.page_subtitles.admit_bytes(header, payload.len())
    }

    /// Страница ушла: текущее поколение уходит с ней и навсегда становится устаревшим.
    fn leave_page(&mut self) {
        if let Some(state) = &self.current {
            self.left_with_page = Some(state.generation);
        }
    }

    /// Новое поколение: ровно следующее за текущим (или 1 для первого).
    fn start_generation(
        &mut self,
        generation: StreamGeneration,
    ) -> Result<GateDecision, StreamViolation> {
        let expected = match &self.current {
            None => Some(StreamGeneration::FIRST),
            Some(state) => state.generation.next(),
        };
        if Some(generation) != expected {
            // Повтор или «назад» тоже нарушение: поколения объявляет один экстеншен
            // строго по порядку, и старое объявление прийти позже не может.
            return Err(StreamViolation::GenerationOutOfOrder);
        }
        self.current = Some(GenerationState::new(generation));
        Ok(GateDecision::Accept)
    }

    /// Объявление SourceBuffer в текущем поколении.
    fn announce_source_buffer(
        &mut self,
        generation: StreamGeneration,
        index: SourceBufferIndex,
    ) -> Result<GateDecision, StreamViolation> {
        match self.relation_to(generation) {
            GenerationRelation::Stale => return Ok(GateDecision::DropStale),
            GenerationRelation::NotAnnounced => {
                return Err(StreamViolation::GenerationNotAnnounced);
            }
            GenerationRelation::Current => {}
        }
        if let Some(violation) = self.current_content().source_buffer_violation() {
            return Err(violation);
        }
        let slot = self
            .current
            .as_mut()
            .and_then(|state| state.progress_mut(index))
            .ok_or(StreamViolation::GenerationNotAnnounced)?;
        if slot.is_some() {
            return Err(StreamViolation::SourceBufferAlreadyAnnounced);
        }
        *slot = Some(SourceBufferProgress {
            next_sequence: AppendSequence(0),
        });
        if let Some(state) = self.current.as_mut() {
            state.content = GenerationContent::MediaSource;
        }
        Ok(GateDecision::Accept)
    }

    /// Объявление «в этом поколении файл» (`file_source_added`) или «HLS»
    /// (`hls_source_added`): поколение целиком, без SourceBuffer.
    ///
    /// Состояние меняется только при успехе: отвергнутое сообщение сеанс
    /// всё равно закрывает, но сторож не должен получать полупринятый вид.
    fn announce_whole_source(
        &mut self,
        generation: StreamGeneration,
        source: WholeSource,
    ) -> Result<GateDecision, StreamViolation> {
        match self.relation_to(generation) {
            GenerationRelation::Stale => return Ok(GateDecision::DropStale),
            GenerationRelation::NotAnnounced => {
                return Err(StreamViolation::GenerationNotAnnounced);
            }
            GenerationRelation::Current => {}
        }
        let state = self
            .current
            .as_mut()
            .ok_or(StreamViolation::GenerationNotAnnounced)?;
        if let Some(violation) = source.conflict_with(state.content) {
            return Err(violation);
        }
        state.content = source.content();
        Ok(GateDecision::Accept)
    }

    /// Проверка остальных сообщений по их области.
    fn check_scope(&self, scope: StreamScope) -> Result<GateDecision, StreamViolation> {
        let (generation, source_buffer) = match scope {
            StreamScope::Unbound => return Ok(GateDecision::Accept),
            StreamScope::Generation(generation) => (generation, None),
            StreamScope::SourceBuffer(generation, index) => (generation, Some(index)),
        };
        match self.relation_to(generation) {
            GenerationRelation::Stale => Ok(GateDecision::DropStale),
            GenerationRelation::NotAnnounced => Err(StreamViolation::GenerationNotAnnounced),
            // Все сообщения областей «поколение» и «SourceBuffer», доходящие сюда, —
            // про MSE (длительность, конец потока, правки SourceBuffer); в
            // поколении-файле и HLS-поколении их быть не может.
            GenerationRelation::Current => {
                if let Some(violation) = self.current_content().media_source_violation() {
                    return Err(violation);
                }
                self.check_source_buffer_announced(source_buffer)
            }
        }
    }

    /// SourceBuffer сообщения (если оно про SourceBuffer) объявлен в текущем поколении.
    fn check_source_buffer_announced(
        &self,
        source_buffer: Option<SourceBufferIndex>,
    ) -> Result<GateDecision, StreamViolation> {
        match source_buffer {
            Some(index) if !self.is_announced(index) => {
                Err(StreamViolation::SourceBufferNotAnnounced)
            }
            _ => Ok(GateDecision::Accept),
        }
    }

    /// Содержимое текущего поколения; без поколения — «ещё не решено».
    fn current_content(&self) -> GenerationContent {
        self.current
            .as_ref()
            .map_or(GenerationContent::Undecided, |state| state.content)
    }

    fn is_announced(&self, index: SourceBufferIndex) -> bool {
        self.current
            .as_ref()
            .is_some_and(|state| state.is_announced(index))
    }

    fn relation_to(&self, generation: StreamGeneration) -> GenerationRelation {
        // Поколение, ушедшее со страницей, устарело и после её возврата (оно ещё `current`).
        if self.left_with_page.is_some_and(|left| generation <= left) {
            return GenerationRelation::Stale;
        }
        match &self.current {
            None => GenerationRelation::NotAnnounced,
            Some(state) if generation < state.generation => GenerationRelation::Stale,
            Some(state) if generation == state.generation => GenerationRelation::Current,
            Some(_) => GenerationRelation::NotAnnounced,
        }
    }
}

#[cfg(test)]
mod tests;
