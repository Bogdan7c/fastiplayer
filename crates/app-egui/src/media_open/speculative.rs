//! Policy-neutral owner одной speculative source/demux preparation.

use std::sync::Arc;

use player_core::MediaInstallCancellationCause;

use crate::app_wake::AppWakePort;
use crate::process_shutdown::{ProcessOwnerShutdownOutcome, ShutdownDeadline};

use super::executor::{
    PreparationCancellation, PreparationExecutor, PreparationResultSlot, PreparationWork,
};
use super::{
    MediaOpenSourceRequest, MediaOpenStartError, MediaPreparationFailureKind, PreparedMediaOpen,
    SafeMediaLabel,
};

/// Результат одного nonblocking owner poll-а.
pub(crate) enum SpeculativeMediaPreparationPoll {
    /// Owner не держит request.
    Idle,
    /// Worker ещё подготавливает source/demux.
    Preparing,
    /// Подготовленный envelope передаётся caller-у ровно один раз.
    Ready {
        prepared_open: Box<PreparedMediaOpen>,
        safe_label: SafeMediaLabel,
    },
    /// Speculative failure не имеет authority ломать текущий playback.
    Failed(MediaPreparationFailureKind),
    /// Shared result state потерян; caller обязан fail closed и забыть preload.
    InvariantLost,
}

/// Текущий request владеет cancellation и exactly-once result slot.
struct CurrentSpeculativePreparation {
    cancellation: Arc<PreparationCancellation>,
    result_slot: Arc<PreparationResultSlot>,
    safe_label: SafeMediaLabel,
}

/// Отдельный bounded executor не конкурирует за single slot authoritative coordinator-а.
pub(crate) struct SpeculativeMediaPreparation {
    executor: Arc<PreparationExecutor>,
    current: Option<CurrentSpeculativePreparation>,
}

impl SpeculativeMediaPreparation {
    /// Создаёт lazy executor; worker threads появляются только при первом preload.
    pub(crate) fn new(wake_port: AppWakePort) -> Self {
        Self {
            executor: PreparationExecutor::new_single_worker(wake_port),
            current: None,
        }
    }

    /// Заменяет только speculative request и никогда не трогает authoritative open.
    pub(crate) fn start(
        &mut self,
        source_request: MediaOpenSourceRequest,
    ) -> Result<(), MediaOpenStartError> {
        self.cancel(MediaInstallCancellationCause::Superseded);
        let safe_label = source_request.safe_label();
        let cancellation = Arc::new(PreparationCancellation::new());
        let result_slot = Arc::new(PreparationResultSlot::new());
        let work = PreparationWork::new(
            Arc::clone(&cancellation),
            Arc::clone(&result_slot),
            move |worker_cancellation| {
                super::preparation::prepare_source(source_request, worker_cancellation)
            },
        );
        self.executor.submit_latest(work)?;
        self.current = Some(CurrentSpeculativePreparation {
            cancellation,
            result_slot,
            safe_label,
        });
        Ok(())
    }

    /// Извлекает готовый результат без polling spin и без скрытой retry policy.
    pub(crate) fn poll(&mut self) -> SpeculativeMediaPreparationPoll {
        let Some(current) = self.current.as_ref() else {
            return SpeculativeMediaPreparationPoll::Idle;
        };
        let result = match current.result_slot.take() {
            Ok(Some(result)) => result,
            Ok(None) => return SpeculativeMediaPreparationPoll::Preparing,
            Err(()) => {
                self.current = None;
                return SpeculativeMediaPreparationPoll::InvariantLost;
            }
        };
        let current = self
            .current
            .take()
            .expect("result принадлежит существующему speculative request");
        match result {
            Ok(prepared_open) => SpeculativeMediaPreparationPoll::Ready {
                prepared_open: Box::new(prepared_open),
                safe_label: current.safe_label,
            },
            Err(failure) => SpeculativeMediaPreparationPoll::Failed(failure),
        }
    }

    /// Cooperative cancellation сразу освобождает owner slot; late payload будет dropped.
    pub(crate) fn cancel(&mut self, cause: MediaInstallCancellationCause) {
        if let Some(current) = self.current.take() {
            current.cancellation.cancel(cause);
        }
    }

    /// Terminal shutdown сохраняет exact join authority executor worker-ов.
    pub(crate) fn shutdown_until(
        &mut self,
        deadline: ShutdownDeadline,
    ) -> ProcessOwnerShutdownOutcome {
        self.cancel(MediaInstallCancellationCause::LifecycleShutdown);
        self.executor.shutdown_until(deadline)
    }
}

#[cfg(test)]
mod tests {
    use std::io::Write;
    use std::time::{Duration, Instant};

    use super::*;
    use crate::app_wake::AppWakeOwner;
    use crate::process_shutdown::ShutdownDeadline;

    #[test]
    fn local_preload_reaches_prepared_demux_without_authoritative_player_install() {
        // Required test сам владеет WAV и не зависит от локальных `test-assets` владельца.
        let mut fixture_file = tempfile::NamedTempFile::new().expect("create hermetic PCM fixture");
        fixture_file
            .write_all(&super::super::local::tests::pcm_wav_bytes())
            .expect("write complete hermetic PCM fixture");
        let fixture_path = fixture_file.path().to_path_buf();
        let mut preparation = SpeculativeMediaPreparation::new(AppWakePort::disconnected(
            AppWakeOwner::PlaylistRuntime,
        ));
        preparation
            .start(MediaOpenSourceRequest::Local {
                path: fixture_path.clone(),
                expected_fingerprint: None,
                demux_config: fastiplayer_config::PlayerDemuxConfig::default(),
            })
            .expect("speculative local preparation starts");

        let deadline = Instant::now() + Duration::from_secs(5);
        let prepared_open = loop {
            match preparation.poll() {
                SpeculativeMediaPreparationPoll::Preparing => {
                    assert!(
                        Instant::now() < deadline,
                        "local speculative preparation timed out"
                    );
                    std::thread::sleep(Duration::from_millis(5));
                }
                SpeculativeMediaPreparationPoll::Ready { prepared_open, .. } => {
                    break prepared_open;
                }
                SpeculativeMediaPreparationPoll::Failed(failure) => {
                    panic!("local speculative preparation failed: {failure:?}");
                }
                SpeculativeMediaPreparationPoll::InvariantLost => {
                    panic!("speculative preparation lost its executor invariant");
                }
                SpeculativeMediaPreparationPoll::Idle => {
                    panic!("started speculative preparation cannot become idle without result");
                }
            }
        };

        assert!(matches!(
            prepared_open.descriptor,
            super::super::PreparedMediaDescriptor::Local {
                source: super::super::ActiveMediaSource::LocalFile(ref path),
                ..
            } if path == &fixture_path
        ));
        assert!(
            !prepared_open.prepared_media.tracks().is_empty(),
            "source open must reach demux probing, not merely read bytes"
        );
        assert_eq!(
            preparation.shutdown_until(ShutdownDeadline::after(Duration::from_secs(2))),
            ProcessOwnerShutdownOutcome::Completed
        );
    }
}
