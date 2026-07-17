//! Bounded process-lifetime enrichment YtDlp metadata без UI/network ownership в domain.

use std::collections::{HashMap, VecDeque};
use std::num::NonZeroUsize;
use std::sync::Arc;
use std::time::{Duration, Instant};

use bounded_work_executor::{
    BoundedExecutor, CancellationToken, ExecutorConfig, SubmitError, TaskFailure, TaskHandle,
    TaskPoll,
};
use playlist_core::{PlaylistItemId, PlaylistLocator, PlaylistMetadataPatch};
use rustiplayer_config::YtDlpConfig;

use crate::app_wake::AppWakePort;
use crate::playlist_runtime::controller::PlaylistController;

/// Два worker-а ограничивают число одновременных внешних `yt-dlp` process-ов.
const YT_DLP_METADATA_WORKER_THREADS: NonZeroUsize = NonZeroUsize::new(2).unwrap();

/// Очередь executor-а ограничивает уже принятые, но ещё не запущенные process-задачи.
const YT_DLP_METADATA_EXECUTOR_QUEUE_CAPACITY: NonZeroUsize = NonZeroUsize::new(16).unwrap();

/// Общий предел running + executor-queued задач, для которых owner хранит handles.
const YT_DLP_METADATA_IN_FLIGHT_LIMIT: usize =
    YT_DLP_METADATA_WORKER_THREADS.get() + YT_DLP_METADATA_EXECUTOR_QUEUE_CAPACITY.get();

/// Pending demands ограничены независимо от потенциального размера playlist-а.
const YT_DLP_METADATA_PENDING_LIMIT: usize = 256;

/// Временная service/network ошибка повторяется, но не создаёт process каждый frame.
const YT_DLP_METADATA_RETRY_DELAY: Duration = Duration::from_secs(30);

/// Один exact playlist item, locator и process policy для фонового enrichment-а.
#[derive(Clone)]
pub(in crate::playlist_runtime) struct YtDlpMetadataDemand {
    item_id: PlaylistItemId,
    expected_locator: PlaylistLocator,
    yt_dlp_locator: service_ytdlp::YtDlpMediaLocator,
    yt_dlp_config: YtDlpConfig,
}

impl YtDlpMetadataDemand {
    pub(in crate::playlist_runtime) fn new(
        item_id: PlaylistItemId,
        expected_locator: PlaylistLocator,
        yt_dlp_locator: service_ytdlp::YtDlpMediaLocator,
        yt_dlp_config: YtDlpConfig,
    ) -> Self {
        Self {
            item_id,
            expected_locator,
            yt_dlp_locator,
            yt_dlp_config,
        }
    }
}

/// Bounded admission result не смешивает coalescing, backpressure и disabled policy.
#[derive(Debug, Default, Clone, Copy, PartialEq, Eq)]
pub(in crate::playlist_runtime) struct YtDlpMetadataRequestOutcome {
    pub(super) accepted: usize,
    pub(super) coalesced: usize,
    pub(super) dropped_by_bound: usize,
    pub(super) disabled_by_config: usize,
    pub(super) executor_unavailable: usize,
}

/// Нормализованный task result без secret-bearing service diagnostics.
#[derive(Debug)]
pub(in crate::playlist_runtime) enum YtDlpMetadataTaskOutcome {
    Resolved {
        title: Option<String>,
        duration: Option<Duration>,
    },
    Cancelled,
    Failed,
}

/// Injectable service boundary позволяет тестам не запускать сеть и `yt-dlp`.
pub(in crate::playlist_runtime) trait YtDlpMetadataResolver:
    Send + Sync
{
    fn resolve(
        &self,
        locator: &service_ytdlp::YtDlpMediaLocator,
        yt_dlp_config: &YtDlpConfig,
        cancellation: &CancellationToken,
    ) -> YtDlpMetadataTaskOutcome;
}

struct ServiceYtDlpMetadataResolver;

impl YtDlpMetadataResolver for ServiceYtDlpMetadataResolver {
    fn resolve(
        &self,
        locator: &service_ytdlp::YtDlpMediaLocator,
        yt_dlp_config: &YtDlpConfig,
        cancellation: &CancellationToken,
    ) -> YtDlpMetadataTaskOutcome {
        if cancellation.is_cancelled() {
            return YtDlpMetadataTaskOutcome::Cancelled;
        }
        let metadata = service_ytdlp::resolve_yt_dlp_playlist_metadata_with_config(
            locator,
            yt_dlp_config,
            || cancellation.is_cancelled(),
        );
        match metadata {
            Ok(metadata) if !cancellation.is_cancelled() => YtDlpMetadataTaskOutcome::Resolved {
                title: metadata.title().map(ToOwned::to_owned),
                duration: metadata.duration(),
            },
            Ok(_) => YtDlpMetadataTaskOutcome::Cancelled,
            Err(_) if cancellation.is_cancelled() => YtDlpMetadataTaskOutcome::Cancelled,
            Err(_) => YtDlpMetadataTaskOutcome::Failed,
        }
    }
}

struct ActiveYtDlpMetadataJob {
    demand: YtDlpMetadataDemand,
    handle: TaskHandle<YtDlpMetadataTaskOutcome>,
}

struct FailedYtDlpMetadataAttempt {
    expected_locator: PlaylistLocator,
    yt_dlp_config: YtDlpConfig,
    failed_at: Instant,
}

/// Process-lifetime owner admission, cancellation, retry и exact cache patch-а.
pub(super) struct YtDlpMetadataOwner {
    executor: Option<BoundedExecutor>,
    resolver: Arc<dyn YtDlpMetadataResolver>,
    wake_port: AppWakePort,
    pending: VecDeque<YtDlpMetadataDemand>,
    active: Vec<ActiveYtDlpMetadataJob>,
    resolved: HashMap<PlaylistItemId, PlaylistLocator>,
    failed: HashMap<PlaylistItemId, FailedYtDlpMetadataAttempt>,
    admission_open: bool,
}

impl YtDlpMetadataOwner {
    pub(super) fn new(wake_port: AppWakePort) -> Self {
        let executor = BoundedExecutor::start(ExecutorConfig::new(
            YT_DLP_METADATA_WORKER_THREADS,
            YT_DLP_METADATA_EXECUTOR_QUEUE_CAPACITY,
            "playlist-yt_dlp-metadata",
        ))
        .ok();
        Self::with_dependencies(wake_port, executor, Arc::new(ServiceYtDlpMetadataResolver))
    }

    fn with_dependencies(
        wake_port: AppWakePort,
        executor: Option<BoundedExecutor>,
        resolver: Arc<dyn YtDlpMetadataResolver>,
    ) -> Self {
        Self {
            executor,
            resolver,
            wake_port,
            pending: VecDeque::new(),
            active: Vec::new(),
            resolved: HashMap::new(),
            failed: HashMap::new(),
            admission_open: true,
        }
    }

    /// Принимает demands без blocking I/O и coalesce-ит exact Item ID.
    pub(super) fn request(
        &mut self,
        demands: Vec<YtDlpMetadataDemand>,
        now: Instant,
    ) -> YtDlpMetadataRequestOutcome {
        let mut outcome = YtDlpMetadataRequestOutcome::default();
        for demand in demands {
            if !demand.yt_dlp_config.enabled {
                outcome.disabled_by_config += 1;
                continue;
            }
            if !self.admission_open || self.executor.is_none() {
                outcome.executor_unavailable += 1;
                continue;
            }
            if self.is_coalesced(&demand, now) {
                outcome.coalesced += 1;
                continue;
            }
            if self.pending.len() == YT_DLP_METADATA_PENDING_LIMIT {
                outcome.dropped_by_bound += 1;
                continue;
            }
            self.failed.remove(&demand.item_id);
            self.pending.push_back(demand);
            outcome.accepted += 1;
        }
        self.start_pending();
        outcome
    }

    /// Drains terminal slots, применяет metadata и запускает следующий bounded prefix.
    pub(super) fn drain(&mut self, controller: &mut PlaylistController, now: Instant) -> bool {
        self.prune_owner_state(controller);
        let mut visible_change = false;
        let mut still_active = Vec::with_capacity(self.active.len());
        for active_job in std::mem::take(&mut self.active) {
            match active_job.handle.try_take() {
                TaskPoll::Pending => still_active.push(active_job),
                TaskPoll::Completed(YtDlpMetadataTaskOutcome::Resolved { title, duration }) => {
                    match apply_resolved_metadata(controller, &active_job.demand, title, duration) {
                        YtDlpMetadataApplyOutcome::Applied => {
                            self.mark_resolved(&active_job.demand);
                            visible_change = true;
                        }
                        YtDlpMetadataApplyOutcome::NoChange => {
                            self.mark_resolved(&active_job.demand);
                        }
                        YtDlpMetadataApplyOutcome::Stale => {}
                        YtDlpMetadataApplyOutcome::Rejected => {
                            self.mark_failed(active_job.demand, now);
                        }
                    }
                }
                TaskPoll::Completed(YtDlpMetadataTaskOutcome::Cancelled)
                | TaskPoll::Failed(TaskFailure::CancelledBeforeStart) => {}
                TaskPoll::Completed(YtDlpMetadataTaskOutcome::Failed)
                | TaskPoll::Failed(TaskFailure::Panicked | TaskFailure::ExecutorStopped) => {
                    self.mark_failed(active_job.demand, now);
                }
            }
        }
        self.active = still_active;
        self.start_pending();
        visible_change
    }

    /// Queue replacement/Clear отменяет только enrichment, не затрагивая playback.
    pub(super) fn cancel_for_queue_replacement(&mut self) {
        self.pending.clear();
        for active_job in &self.active {
            active_job.handle.cancel();
        }
        self.active.clear();
        self.resolved.clear();
        self.failed.clear();
    }

    /// Shutdown закрывает admission и cooperative-cancel-ит running `yt-dlp`.
    pub(super) fn begin_shutdown(&mut self) {
        self.admission_open = false;
        self.cancel_for_queue_replacement();
        if let Some(executor) = &self.executor {
            executor.shutdown();
        }
    }

    #[cfg(test)]
    pub(super) fn replace_resolver_for_test(&mut self, resolver: Arc<dyn YtDlpMetadataResolver>) {
        self.cancel_for_queue_replacement();
        self.resolver = resolver;
    }

    fn is_coalesced(&self, demand: &YtDlpMetadataDemand, now: Instant) -> bool {
        if self
            .resolved
            .get(&demand.item_id)
            .is_some_and(|locator| locator == &demand.expected_locator)
            || self
                .pending
                .iter()
                .any(|queued| queued.item_id == demand.item_id)
            || self
                .active
                .iter()
                .any(|active| active.demand.item_id == demand.item_id)
        {
            return true;
        }
        self.failed.get(&demand.item_id).is_some_and(|failed| {
            failed.expected_locator == demand.expected_locator
                && failed.yt_dlp_config == demand.yt_dlp_config
                && now.saturating_duration_since(failed.failed_at) < YT_DLP_METADATA_RETRY_DELAY
        })
    }

    fn start_pending(&mut self) {
        let Some(executor) = self.executor.as_ref() else {
            return;
        };
        while self.active.len() < YT_DLP_METADATA_IN_FLIGHT_LIMIT {
            let Some(demand) = self.pending.pop_front() else {
                break;
            };
            let resolver = Arc::clone(&self.resolver);
            let locator = demand.yt_dlp_locator.clone();
            let yt_dlp_config = demand.yt_dlp_config.clone();
            let wake_port = self.wake_port.clone();
            match executor.try_submit_with_terminal_notifier(
                move |cancellation| resolver.resolve(&locator, &yt_dlp_config, &cancellation),
                move || {
                    let _wake_delivery = wake_port.request_wake();
                },
            ) {
                Ok(handle) => self.active.push(ActiveYtDlpMetadataJob { demand, handle }),
                Err(SubmitError::QueueFull) => {
                    self.pending.push_front(demand);
                    break;
                }
                Err(SubmitError::ShuttingDown) => break,
            }
        }
    }

    fn prune_owner_state(&mut self, controller: &PlaylistController) {
        self.pending
            .retain(|demand| demand_still_current(controller, demand));
        self.resolved.retain(|item_id, expected_locator| {
            controller
                .queue()
                .item(*item_id)
                .is_some_and(|item| item.locator() == expected_locator)
        });
        self.failed.retain(|item_id, failed| {
            controller
                .queue()
                .item(*item_id)
                .is_some_and(|item| item.locator() == &failed.expected_locator)
        });
    }

    fn mark_resolved(&mut self, demand: &YtDlpMetadataDemand) {
        self.failed.remove(&demand.item_id);
        self.resolved
            .insert(demand.item_id, demand.expected_locator.clone());
    }

    fn mark_failed(&mut self, demand: YtDlpMetadataDemand, failed_at: Instant) {
        self.failed.insert(
            demand.item_id,
            FailedYtDlpMetadataAttempt {
                expected_locator: demand.expected_locator,
                yt_dlp_config: demand.yt_dlp_config,
                failed_at,
            },
        );
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum YtDlpMetadataApplyOutcome {
    Applied,
    NoChange,
    Stale,
    Rejected,
}

/// Merge выполняется поверх последнего cache snapshot-а, поэтому playback metadata не стирается.
fn apply_resolved_metadata(
    controller: &mut PlaylistController,
    demand: &YtDlpMetadataDemand,
    resolved_title: Option<String>,
    resolved_duration: Option<Duration>,
) -> YtDlpMetadataApplyOutcome {
    let Some(item) = controller.queue().item(demand.item_id) else {
        return YtDlpMetadataApplyOutcome::Stale;
    };
    if item.locator() != &demand.expected_locator {
        return YtDlpMetadataApplyOutcome::Stale;
    }

    let current_metadata = item.cached_metadata();
    let title = resolved_title.or_else(|| current_metadata.title().map(ToOwned::to_owned));
    let duration = current_metadata
        .duration()
        .or_else(|| resolved_duration.map(media_core::MediaDuration::from_duration));
    let merged_metadata = current_metadata
        .clone()
        .with_title(title)
        .with_duration(duration);
    let patch = PlaylistMetadataPatch::new(
        demand.item_id,
        demand.expected_locator.clone(),
        item.local_fingerprint(),
        merged_metadata,
    );
    match controller.apply_metadata_patches(vec![patch]) {
        Ok(outcome) if outcome.domain.changed_metadata() => YtDlpMetadataApplyOutcome::Applied,
        Ok(_) => YtDlpMetadataApplyOutcome::NoChange,
        Err(_) => YtDlpMetadataApplyOutcome::Rejected,
    }
}

fn demand_still_current(controller: &PlaylistController, demand: &YtDlpMetadataDemand) -> bool {
    controller
        .queue()
        .item(demand.item_id)
        .is_some_and(|item| item.locator() == &demand.expected_locator)
}

#[cfg(test)]
mod tests;
