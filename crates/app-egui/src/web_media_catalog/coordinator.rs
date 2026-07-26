use std::num::NonZeroUsize;
use std::sync::Arc;
use std::time::Instant;

use bounded_work_executor::{
    BoundedExecutor, ExecutorConfig, SubmitError, TaskFailure, TaskHandle, TaskPoll,
};
use player_core::MediaInstanceId;
use playlist_core::PlaylistItemId;
use web_media_core::ExactSelectionIdentity;

use crate::app_wake::AppWakePort;
use crate::playlist_runtime::PlaylistRuntimeBinding;
use crate::web_media_stream_model::WebMediaStreamGeneration;

use super::discovery::{DiscoveredWebMediaCatalog, WebMediaCatalogAttachment};
use super::model::{WebMediaCatalogSafeError, WebMediaCatalogState, WebMediaVerifiedCatalog};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum WebMediaCatalogScope {
    Item(PlaylistItemId),
    Detached,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct WebMediaCatalogCorrelation {
    pub(crate) scope: WebMediaCatalogScope,
    pub(crate) parent: ExactSelectionIdentity,
    pub(crate) media_instance: MediaInstanceId,
    pub(crate) binding: PlaylistRuntimeBinding,
    pub(crate) parent_generation: WebMediaStreamGeneration,
}

struct CatalogRequest {
    generation: u64,
    correlation: WebMediaCatalogCorrelation,
    attachment: WebMediaCatalogAttachment,
}

struct CatalogCompletion {
    generation: u64,
    correlation: WebMediaCatalogCorrelation,
    result: anyhow::Result<DiscoveredWebMediaCatalog>,
}

struct RunningCatalogRequest {
    generation: u64,
    correlation: WebMediaCatalogCorrelation,
    source_cancellation: source_core::CancellationToken,
    task: TaskHandle<CatalogCompletion>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) struct WebMediaCatalogShutdownReport {
    pub(crate) detached_deadline_workers: usize,
    pub(crate) panicked_workers: usize,
}

pub(crate) struct WebMediaCatalogCoordinator {
    executor: Option<BoundedExecutor>,
    wake_port: AppWakePort,
    next_generation: u64,
    active_correlation: Option<WebMediaCatalogCorrelation>,
    running: Option<RunningCatalogRequest>,
    latest_pending: Option<CatalogRequest>,
    visible: WebMediaCatalogState,
}

impl WebMediaCatalogCoordinator {
    pub(crate) fn new(wake_port: AppWakePort) -> Self {
        let executor = BoundedExecutor::start(ExecutorConfig::new(
            NonZeroUsize::new(1).expect("catalog worker count is non-zero"),
            NonZeroUsize::new(1).expect("catalog queue capacity is non-zero"),
            "web-media-catalog",
        ))
        .ok();
        Self {
            executor,
            wake_port,
            next_generation: 0,
            active_correlation: None,
            running: None,
            latest_pending: None,
            visible: WebMediaCatalogState::Inactive,
        }
    }

    pub(crate) fn ensure(
        &mut self,
        correlation: WebMediaCatalogCorrelation,
        attachment: WebMediaCatalogAttachment,
    ) {
        if attachment.parent() != &correlation.parent {
            self.visible = WebMediaCatalogState::Failed {
                parent_generation: correlation.parent_generation,
                error: WebMediaCatalogSafeError::DiscoveryFailed,
            };
            return;
        }
        if self.active_correlation.as_ref() == Some(&correlation) {
            return;
        }
        self.cancel_running();
        self.next_generation = self.next_generation.saturating_add(1);
        self.active_correlation = Some(correlation.clone());
        self.visible = WebMediaCatalogState::Loading {
            parent_generation: correlation.parent_generation,
        };
        let request = CatalogRequest {
            generation: self.next_generation,
            correlation,
            attachment,
        };
        if self.running.is_some() {
            self.latest_pending = Some(request);
        } else {
            self.submit(request);
        }
    }

    pub(crate) fn clear(&mut self) {
        self.cancel_running();
        self.latest_pending = None;
        self.active_correlation = None;
        self.visible = WebMediaCatalogState::Inactive;
    }

    pub(crate) fn drain(&mut self) -> bool {
        let Some(running) = self.running.take() else {
            return false;
        };
        let poll = running.task.try_take();
        match poll {
            TaskPoll::Pending => {
                self.running = Some(running);
                false
            }
            TaskPoll::Completed(completion) => {
                let current = completion.generation == running.generation
                    && completion.correlation == running.correlation
                    && self.active_correlation.as_ref() == Some(&completion.correlation);
                if let Some(pending) = self.latest_pending.take() {
                    self.submit(pending);
                    return true;
                }
                if current {
                    self.visible = match completion.result {
                        Ok(discovered) => WebMediaVerifiedCatalog::new(
                            completion.generation,
                            completion.correlation.parent_generation,
                            discovered.choices,
                            &discovered.active,
                            discovered.rejected_siblings,
                        )
                        .map(Arc::new)
                        .map(WebMediaCatalogState::Ready)
                        .unwrap_or(WebMediaCatalogState::Failed {
                            parent_generation: completion.correlation.parent_generation,
                            error: WebMediaCatalogSafeError::DiscoveryFailed,
                        }),
                        Err(_) => WebMediaCatalogState::Failed {
                            parent_generation: completion.correlation.parent_generation,
                            error: WebMediaCatalogSafeError::DiscoveryFailed,
                        },
                    };
                }
                true
            }
            TaskPoll::Failed(failure) => {
                if let Some(pending) = self.latest_pending.take() {
                    self.submit(pending);
                    return true;
                }
                if self.active_correlation.as_ref() == Some(&running.correlation) {
                    self.visible = WebMediaCatalogState::Failed {
                        parent_generation: running.correlation.parent_generation,
                        error: match failure {
                            TaskFailure::CancelledBeforeStart => {
                                WebMediaCatalogSafeError::DiscoveryFailed
                            }
                            TaskFailure::Panicked | TaskFailure::ExecutorStopped => {
                                WebMediaCatalogSafeError::WorkerFailed
                            }
                        },
                    };
                }
                true
            }
        }
    }

    pub(crate) fn state(&self) -> WebMediaCatalogState {
        self.visible.clone()
    }

    pub(crate) fn shutdown_until(&mut self, deadline: Instant) -> WebMediaCatalogShutdownReport {
        self.cancel_running();
        self.latest_pending = None;
        let Some(executor) = self.executor.take() else {
            return WebMediaCatalogShutdownReport {
                detached_deadline_workers: 0,
                panicked_workers: 0,
            };
        };
        let report = executor.shutdown_and_join_until(deadline);
        WebMediaCatalogShutdownReport {
            detached_deadline_workers: report.detached_deadline_workers,
            panicked_workers: report.panicked_workers,
        }
    }

    fn submit(&mut self, request: CatalogRequest) {
        let Some(executor) = self.executor.as_ref() else {
            self.visible = WebMediaCatalogState::Failed {
                parent_generation: request.correlation.parent_generation,
                error: WebMediaCatalogSafeError::ExecutorUnavailable,
            };
            return;
        };
        let source_cancellation = source_core::CancellationToken::new();
        let task_cancellation = source_cancellation.clone();
        let generation = request.generation;
        let correlation = request.correlation.clone();
        let attachment = request.attachment;
        let wake = self.wake_port.clone();
        match executor.try_submit_with_terminal_notifier(
            move |executor_cancellation| CatalogCompletion {
                generation,
                correlation,
                result: if executor_cancellation.is_cancelled() {
                    Err(anyhow::anyhow!("catalog task cancelled before discovery"))
                } else {
                    attachment.run(task_cancellation)
                },
            },
            move || {
                let _delivery = wake.request_wake();
            },
        ) {
            Ok(task) => {
                self.running = Some(RunningCatalogRequest {
                    generation: request.generation,
                    correlation: request.correlation,
                    source_cancellation,
                    task,
                });
            }
            Err(SubmitError::QueueFull) => {
                self.visible = WebMediaCatalogState::Failed {
                    parent_generation: request.correlation.parent_generation,
                    error: WebMediaCatalogSafeError::Backpressure,
                };
            }
            Err(SubmitError::ShuttingDown) => {
                self.visible = WebMediaCatalogState::Failed {
                    parent_generation: request.correlation.parent_generation,
                    error: WebMediaCatalogSafeError::ExecutorUnavailable,
                };
            }
        }
    }

    fn cancel_running(&mut self) {
        if let Some(running) = &self.running {
            running.source_cancellation.cancel();
            running.task.cancel();
        }
    }
}

#[cfg(test)]
mod tests {
    use std::num::NonZeroU64;
    use std::sync::atomic::{AtomicBool, Ordering};
    use std::sync::mpsc::{Receiver, sync_channel};
    use std::time::Duration;

    use web_media_core::{
        CandidateFormatIdentity, CandidateIdentity, ExtractionGeneration, SemanticIdentity,
        SourceIdentity,
    };

    use super::*;
    use crate::app_wake::AppWakeOwner;
    use crate::web_media_catalog::{
        WebMediaCatalogChoice, WebMediaCatalogDiscovery, WebMediaMode, WebMediaSelectionTarget,
    };

    struct ImmediateDiscovery {
        target: u64,
    }

    impl WebMediaCatalogDiscovery for ImmediateDiscovery {
        fn discover(
            &self,
            _cancellation: source_core::CancellationToken,
        ) -> anyhow::Result<DiscoveredWebMediaCatalog> {
            let target = WebMediaSelectionTarget::Fixture(self.target);
            Ok(DiscoveredWebMediaCatalog {
                choices: vec![WebMediaCatalogChoice {
                    mode: WebMediaMode::AudioOnly,
                    video: None,
                    rank: web_media_playback_plan::OpaqueAlternativeRank::parent(0),
                    target: target.clone(),
                }],
                active: target,
                rejected_siblings: 0,
            })
        }
    }

    struct WaitForCancellationDiscovery {
        started: std::sync::mpsc::SyncSender<()>,
        observed_cancellation: Arc<AtomicBool>,
    }

    impl WebMediaCatalogDiscovery for WaitForCancellationDiscovery {
        fn discover(
            &self,
            cancellation: source_core::CancellationToken,
        ) -> anyhow::Result<DiscoveredWebMediaCatalog> {
            self.started.send(()).unwrap();
            while !cancellation.is_cancelled() {
                std::thread::yield_now();
            }
            self.observed_cancellation.store(true, Ordering::Release);
            anyhow::bail!("cancelled fixture")
        }
    }

    struct NonCooperativeDiscovery {
        started: std::sync::mpsc::SyncSender<()>,
    }

    impl WebMediaCatalogDiscovery for NonCooperativeDiscovery {
        fn discover(
            &self,
            _cancellation: source_core::CancellationToken,
        ) -> anyhow::Result<DiscoveredWebMediaCatalog> {
            self.started.send(()).unwrap();
            std::thread::sleep(Duration::from_millis(100));
            anyhow::bail!("late fixture")
        }
    }

    fn exact_identity(source: u64, generation: u64, key: &str) -> ExactSelectionIdentity {
        let source = SourceIdentity::new(source);
        ExactSelectionIdentity::new(
            CandidateIdentity::new(
                source,
                ExtractionGeneration::new(generation),
                CandidateFormatIdentity::new(key).unwrap(),
            ),
            SemanticIdentity::new(source, key).unwrap(),
        )
        .unwrap()
    }

    fn correlation(
        parent: ExactSelectionIdentity,
        item: u64,
        media_instance: u64,
        generation: u64,
    ) -> WebMediaCatalogCorrelation {
        WebMediaCatalogCorrelation {
            scope: WebMediaCatalogScope::Item(
                PlaylistItemId::from_persistence_value(item).unwrap(),
            ),
            parent,
            media_instance: MediaInstanceId::from_non_zero(
                NonZeroU64::new(media_instance).unwrap(),
            ),
            binding: PlaylistRuntimeBinding::for_test(1, generation),
            parent_generation: WebMediaStreamGeneration::for_test(1, generation),
        }
    }

    fn wait_until_ready(
        coordinator: &mut WebMediaCatalogCoordinator,
    ) -> Arc<WebMediaVerifiedCatalog> {
        let deadline = Instant::now() + Duration::from_secs(2);
        loop {
            coordinator.drain();
            if let WebMediaCatalogState::Ready(catalog) = coordinator.state() {
                return catalog;
            }
            assert!(Instant::now() < deadline, "catalog fixture did not finish");
            std::thread::yield_now();
        }
    }

    fn started_channel() -> (std::sync::mpsc::SyncSender<()>, Receiver<()>) {
        sync_channel(1)
    }

    #[test]
    fn latest_correlation_cancels_running_and_only_latest_snapshot_becomes_visible() {
        let wake = AppWakePort::disconnected(AppWakeOwner::PlaylistRuntime);
        let mut coordinator = WebMediaCatalogCoordinator::new(wake);
        let first_parent = exact_identity(1, 1, "first");
        let second_parent = exact_identity(1, 2, "second");
        let (started, started_receiver) = started_channel();
        let observed_cancellation = Arc::new(AtomicBool::new(false));
        coordinator.ensure(
            correlation(first_parent.clone(), 1, 1, 1),
            WebMediaCatalogAttachment::new(
                first_parent,
                Arc::new(WaitForCancellationDiscovery {
                    started,
                    observed_cancellation: Arc::clone(&observed_cancellation),
                }),
            ),
        );
        started_receiver.recv().unwrap();
        coordinator.ensure(
            correlation(second_parent.clone(), 1, 2, 2),
            WebMediaCatalogAttachment::new(
                second_parent,
                Arc::new(ImmediateDiscovery { target: 2 }),
            ),
        );

        let catalog = wait_until_ready(&mut coordinator);

        assert!(observed_cancellation.load(Ordering::Acquire));
        assert_eq!(
            catalog.active_choice().target,
            WebMediaSelectionTarget::Fixture(2)
        );
    }

    #[test]
    fn shutdown_reports_non_cooperative_catalog_worker_at_deadline() {
        let wake = AppWakePort::disconnected(AppWakeOwner::PlaylistRuntime);
        let mut coordinator = WebMediaCatalogCoordinator::new(wake);
        let parent = exact_identity(2, 1, "slow");
        let (started, started_receiver) = started_channel();
        coordinator.ensure(
            correlation(parent.clone(), 2, 3, 1),
            WebMediaCatalogAttachment::new(parent, Arc::new(NonCooperativeDiscovery { started })),
        );
        started_receiver.recv().unwrap();

        let report = coordinator.shutdown_until(Instant::now() + Duration::from_millis(5));

        assert_eq!(report.detached_deadline_workers, 1);
        assert_eq!(report.panicked_workers, 0);
    }
}
