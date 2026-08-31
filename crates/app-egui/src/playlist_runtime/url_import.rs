//! Latest-only извлечение yt-dlp topology для toolbar-действия Add URL.

use std::panic::{AssertUnwindSafe, catch_unwind};
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, LockResult, Mutex, MutexGuard};
use std::thread::{self, JoinHandle};

use rustiplayer_config::YtDlpConfig;

use crate::app_wake::AppWakePort;
use crate::process_shutdown::{
    FinishedThreadJoin, ProcessOwnerShutdownOutcome, ShutdownDeadline, join_thread_until,
};
use crate::url_topology_drafts::map_yt_dlp_topology_to_playlist_drafts;

use super::PlaylistRuntime;
use super::import_transaction::{
    PlaylistImportDraft, PlaylistImportIntent, PlaylistImportIssue, PlaylistImportIssueKind,
};

/// Нулевое значение не является job generation и используется для отмены exact request-а.
const NO_URL_IMPORT_GENERATION: u64 = 0;

/// Ошибка admission не содержит исходный URL или service diagnostics.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) enum PlaylistUrlImportStartError {
    /// Monotonic generation больше нельзя безопасно выдать.
    GenerationExhausted,
    /// Worker не запустился либо уже завершает process lifecycle.
    WorkerUnavailable,
}

/// Terminal result, который UI owner может применить только при exact generation match.
pub(super) enum PlaylistUrlImportCompletion {
    /// Topology успешно преобразована в source-neutral S08 draft.
    Resolved(PlaylistImportDraft),
    /// Extraction либо mapping завершились безопасной общей ошибкой.
    Failed,
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
    ) -> Result<PlaylistImportDraft, ()>;
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
    ) -> Result<PlaylistImportDraft, ()> {
        // Service владеет процессом, bounded JSON contract и cooperative cancellation.
        let topology = service_ytdlp::YtDlpExtractorAdapter::default()
            .extract_topology_with_budgets(
                locator,
                yt_dlp_config,
                service_ytdlp::YtDlpTopologyBudgets::default(),
                web_media_core::ExtractorInvocationReason::CollectionTopologyResolution,
                is_cancelled,
            )
            .map_err(|_| ())?;
        // App mapper сохраняет exact root provenance и service-owned child reopen identity.
        let preview = map_yt_dlp_topology_to_playlist_drafts(locator, &topology).map_err(|_| ())?;
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

/// Process-lifetime owner одного worker-а и exact latest generation fence.
pub(super) struct PlaylistUrlImportOwner {
    shared_state: Arc<Mutex<PlaylistUrlImportWorkerState>>,
    current_generation: Arc<AtomicU64>,
    next_generation: Option<u64>,
    latest_generation: Option<u64>,
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
            latest_generation: None,
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
        self.latest_generation = Some(generation);
        self.next_generation = generation.checked_add(1);
        drop(shared_state);
        worker.thread().unpark();
        Ok(())
    }

    /// Отменяет running/pending request и удаляет недоставленный stale completion.
    pub(super) fn cancel_active(&mut self) {
        self.current_generation
            .store(NO_URL_IMPORT_GENERATION, Ordering::Release);
        self.latest_generation = None;
        let Ok(mut shared_state) = lock_worker_state(&self.shared_state) else {
            self.state_poisoned = true;
            tracing::error!("URL topology owner обнаружил poisoned worker state при отмене");
            return;
        };
        shared_state.pending_request = None;
        shared_state.completion = None;
    }

    /// Неблокирующе забирает только exact latest terminal result.
    pub(super) fn drain(&mut self) -> Option<PlaylistUrlImportCompletion> {
        let Ok(mut shared_state) = lock_worker_state(&self.shared_state) else {
            self.state_poisoned = true;
            self.latest_generation = None;
            self.current_generation
                .store(NO_URL_IMPORT_GENERATION, Ordering::Release);
            tracing::error!("URL topology owner обнаружил poisoned worker state при drain");
            return Some(PlaylistUrlImportCompletion::Failed);
        };
        let tagged = shared_state.completion.take()?;
        if self.latest_generation != Some(tagged.generation)
            || self.current_generation.load(Ordering::Acquire) != tagged.generation
        {
            return None;
        }
        self.latest_generation = None;
        self.current_generation
            .store(NO_URL_IMPORT_GENERATION, Ordering::Release);
        Some(tagged.completion)
    }

    /// Закрывает admission, будит idle worker и join-ит его в общем shutdown budget.
    pub(super) fn shutdown_until(
        &mut self,
        deadline: ShutdownDeadline,
    ) -> ProcessOwnerShutdownOutcome {
        let Some(worker) = self.worker.as_ref() else {
            return ProcessOwnerShutdownOutcome::AlreadyCompleted;
        };
        self.latest_generation = None;
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
    ) -> Result<(), PlaylistUrlImportStartError> {
        self.url_import
            .submit(locator, yt_dlp_config, sensitive_durable_locator_count)
    }

    /// Общий supersede boundary cooperative-cancel-ит process и stale terminal slot.
    pub(in crate::playlist_runtime) fn cancel_playlist_url_import(&mut self) {
        self.url_import.cancel_active();
    }

    /// UI-thread drain передаёт exact latest result единственной S08 transaction.
    pub(in crate::playlist_runtime) fn drain_playlist_url_import_job(&mut self) -> bool {
        let Some(completion) = self.url_import.drain() else {
            return false;
        };
        match completion {
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
            PlaylistUrlImportCompletion::Failed => {
                self.set_playlist_safe_feedback("Не удалось получить структуру media URL");
            }
        }
        true
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
            Ok(Err(())) => PlaylistUrlImportCompletion::Failed,
            Err(_) => {
                tracing::error!("URL topology resolver завершился panic без раскрытия locator-а");
                PlaylistUrlImportCompletion::Failed
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
