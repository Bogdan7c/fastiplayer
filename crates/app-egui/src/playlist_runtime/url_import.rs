//! Latest-only извлечение yt-dlp topology для toolbar-действия Add URL.

use std::panic::{AssertUnwindSafe, catch_unwind};
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, LockResult, Mutex, MutexGuard};
use std::thread::{self, JoinHandle};

use fastiplayer_config::YtDlpConfig;

use crate::app_wake::AppWakePort;
use crate::media_open::WebOpenFailureReason;
use crate::process_shutdown::{
    FinishedThreadJoin, ProcessOwnerShutdownOutcome, ShutdownDeadline, join_thread_until,
};
use crate::url_topology_drafts::map_yt_dlp_topology_to_playlist_drafts;
use crate::web_open_message::{url_import_failure_message, url_import_internal_failure_message};

use super::PlaylistRuntime;
use super::import_transaction::{
    PlaylistImportDraft, PlaylistImportIntent, PlaylistImportIssue, PlaylistImportIssueKind,
};
use super::resolved_url_collection::ResolvedUrlCollection;

/// Нулевое значение не является job generation и используется для отмены exact request-а.
const NO_URL_IMPORT_GENERATION: u64 = 0;

/// Куда попадёт результат получения структуры ссылки (решение владельца 9, сессия 12).
///
/// Сам job (классификация, прогресс-строка, отмена, типизированные отказы) один и тот же;
/// различается только судьба успешного результата. Тип принадлежит владельцу URL-импорта,
/// чтобы второй job для «ссылки на видео» не дублировался.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) enum PlaylistUrlImportDestination {
    /// Кнопка «Добавить URL» и бросок на панель: результат идёт в S08 preview и дописывается
    /// в конец очереди (поведение без изменений).
    AppendToQueue,
    /// Бросок на видео: результат НЕ трогает очередь, а ждёт замены очереди через общий
    /// admission (подтверждение для непустой очереди) и воспроизведения первой строки.
    ReplaceAfterConfirmation,
}

/// Ошибка admission не содержит исходный URL или service diagnostics.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) enum PlaylistUrlImportStartError {
    /// Monotonic generation больше нельзя безопасно выдать.
    GenerationExhausted,
    /// Worker не запустился либо уже завершает process lifecycle.
    WorkerUnavailable,
}

/// Почему ссылка не превратилась в строки очереди (UX сессия 09).
///
/// Три исхода не сливаются: причина со стороны сайта/yt-dlp показывается пользователю,
/// отмена молчит, внутренний сбой приложения не выдаётся за отказ сайта.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) enum PlaylistUrlImportFailure {
    /// yt-dlp или сайт отказали; причина классифицирована `media-source-open`.
    Rejected(WebOpenFailureReason),
    /// Извлечение отменено (новый URL, кнопка «Отменить», shutdown) — не ошибка.
    Cancelled,
    /// Сбой внутри приложения: panic resolver-а, poisoned state, невалидный root locator.
    Internal,
}

/// Terminal result, который UI owner может применить только при exact generation match.
pub(super) enum PlaylistUrlImportCompletion {
    /// Topology успешно преобразована в source-neutral S08 draft.
    Resolved(PlaylistImportDraft),
    /// Extraction либо mapping завершились типизированной ошибкой.
    Failed(PlaylistUrlImportFailure),
}

/// Завершённый импорт вместе с доменом ссылки, которым подписывается текст ошибки.
pub(super) struct FinishedPlaylistUrlImport {
    /// Результат exact latest generation.
    completion: PlaylistUrlImportCompletion,
    /// Куда этот результат надо применить; фиксируется при submit и не меняется.
    destination: PlaylistUrlImportDestination,
    /// Безопасный домен («youtube.com»); `None` — у ссылки нет домена.
    display_host: Option<Arc<str>>,
}

/// Read-only факт «сейчас получаем структуру ссылки» для индикатора в toolbar.
///
/// Только домен: путь, query и токены ссылки сюда не попадают.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct PlaylistUrlImportProgress {
    /// Безопасный домен («youtube.com»); `None` — у ссылки нет домена.
    pub(crate) display_host: Option<Arc<str>>,
}

/// Итог явной отмены пользователем: отличает «отменили» от «нечего было отменять».
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum PlaylistUrlImportCancelOutcome {
    /// Идущее извлечение остановлено, его результат не попадёт в очередь.
    Cancelled,
    /// Активного извлечения не было (уже завершилось или не запускалось).
    NothingActive,
}

/// Service boundary скрывает process и mapping детали от lifecycle owner-а.
trait PlaylistUrlTopologyResolver: Send + Sync {
    /// Извлекает topology и строит ID-less draft без queue/player authority.
    fn resolve(
        &self,
        locator: &service_ytdlp::YtDlpMediaLocator,
        yt_dlp_config: &YtDlpConfig,
        sensitive_durable_locator_count: usize,
        is_cancelled: &dyn Fn() -> bool,
    ) -> Result<PlaylistImportDraft, PlaylistUrlImportFailure>;
}

/// Production resolver переиспользует S15 extraction и чистый S16 mapper.
struct ServicePlaylistUrlTopologyResolver;

impl PlaylistUrlTopologyResolver for ServicePlaylistUrlTopologyResolver {
    fn resolve(
        &self,
        locator: &service_ytdlp::YtDlpMediaLocator,
        yt_dlp_config: &YtDlpConfig,
        sensitive_durable_locator_count: usize,
        is_cancelled: &dyn Fn() -> bool,
    ) -> Result<PlaylistImportDraft, PlaylistUrlImportFailure> {
        // Service владеет процессом, bounded JSON contract и cooperative cancellation.
        let topology = service_ytdlp::YtDlpExtractorAdapter::default()
            .extract_topology_with_budgets(
                locator,
                yt_dlp_config,
                service_ytdlp::YtDlpTopologyBudgets::default(),
                web_media_core::ExtractorInvocationReason::CollectionTopologyResolution,
                is_cancelled,
            )
            .map_err(|error| topology_extraction_failure(&error))?;
        // App mapper сохраняет exact root provenance и service-owned child reopen identity.
        let preview =
            map_yt_dlp_topology_to_playlist_drafts(locator, &topology).map_err(|error| {
                // Root locator уже прошёл classification: отказ здесь — дефект приложения.
                tracing::error!(?error, "URL topology не превратилась в черновик очереди");
                PlaylistUrlImportFailure::Internal
            })?;
        // Source-neutral S08 preview пока показывает topology diagnostics общей категорией.
        let mut issues = preview
            .issues()
            .map(|_| PlaylistImportIssue::new(PlaylistImportIssueKind::SourceRejectedEntry))
            .collect::<Vec<_>>();
        // Отдельный marker сообщает UI, что bounded diagnostic prefix был усечён.
        if preview.omitted_issue_count() > 0 {
            issues.push(PlaylistImportIssue::new(
                PlaylistImportIssueKind::DiagnosticPrefixTruncated,
            ));
        }
        // IDs и capacity здесь не вычисляются: это остаётся единственной обязанностью S08.
        Ok(PlaylistImportDraft::new(
            preview.into_entries(),
            issues,
            None,
            sensitive_durable_locator_count,
        ))
    }
}

/// Переводит ошибку yt-dlp topology в причину и пишет диагностику в лог.
///
/// Display ошибки сервиса не содержит URL, stderr и argv (`service-ytdlp`), поэтому его
/// можно логировать целиком; пользователю уходит только типизированная причина.
fn topology_extraction_failure(
    error: &service_ytdlp::YtDlpTopologyError,
) -> PlaylistUrlImportFailure {
    match media_source_open::web_open_failure::classify_yt_dlp_topology_error(error) {
        Some(reason) => {
            tracing::warn!(?reason, %error, "yt-dlp не получил структуру URL");
            PlaylistUrlImportFailure::Rejected(reason)
        }
        None => PlaylistUrlImportFailure::Cancelled,
    }
}

/// Один immutable request, который worker забирает из заменяемого latest slot-а.
struct PlaylistUrlImportRequest {
    generation: u64,
    locator: service_ytdlp::YtDlpMediaLocator,
    yt_dlp_config: YtDlpConfig,
    sensitive_durable_locator_count: usize,
    resolver: Arc<dyn PlaylistUrlTopologyResolver>,
}

/// Completion остаётся внутри owner mailbox и никогда не переносится через winit event.
struct GenerationTaggedCompletion {
    generation: u64,
    completion: PlaylistUrlImportCompletion,
}

/// Mutex защищает заменяемый request, terminal slot и shutdown predicate вместе.
struct PlaylistUrlImportWorkerState {
    pending_request: Option<PlaylistUrlImportRequest>,
    completion: Option<GenerationTaggedCompletion>,
    shutdown_requested: bool,
}

impl PlaylistUrlImportWorkerState {
    /// Создаёт пустой reusable worker state до запуска потока.
    const fn new() -> Self {
        Self {
            pending_request: None,
            completion: None,
            shutdown_requested: false,
        }
    }
}

/// Последний принятый request с точки зрения UI owner-а.
struct ActivePlaylistUrlImport {
    /// Exact generation, результат которой ещё ждём.
    generation: u64,
    /// Безопасный домен для индикатора и текста ошибки.
    display_host: Option<Arc<str>>,
    /// Судьба результата; подмена destination у уже идущего job-а невозможна.
    destination: PlaylistUrlImportDestination,
}

/// Process-lifetime owner одного worker-а и exact latest generation fence.
pub(super) struct PlaylistUrlImportOwner {
    shared_state: Arc<Mutex<PlaylistUrlImportWorkerState>>,
    current_generation: Arc<AtomicU64>,
    next_generation: Option<u64>,
    /// `Some` от submit до drain/cancel: ровно то время, пока виден индикатор.
    active_request: Option<ActivePlaylistUrlImport>,
    /// Разобранная коллекция, ждущая admission замены очереди (`ReplaceAfterConfirmation`).
    /// Любая отмена/новый submit/shutdown очищает слот: устаревший результат не оживает.
    resolved_replacement: Option<ResolvedUrlCollection>,
    resolver: Arc<dyn PlaylistUrlTopologyResolver>,
    worker: Option<JoinHandle<()>>,
    state_poisoned: bool,
}

impl PlaylistUrlImportOwner {
    /// Запускает ровно один worker; failure остаётся typed и не ломает runtime construction.
    pub(super) fn new(wake_port: AppWakePort) -> Self {
        Self::with_resolver(wake_port, Arc::new(ServicePlaylistUrlTopologyResolver))
    }

    /// Dependency injection сохраняет production thread/lifecycle semantics в focused tests.
    fn with_resolver(
        wake_port: AppWakePort,
        resolver: Arc<dyn PlaylistUrlTopologyResolver>,
    ) -> Self {
        let shared_state = Arc::new(Mutex::new(PlaylistUrlImportWorkerState::new()));
        let current_generation = Arc::new(AtomicU64::new(NO_URL_IMPORT_GENERATION));
        let worker_state = Arc::clone(&shared_state);
        let worker_generation = Arc::clone(&current_generation);
        let worker = thread::Builder::new()
            .name("playlist-url-topology".to_owned())
            .spawn(move || url_import_worker_loop(worker_state, worker_generation, wake_port))
            .map_err(|error| {
                tracing::error!(%error, "Не удалось запустить worker импорта URL topology");
                error
            })
            .ok();
        Self {
            shared_state,
            current_generation,
            next_generation: Some(1),
            active_request: None,
            resolved_replacement: None,
            resolver,
            worker,
            state_poisoned: false,
        }
    }

    /// Заменяет pending request и cooperative-cancel-ит running extraction новой generation.
    pub(super) fn submit(
        &mut self,
        locator: service_ytdlp::YtDlpMediaLocator,
        yt_dlp_config: YtDlpConfig,
        sensitive_durable_locator_count: usize,
        display_host: Option<String>,
        destination: PlaylistUrlImportDestination,
    ) -> Result<(), PlaylistUrlImportStartError> {
        let Some(worker) = self.worker.as_ref() else {
            return Err(PlaylistUrlImportStartError::WorkerUnavailable);
        };
        let generation = self
            .next_generation
            .ok_or(PlaylistUrlImportStartError::GenerationExhausted)?;
        let mut shared_state = lock_worker_state(&self.shared_state).map_err(|_| {
            self.state_poisoned = true;
            PlaylistUrlImportStartError::WorkerUnavailable
        })?;
        if shared_state.shutdown_requested {
            return Err(PlaylistUrlImportStartError::WorkerUnavailable);
        }
        // Generation публикуется до request-а, чтобы running process сразу увидел отмену.
        self.current_generation.store(generation, Ordering::Release);
        shared_state.pending_request = Some(PlaylistUrlImportRequest {
            generation,
            locator,
            yt_dlp_config,
            sensitive_durable_locator_count,
            resolver: Arc::clone(&self.resolver),
        });
        // Новый intent атомарно делает даже уже опубликованный старый completion недействительным.
        shared_state.completion = None;
        self.active_request = Some(ActivePlaylistUrlImport {
            generation,
            display_host: display_host.map(Arc::from),
            destination,
        });
        // Новый intent делает недоставленную коллекцию прошлого job-а недействительной.
        self.resolved_replacement = None;
        self.next_generation = generation.checked_add(1);
        drop(shared_state);
        worker.thread().unpark();
        Ok(())
    }

    /// Отменяет running/pending request и удаляет недоставленный stale completion.
    ///
    /// Running process видит смену generation через `is_cancelled` и останавливается
    /// сервисом; его результат уже не пройдёт generation fence.
    pub(super) fn cancel_active(&mut self) -> PlaylistUrlImportCancelOutcome {
        self.current_generation
            .store(NO_URL_IMPORT_GENERATION, Ordering::Release);
        self.resolved_replacement = None;
        let outcome = match self.active_request.take() {
            Some(_) => PlaylistUrlImportCancelOutcome::Cancelled,
            None => PlaylistUrlImportCancelOutcome::NothingActive,
        };
        let Ok(mut shared_state) = lock_worker_state(&self.shared_state) else {
            self.state_poisoned = true;
            tracing::error!("URL topology owner обнаружил poisoned worker state при отмене");
            return outcome;
        };
        shared_state.pending_request = None;
        shared_state.completion = None;
        outcome
    }

    /// Индикатор «получаем структуру ссылки»: есть принятый, ещё не доставленный request.
    pub(super) fn progress(&self) -> Option<PlaylistUrlImportProgress> {
        self.active_request
            .as_ref()
            .map(|active| PlaylistUrlImportProgress {
                display_host: active.display_host.clone(),
            })
    }

    /// Неблокирующе забирает только exact latest terminal result.
    pub(super) fn drain(&mut self) -> Option<FinishedPlaylistUrlImport> {
        let Ok(mut shared_state) = lock_worker_state(&self.shared_state) else {
            self.state_poisoned = true;
            let active = self.active_request.take();
            let destination = active
                .as_ref()
                .map_or(PlaylistUrlImportDestination::AppendToQueue, |active| {
                    active.destination
                });
            let display_host = active.and_then(|active| active.display_host);
            self.current_generation
                .store(NO_URL_IMPORT_GENERATION, Ordering::Release);
            tracing::error!("URL topology owner обнаружил poisoned worker state при drain");
            return Some(FinishedPlaylistUrlImport {
                completion: PlaylistUrlImportCompletion::Failed(PlaylistUrlImportFailure::Internal),
                destination,
                display_host,
            });
        };
        let tagged = shared_state.completion.take()?;
        let is_exact_latest = self
            .active_request
            .as_ref()
            .is_some_and(|active| active.generation == tagged.generation);
        if !is_exact_latest || self.current_generation.load(Ordering::Acquire) != tagged.generation
        {
            return None;
        }
        let active = self.active_request.take()?;
        self.current_generation
            .store(NO_URL_IMPORT_GENERATION, Ordering::Release);
        Some(FinishedPlaylistUrlImport {
            completion: tagged.completion,
            destination: active.destination,
            display_host: active.display_host,
        })
    }

    /// Закрывает admission, будит idle worker и join-ит его в общем shutdown budget.
    pub(super) fn shutdown_until(
        &mut self,
        deadline: ShutdownDeadline,
    ) -> ProcessOwnerShutdownOutcome {
        let Some(worker) = self.worker.as_ref() else {
            return ProcessOwnerShutdownOutcome::AlreadyCompleted;
        };
        self.active_request = None;
        self.resolved_replacement = None;
        self.current_generation
            .store(NO_URL_IMPORT_GENERATION, Ordering::Release);
        match lock_worker_state(&self.shared_state) {
            Ok(mut shared_state) => {
                shared_state.shutdown_requested = true;
                shared_state.pending_request = None;
                shared_state.completion = None;
            }
            Err(_) => {
                self.state_poisoned = true;
                tracing::error!("URL topology owner обнаружил poisoned worker state при shutdown");
            }
        }
        worker.thread().unpark();
        match join_thread_until(&mut self.worker, deadline) {
            FinishedThreadJoin::AlreadyJoined | FinishedThreadJoin::Joined => {
                if self.state_poisoned {
                    ProcessOwnerShutdownOutcome::ThreadPanicked {
                        panicked_threads: 1,
                        pending_threads: 0,
                    }
                } else {
                    ProcessOwnerShutdownOutcome::Completed
                }
            }
            FinishedThreadJoin::StillRunning => {
                ProcessOwnerShutdownOutcome::TimedOut { pending_threads: 1 }
            }
            FinishedThreadJoin::Panicked => ProcessOwnerShutdownOutcome::ThreadPanicked {
                panicked_threads: 1,
                pending_threads: 0,
            },
        }
    }

    /// Кладёт разобранную коллекцию в слот ожидания admission (заменяет прежнюю).
    pub(super) fn park_resolved_replacement(&mut self, collection: ResolvedUrlCollection) {
        self.resolved_replacement = Some(collection);
    }

    /// Забирает разобранную коллекцию ровно один раз; после отмены слот пуст.
    pub(super) fn take_resolved_replacement(&mut self) -> Option<ResolvedUrlCollection> {
        self.resolved_replacement.take()
    }

    #[cfg(test)]
    /// Подменяет только resolver будущих requests, не меняя worker/generation semantics.
    fn replace_resolver_for_test(&mut self, resolver: Arc<dyn PlaylistUrlTopologyResolver>) {
        self.resolver = resolver;
    }
}

impl PlaylistRuntime {
    /// Передаёт уже classified yt-dlp locator latest-only owner-у без повторного parser-а.
    pub(in crate::playlist_runtime) fn start_playlist_url_import(
        &mut self,
        locator: service_ytdlp::YtDlpMediaLocator,
        yt_dlp_config: YtDlpConfig,
        sensitive_durable_locator_count: usize,
        display_host: Option<String>,
        destination: PlaylistUrlImportDestination,
    ) -> Result<(), PlaylistUrlImportStartError> {
        self.url_import.submit(
            locator,
            yt_dlp_config,
            sensitive_durable_locator_count,
            display_host,
            destination,
        )
    }

    /// Общий supersede boundary cooperative-cancel-ит process и stale terminal slot.
    pub(in crate::playlist_runtime) fn cancel_playlist_url_import(&mut self) {
        // Supersede-у неважно, было ли что отменять: новый intent заменяет любой старый.
        let _superseded = self.url_import.cancel_active();
    }

    /// Кнопка «Отменить» у индикатора: останавливает только получение структуры ссылки.
    ///
    /// Очередь, текущий трек, player, staged preview и черновик URL-формы не меняются;
    /// отмена молчит — это не ошибка для пользователя.
    pub(crate) fn cancel_playlist_url_import_by_user(&mut self) -> PlaylistUrlImportCancelOutcome {
        let outcome = self.url_import.cancel_active();
        tracing::debug!(?outcome, "Пользователь отменил получение структуры URL");
        outcome
    }

    /// Read-only индикатор для toolbar: идёт ли получение структуры ссылки.
    pub(crate) fn playlist_url_import_progress(&self) -> Option<PlaylistUrlImportProgress> {
        self.url_import.progress()
    }

    /// UI-thread drain передаёт exact latest result единственной S08 transaction.
    pub(in crate::playlist_runtime) fn drain_playlist_url_import_job(&mut self) -> bool {
        let Some(FinishedPlaylistUrlImport {
            completion,
            destination,
            display_host,
        }) = self.url_import.drain()
        else {
            return false;
        };
        match completion {
            PlaylistUrlImportCompletion::Resolved(draft)
                if destination == PlaylistUrlImportDestination::ReplaceAfterConfirmation =>
            {
                // Очередь не трогаем: коллекция ждёт, пока оболочка проведёт её через admission.
                self.url_import.park_resolved_replacement(
                    ResolvedUrlCollection::from_resolved_draft(draft, display_host),
                );
            }
            PlaylistUrlImportCompletion::Resolved(draft) => {
                if let Err(error) =
                    self.stage_playlist_import(PlaylistImportIntent::AppendToQueue, draft)
                {
                    tracing::warn!(?error, "URL topology preview не прошёл S08 staging");
                    self.set_playlist_safe_feedback(
                        "Импорт URL устарел или сейчас недоступен; добавьте URL ещё раз",
                    );
                }
            }
            PlaylistUrlImportCompletion::Failed(failure) => {
                self.report_playlist_url_import_failure(failure, display_host.as_deref());
            }
        }
        true
    }

    /// Показывает причину отказа в области проблем плейлиста; отмена молчит.
    fn report_playlist_url_import_failure(
        &mut self,
        failure: PlaylistUrlImportFailure,
        display_host: Option<&str>,
    ) {
        match failure {
            PlaylistUrlImportFailure::Rejected(reason) => {
                self.set_playlist_safe_feedback(url_import_failure_message(display_host, reason));
            }
            PlaylistUrlImportFailure::Internal => {
                self.set_playlist_safe_feedback(url_import_internal_failure_message(display_host));
            }
            PlaylistUrlImportFailure::Cancelled => {}
        }
    }
}

impl Drop for PlaylistUrlImportOwner {
    fn drop(&mut self) {
        let Some(worker) = self.worker.as_ref() else {
            return;
        };
        self.current_generation
            .store(NO_URL_IMPORT_GENERATION, Ordering::Release);
        if let Ok(mut shared_state) = lock_worker_state(&self.shared_state) {
            shared_state.shutdown_requested = true;
            shared_state.pending_request = None;
            shared_state.completion = None;
        }
        worker.thread().unpark();
    }
}

/// Один поток последовательно исполняет только latest request из bounded slot-а.
fn url_import_worker_loop(
    shared_state: Arc<Mutex<PlaylistUrlImportWorkerState>>,
    current_generation: Arc<AtomicU64>,
    wake_port: AppWakePort,
) {
    loop {
        let request = {
            let Ok(mut worker_state) = lock_worker_state(&shared_state) else {
                tracing::error!("URL topology worker остановлен из-за poisoned state");
                return;
            };
            if worker_state.shutdown_requested {
                return;
            }
            worker_state.pending_request.take()
        };
        let Some(request) = request else {
            // `unpark` хранит token, поэтому publish между проверкой и park не теряется.
            thread::park();
            continue;
        };
        let request_generation = request.generation;
        let cancellation_generation = Arc::clone(&current_generation);
        // Resolver panic изолируется внутри reusable worker-а и становится safe failure.
        let resolution = catch_unwind(AssertUnwindSafe(|| {
            request.resolver.resolve(
                &request.locator,
                &request.yt_dlp_config,
                request.sensitive_durable_locator_count,
                &|| cancellation_generation.load(Ordering::Acquire) != request_generation,
            )
        }));
        let completion = match resolution {
            Ok(Ok(draft)) => PlaylistUrlImportCompletion::Resolved(draft),
            Ok(Err(failure)) => PlaylistUrlImportCompletion::Failed(failure),
            Err(_) => {
                tracing::error!("URL topology resolver завершился panic без раскрытия locator-а");
                PlaylistUrlImportCompletion::Failed(PlaylistUrlImportFailure::Internal)
            }
        };
        let Ok(mut worker_state) = lock_worker_state(&shared_state) else {
            tracing::error!("URL topology worker остановлен из-за poisoned completion state");
            return;
        };
        if worker_state.shutdown_requested
            || current_generation.load(Ordering::Acquire) != request_generation
        {
            continue;
        }
        worker_state.completion = Some(GenerationTaggedCompletion {
            generation: request_generation,
            completion,
        });
        drop(worker_state);
        let _delivery = wake_port.request_wake();
    }
}

/// Lock result остаётся fallible: poisoned state нельзя использовать повторно.
fn lock_worker_state(
    shared_state: &Mutex<PlaylistUrlImportWorkerState>,
) -> LockResult<MutexGuard<'_, PlaylistUrlImportWorkerState>> {
    shared_state.lock()
}

#[cfg(test)]
mod tests;
