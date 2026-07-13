//! Queue-neutral contract установки media.
//!
//! Порядок strong protocol фиксирован типами: transport acceptance → ordinary fallible
//! preparation → `ReadyToCommit` → matching `AuthorizeInstallCommit` → один infallible owner
//! turn, который заменяет active ownership и публикует `Installed` в request-owned slot.
//! Existing single-media adapter пока destructive: reset seek floor/decoder, open transition,
//! audio plan, video configuration и post-ownership `MediaOpened` перечислены как отдельные
//! characterization stages. Session 00C1 strong path переносит ordinary failures до ready;
//! protocol остаётся queue-neutral и не владеет playlist lineage.

use std::fmt;
use std::num::NonZeroU64;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex};

use crate::{PlayerError, PlayerErrorKind};

/// Общий process-local allocator request identities.
static NEXT_MEDIA_INSTALL_REQUEST_ID: AtomicU64 = AtomicU64::new(1);

/// Общий process-local allocator installed-media identities.
///
/// Счётчик общий для всех `PlayerWorker`, поэтому controlled rebind не переиспользует
/// identity старого worker-а и late event можно отвергнуть точным сравнением.
static NEXT_MEDIA_INSTANCE_ID: AtomicU64 = AtomicU64::new(1);

/// Выдаёт следующий ненулевой ID и завершает процесс только при физически недостижимом exhaustion.
fn allocate_process_identity(counter: &AtomicU64, identity_name: &str) -> NonZeroU64 {
    let raw_identity = counter
        .fetch_update(Ordering::Relaxed, Ordering::Relaxed, |current| {
            current.checked_add(1)
        })
        .unwrap_or_else(|_| panic!("{identity_name} identity space exhausted"));

    NonZeroU64::new(raw_identity)
        .unwrap_or_else(|| panic!("{identity_name} allocator produced zero identity"))
}

/// Нейтральная identity одного запроса установки media.
///
/// Тип намеренно не содержит playlist item, source label или app-owned lineage.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub struct MediaInstallRequestId(NonZeroU64);

impl MediaInstallRequestId {
    /// Создаёт новый process-local request ID для production dispatch.
    #[must_use]
    pub fn new_unique() -> Self {
        Self(allocate_process_identity(
            &NEXT_MEDIA_INSTALL_REQUEST_ID,
            "media install request",
        ))
    }

    /// Строит identity из явного ненулевого значения для deterministic tests и replay fixtures.
    #[must_use]
    pub const fn from_non_zero(raw_identity: NonZeroU64) -> Self {
        Self(raw_identity)
    }

    /// Возвращает transport-neutral числовое представление без source semantics.
    #[must_use]
    pub const fn get(self) -> u64 {
        self.0.get()
    }
}

impl fmt::Display for MediaInstallRequestId {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        self.0.fmt(formatter)
    }
}

/// Точная identity одного успешно установленного media instance.
///
/// Повторное открытие того же locator получает новый ID; source label identity не является.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub struct MediaInstanceId(NonZeroU64);

impl MediaInstanceId {
    /// Выдаёт новый process-local instance ID в точке фактического install.
    #[must_use]
    pub(crate) fn new_unique() -> Self {
        Self(allocate_process_identity(
            &NEXT_MEDIA_INSTANCE_ID,
            "media instance",
        ))
    }

    /// Строит identity из явного ненулевого значения для deterministic tests и fixtures.
    #[must_use]
    pub const fn from_non_zero(raw_identity: NonZeroU64) -> Self {
        Self(raw_identity)
    }

    /// Возвращает transport-neutral числовое представление.
    #[must_use]
    pub const fn get(self) -> u64 {
        self.0.get()
    }
}

impl fmt::Display for MediaInstanceId {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        self.0.fmt(formatter)
    }
}

/// Причина terminal cancellation до принятой authorization.
///
/// Варианты не сворачиваются в общий `Cancelled`, потому что controller по-разному
/// продолжает transport, structural mutation и lifecycle.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum MediaInstallCancellationCause {
    /// Пользователь отменил текущую подготовку явно.
    UserCancelled,

    /// Новый install intent заменил прежний запрос.
    Superseded,

    /// Политика «остановиться после текущего» выиграла до commit barrier.
    StopAfterCurrent,

    /// Явный transport Stop выиграл до commit barrier.
    TransportStop,

    /// Structural revision сделала candidate недействительным.
    StructuralInvalidation,

    /// Renderer/player binding приостановлен до commit barrier.
    LifecycleSuspended,

    /// Process lifecycle завершает worker до commit barrier.
    LifecycleShutdown,
}

/// Fallible stage существующего single-media call graph.
///
/// Этот список является characterization, а не заявлением о strong transaction:
/// Session 00C/00C1 должна вынести все обычные failures до `ReadyToCommit`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum MediaInstallFailureStage {
    /// Existing reset не смог очистить Accurate-seek decoder floor и продолжил работу.
    LegacyResetSeekFloor,

    /// Existing reset не смог flush-ить singleton decoder и продолжил работу.
    LegacyResetDecoderFlush,

    /// Existing reset получил typed reject/backpressure/fatal при clear stream и продолжил работу.
    LegacyResetDecoderStream,

    /// Session не приняла `OpenMedia` transition после destructive reset.
    OpenTransition,

    /// Lazy audio plan не построен; legacy path сохраняет recoverable audio-less behavior.
    AudioTrackPlanning,

    /// Выбор/configure video stream завершился typed ошибкой.
    VideoStreamConfiguration,

    /// App resource owner не выдал matching detached backend/materializer pair.
    CandidateVideoResourceAcquisition,

    /// Полученный backend ID не совпал с pure capability plan candidate-а.
    CandidateVideoBackendMatching,

    /// Detached decoder отверг candidate stream config до commit barrier-а.
    CandidateVideoBackendConfiguration,

    /// Configured/cancelled status не доставлен app resource owner-у.
    CandidateVideoStatusPublication,

    /// Ownership уже перемещён, но `MediaOpened` transition не завершился.
    LegacyMediaOpenedTransition,
}

impl MediaInstallFailureStage {
    /// Полный ordered inventory legacy и strong candidate stages после Session 00C1.
    pub const ALL: [Self; 11] = [
        Self::LegacyResetSeekFloor,
        Self::LegacyResetDecoderFlush,
        Self::LegacyResetDecoderStream,
        Self::OpenTransition,
        Self::AudioTrackPlanning,
        Self::VideoStreamConfiguration,
        Self::CandidateVideoResourceAcquisition,
        Self::CandidateVideoBackendMatching,
        Self::CandidateVideoBackendConfiguration,
        Self::CandidateVideoStatusPublication,
        Self::LegacyMediaOpenedTransition,
    ];
}

/// Player-side port к заранее staged app half video candidate-а.
///
/// `Send` требуется только потому, что owner перемещается в player worker thread;
/// concrete renderer/materializer pointers через этот trait в player не проходят.
pub type MediaInstallVideoResourcePort = Box<
    dyn video_backend_api::DetachedVideoBackendResourcePort<RequestId = MediaInstallRequestId>
        + Send,
>;

/// Единственная будущая atomic commit point strong media transaction.
///
/// В этой точке ordinary failure запрещён: matching authorization меняет active resource
/// ownership и записывает `Installed` до обработки следующего cancel/lifecycle control.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum MediaInstallCommitPoint {
    /// Неделимый owner turn: active swap + exact instance + lossless terminal publication.
    ReplaceActiveOwnershipAndPublishInstalled,
}

/// Typed failure одного принятого install request-а.
#[derive(Debug, Clone, PartialEq)]
pub struct MediaInstallFailure {
    /// Точный этап, на котором запрос завершился.
    pub stage: MediaInstallFailureStage,

    /// Исходная player ошибка без потери kind/message distinctions.
    pub error: PlayerError,
}

impl MediaInstallFailure {
    /// Создаёт correlated failure для конкретного fallible stage.
    #[must_use]
    pub const fn new(stage: MediaInstallFailureStage, error: PlayerError) -> Self {
        Self { stage, error }
    }

    /// Создаёт typed failure для legacy command rejection, который ранее был только debug event.
    #[must_use]
    pub(crate) fn legacy_open_rejected(message: impl Into<String>) -> Self {
        Self::new(
            MediaInstallFailureStage::OpenTransition,
            PlayerError::new(PlayerErrorKind::InvalidCommand, message),
        )
    }
}

/// Non-terminal phase, опубликованная после всех ordinary fallible candidate stages.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum MediaInstallPhase {
    /// Candidate готов, но active ownership ещё не менялся и требуется matching authorization.
    ReadyToCommit {
        /// Request identity, которой принадлежит candidate и будущий terminal slot.
        request_id: MediaInstallRequestId,
    },
}

/// Terminal outcome принятого install request-а.
#[derive(Debug, Clone, PartialEq)]
pub enum MediaInstallCompletion {
    /// Matching authorization принята и новый instance гарантированно установлен.
    Installed {
        /// Request identity, для которой была принята authorization.
        request_id: MediaInstallRequestId,

        /// Точная identity установленного player instance.
        media_instance_id: MediaInstanceId,
    },

    /// Ordinary fallible stage завершил запрос до accepted authorization.
    Failed {
        /// Request identity завершившейся операции.
        request_id: MediaInstallRequestId,

        /// Typed stage и исходная player error.
        failure: MediaInstallFailure,
    },

    /// Ordered cancel/lifecycle control линейно выиграл до authorization.
    Cancelled {
        /// Request identity отменённой операции.
        request_id: MediaInstallRequestId,

        /// Точная причина terminal cancellation.
        cause: MediaInstallCancellationCause,
    },
}

/// Fatal нарушение terminal guarantee после `AuthorizationAccepted`.
#[derive(Debug, Clone, PartialEq)]
pub enum AcceptedMediaInstallTerminalError {
    /// Owner outcome принят, но request-owned terminal slot остался пустым.
    MissingInstalled {
        /// Exact request, для которого terminal был обязательным.
        request_id: MediaInstallRequestId,
    },

    /// Terminal `Installed` принадлежит другому request-у.
    InstalledRequestMismatch {
        /// Request receipt-а.
        expected_request_id: MediaInstallRequestId,

        /// Request фактически опубликованного terminal-а.
        installed_request_id: MediaInstallRequestId,
    },

    /// После accepted authorization опубликован pre-barrier terminal variant.
    UnexpectedCompletion(MediaInstallCompletion),
}

impl fmt::Display for AcceptedMediaInstallTerminalError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::MissingInstalled { request_id } => write!(
                formatter,
                "accepted media install request {} не содержит обязательный Installed terminal",
                request_id.get()
            ),
            Self::InstalledRequestMismatch {
                expected_request_id,
                installed_request_id,
            } => write!(
                formatter,
                "Installed terminal request {} не совпал с expected request {}",
                installed_request_id.get(),
                expected_request_id.get()
            ),
            Self::UnexpectedCompletion(completion) => write!(
                formatter,
                "accepted authorization завершилась unexpected terminal: {completion:?}"
            ),
        }
    }
}

impl std::error::Error for AcceptedMediaInstallTerminalError {}

impl MediaInstallCompletion {
    /// Возвращает request identity независимо от terminal variant-а.
    #[must_use]
    pub const fn request_id(&self) -> MediaInstallRequestId {
        match self {
            Self::Installed { request_id, .. }
            | Self::Failed { request_id, .. }
            | Self::Cancelled { request_id, .. } => *request_id,
        }
    }
}

/// Explicit authorization intent для matching ready request-а.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct AuthorizeInstallCommit {
    /// Единственный request, которому разрешён ownership commit.
    pub request_id: MediaInstallRequestId,
}

/// Explicit cancel intent, сериализуемый тем же owner stream, что и authorization/lifecycle.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct CancelMediaInstall {
    /// Request, который разрешено отменить только до accepted authorization.
    pub request_id: MediaInstallRequestId,

    /// Причина, которую terminal completion сохраняет без нормализации.
    pub cause: MediaInstallCancellationCause,
}

/// Один ordered control stream install owner-а.
///
/// Lifecycle представлен точными cancellation causes и не обходит authorization отдельным flag-ом.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum MediaInstallControl {
    /// Matching commit authorization.
    Authorize(AuthorizeInstallCommit),

    /// User/transport/structural/lifecycle cancellation до barrier.
    Cancel(CancelMediaInstall),
}

/// Outcome применения control к request-owned state machine.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum MediaInstallControlOutcome {
    /// Authorization принята; terminal `Installed` уже записан до возврата outcome.
    AuthorizationAccepted,

    /// Cancellation выиграла и terminal record уже записан.
    CancellationAccepted,

    /// Control адресован не тому request-у.
    StaleRequest,

    /// Authorization пришла до `ReadyToCommit`.
    NotReady,

    /// Request уже имеет terminal record; duplicate control ничего не меняет.
    AlreadyTerminal,
}

/// Fake-able publication port между owner state machine и request-owned storage.
///
/// Методы не возвращают channel/backpressure error: production реализация использует
/// preallocated mutex slots, поэтому terminal publication после authorization infallible.
pub trait MediaInstallPhaseCompletionPort: Send + Sync + fmt::Debug {
    /// Публикует единственную non-terminal ready phase.
    fn publish_ready_to_commit(&self, phase: MediaInstallPhase);

    /// Публикует ровно один lossless terminal record.
    fn publish_terminal(&self, completion: MediaInstallCompletion);
}

/// Внутреннее содержимое request-owned phase/completion slots.
#[derive(Debug, Default)]
struct MediaInstallSignalSlots {
    /// Ready phase хранится отдельно от terminal и забирается независимо.
    ready_to_commit: Option<MediaInstallPhase>,

    /// Terminal record не конкурирует с bounded shared event channel.
    terminal: Option<MediaInstallCompletion>,
}

/// Production request-owned port с lossless single-assignment slots.
#[derive(Debug, Default)]
struct RequestOwnedMediaInstallPort {
    /// Mutex задаёт linearizable publication/take без blocking I/O.
    slots: Mutex<MediaInstallSignalSlots>,
}

impl MediaInstallPhaseCompletionPort for RequestOwnedMediaInstallPort {
    fn publish_ready_to_commit(&self, phase: MediaInstallPhase) {
        let mut slots = self
            .slots
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        debug_assert!(
            slots.ready_to_commit.is_none(),
            "ready phase published twice"
        );
        debug_assert!(
            slots.terminal.is_none(),
            "ready phase published after terminal"
        );
        slots.ready_to_commit = Some(phase);
    }

    fn publish_terminal(&self, completion: MediaInstallCompletion) {
        let mut slots = self
            .slots
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        debug_assert!(
            slots.terminal.is_none(),
            "terminal completion published twice"
        );
        slots.terminal = Some(completion);
    }
}

/// Caller-owned receipt принятого install request-а.
///
/// Receipt не является install success: он лишь подтверждает command enqueue и даёт доступ
/// к отдельным `ReadyToCommit`/terminal slots.
#[derive(Debug, Clone)]
pub struct MediaInstallReceipt {
    /// Correlation identity этого receipt-а.
    request_id: MediaInstallRequestId,

    /// Shared request-owned storage остаётся живым до drain или drop receipt-а.
    port: Arc<RequestOwnedMediaInstallPort>,
}

impl MediaInstallReceipt {
    /// Создаёт receipt и writer port до неблокирующего command enqueue.
    pub(crate) fn new(
        request_id: MediaInstallRequestId,
    ) -> (Self, Arc<dyn MediaInstallPhaseCompletionPort>) {
        let port = Arc::new(RequestOwnedMediaInstallPort::default());
        let receipt = Self {
            request_id,
            port: Arc::clone(&port),
        };
        (receipt, port)
    }

    /// Возвращает request identity receipt-а.
    #[must_use]
    pub const fn request_id(&self) -> MediaInstallRequestId {
        self.request_id
    }

    /// Неблокирующе забирает `ReadyToCommit` exactly once.
    #[must_use]
    pub fn try_take_ready_to_commit(&self) -> Option<MediaInstallPhase> {
        self.port
            .slots
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .ready_to_commit
            .take()
    }

    /// Неблокирующе забирает terminal completion exactly once.
    #[must_use]
    pub fn try_take_completion(&self) -> Option<MediaInstallCompletion> {
        self.port
            .slots
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .terminal
            .take()
    }

    /// Требует exact `Installed` после уже полученного `AuthorizationAccepted`.
    ///
    /// `None`, cancellation или ordinary failure здесь являются fatal protocol invariant,
    /// а не recoverable candidate failure: player ownership уже обязан быть переключён.
    pub fn take_required_installed_after_authorization(
        &self,
    ) -> Result<MediaInstanceId, AcceptedMediaInstallTerminalError> {
        let Some(completion) = self.try_take_completion() else {
            return Err(AcceptedMediaInstallTerminalError::MissingInstalled {
                request_id: self.request_id,
            });
        };
        match completion {
            MediaInstallCompletion::Installed {
                request_id,
                media_instance_id,
            } if request_id == self.request_id => Ok(media_instance_id),
            MediaInstallCompletion::Installed { request_id, .. } => Err(
                AcceptedMediaInstallTerminalError::InstalledRequestMismatch {
                    expected_request_id: self.request_id,
                    installed_request_id: request_id,
                },
            ),
            unexpected_completion => Err(AcceptedMediaInstallTerminalError::UnexpectedCompletion(
                unexpected_completion,
            )),
        }
    }
}

/// Внутренняя request-owned phase state.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum MediaInstallProtocolState {
    /// Worker dequeued command, но ordinary preparation ещё не завершена.
    Accepted,

    /// Все ordinary fallible stages завершены; matching authorization допустима.
    ReadyToCommit,

    /// Terminal record опубликован, дальнейшие controls являются duplicate/stale.
    Terminal,
}

/// Маленькая owner state machine install barrier-а.
///
/// Compatibility path завершает её в одном worker turn, а Session 00C1 strong path
/// удерживает state machine рядом с bounded staged resource owner-ом до control barrier-а.
#[derive(Debug)]
pub(crate) struct MediaInstallProtocol {
    /// Exact request, которой принадлежит state machine.
    request_id: MediaInstallRequestId,

    /// Request-owned lossless publication port.
    port: Arc<dyn MediaInstallPhaseCompletionPort>,

    /// Текущая typed protocol phase.
    state: MediaInstallProtocolState,
}

impl MediaInstallProtocol {
    /// Фиксирует worker-owner acceptance без публикации ready/terminal.
    #[must_use]
    pub(crate) fn accept(
        request_id: MediaInstallRequestId,
        port: Arc<dyn MediaInstallPhaseCompletionPort>,
    ) -> Self {
        Self {
            request_id,
            port,
            state: MediaInstallProtocolState::Accepted,
        }
    }

    /// Публикует non-terminal ready phase после успешной ordinary preparation.
    pub(crate) fn mark_ready_to_commit(&mut self) {
        debug_assert_eq!(self.state, MediaInstallProtocolState::Accepted);
        self.port
            .publish_ready_to_commit(MediaInstallPhase::ReadyToCommit {
                request_id: self.request_id,
            });
        self.state = MediaInstallProtocolState::ReadyToCommit;
    }

    /// Сообщает owner-у, что authorization уже может безопасно потребить staged payload.
    pub(crate) fn is_ready_to_commit(&self) -> bool {
        self.state == MediaInstallProtocolState::ReadyToCommit
    }

    /// Публикует ordinary failure только до accepted authorization.
    pub(crate) fn complete_failed(&mut self, failure: MediaInstallFailure) {
        debug_assert_eq!(self.state, MediaInstallProtocolState::Accepted);
        self.port.publish_terminal(MediaInstallCompletion::Failed {
            request_id: self.request_id,
            failure,
        });
        self.state = MediaInstallProtocolState::Terminal;
    }

    /// Применяет следующий элемент единого ordered authorize/cancel/lifecycle stream.
    ///
    /// `commit_media_instance` вызывается только для matching ready authorization. Closure
    /// обязана быть infallible; `Installed` записывается до возврата и до следующего control.
    pub(crate) fn apply_control(
        &mut self,
        control: MediaInstallControl,
        commit_media_instance: impl FnOnce() -> MediaInstanceId,
    ) -> MediaInstallControlOutcome {
        let control_request_id = match control {
            MediaInstallControl::Authorize(authorization) => authorization.request_id,
            MediaInstallControl::Cancel(cancellation) => cancellation.request_id,
        };

        if control_request_id != self.request_id {
            return MediaInstallControlOutcome::StaleRequest;
        }
        if self.state == MediaInstallProtocolState::Terminal {
            return MediaInstallControlOutcome::AlreadyTerminal;
        }

        match control {
            MediaInstallControl::Authorize(_) => {
                if self.state != MediaInstallProtocolState::ReadyToCommit {
                    return MediaInstallControlOutcome::NotReady;
                }

                let media_instance_id = commit_media_instance();
                self.port
                    .publish_terminal(MediaInstallCompletion::Installed {
                        request_id: self.request_id,
                        media_instance_id,
                    });
                self.state = MediaInstallProtocolState::Terminal;
                MediaInstallControlOutcome::AuthorizationAccepted
            }
            MediaInstallControl::Cancel(cancellation) => {
                self.port
                    .publish_terminal(MediaInstallCompletion::Cancelled {
                        request_id: self.request_id,
                        cause: cancellation.cause,
                    });
                self.state = MediaInstallProtocolState::Terminal;
                MediaInstallControlOutcome::CancellationAccepted
            }
        }
    }
}

#[cfg(test)]
mod tests;
