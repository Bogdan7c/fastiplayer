use std::fs;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
use std::sync::mpsc::{self, Receiver, Sender};
use std::sync::{Arc, Condvar, Mutex};
use std::time::{Duration, Instant, SystemTime};

use media_core::MediaTagMetadata;

use super::*;

mod executor_lifecycle_tests;
mod job_stream_tests;

#[derive(Default)]
struct ProbeGate {
    released: Mutex<bool>,
    changed: Condvar,
}

impl ProbeGate {
    fn release(&self) {
        *self
            .released
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner()) = true;
        self.changed.notify_all();
    }

    fn wait(&self) {
        let mut released = self
            .released
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        while !*released {
            released = self
                .changed
                .wait(released)
                .unwrap_or_else(|poisoned| poisoned.into_inner());
        }
    }
}

struct FakeProbe {
    starts: Sender<String>,
    gate: Arc<ProbeGate>,
}

impl DiscoveryProbe for FakeProbe {
    fn read_fingerprint(
        &self,
        _locator: &Path,
        _cancellation: &source_core::CancellationToken,
    ) -> Result<LocalMediaFingerprint, ProbeOneLocalMediaError> {
        Ok(LocalMediaFingerprint::new(1, SystemTime::UNIX_EPOCH))
    }

    fn probe(
        &self,
        locator: &Path,
        _cancellation: &source_core::CancellationToken,
    ) -> Result<ProbedLocalMedia, ProbeOneLocalMediaError> {
        let filename = locator
            .file_name()
            .and_then(|name| name.to_str())
            .unwrap_or("unnamed")
            .to_owned();
        let _ = self.starts.send(filename.clone());
        assert!(!filename.starts_with("panic"), "deterministic fake panic");
        if filename.contains("block") {
            self.gate.wait();
        }
        if filename.starts_with("fail") || filename.contains("-fail-") {
            return Err(ProbeOneLocalMediaError::ProbeFailure {
                reason: "deterministic fake failure".to_owned(),
            });
        }
        let media_kind = if filename.starts_with("audio") {
            LocalMediaKind::AudioOnly
        } else {
            LocalMediaKind::VideoContaining
        };
        Ok(fake_media(filename, media_kind))
    }
}

#[derive(Default)]
struct CountingWake {
    count: AtomicUsize,
    disconnected: AtomicBool,
}

impl DiscoveryWakePort for CountingWake {
    fn wake(&self) -> Result<(), WakeDisconnected> {
        self.count.fetch_add(1, Ordering::AcqRel);
        if self.disconnected.load(Ordering::Acquire) {
            Err(WakeDisconnected)
        } else {
            Ok(())
        }
    }
}

fn fake_media(display_filename: String, media_kind: LocalMediaKind) -> ProbedLocalMedia {
    ProbedLocalMedia::new(
        display_filename,
        media_kind,
        None,
        MediaTagMetadata::default(),
        LocalMediaFingerprint::new(1, SystemTime::UNIX_EPOCH),
    )
}

fn fake_executor() -> (DiscoveryExecutor, Receiver<String>, Arc<ProbeGate>) {
    fake_executor_with_wake(wake_port())
}

fn fake_executor_with_wake(
    wake: Arc<CountingWake>,
) -> (DiscoveryExecutor, Receiver<String>, Arc<ProbeGate>) {
    let (starts, started) = mpsc::channel();
    let gate = Arc::new(ProbeGate::default());
    let probe = Arc::new(FakeProbe {
        starts,
        gate: Arc::clone(&gate),
    });
    (
        DiscoveryExecutor::start_with_probe(probe, wake).expect("fake executor must start"),
        started,
        gate,
    )
}

fn fake_executor_with_worker_count(
    worker_count: usize,
) -> (DiscoveryExecutor, Receiver<String>, Arc<ProbeGate>) {
    let (starts, started) = mpsc::channel();
    let gate = Arc::new(ProbeGate::default());
    let probe = Arc::new(FakeProbe {
        starts,
        gate: Arc::clone(&gate),
    });
    (
        DiscoveryExecutor::start_with_test_worker_count(probe, wake_port(), worker_count)
            .expect("fake executor must start"),
        started,
        gate,
    )
}

fn wake_port() -> Arc<CountingWake> {
    Arc::new(CountingWake::default())
}

fn wait_for_final(handle: &DiscoveryJobHandle) -> DiscoveryFinalSummary {
    let deadline = Instant::now() + Duration::from_secs(5);
    loop {
        if let Some(summary) = handle.take_final_summary() {
            return summary;
        }
        assert!(
            Instant::now() < deadline,
            "job did not publish terminal slot"
        );
        std::thread::yield_now();
    }
}

fn wait_until_processed(handle: &DiscoveryJobHandle, expected: usize) {
    let deadline = Instant::now() + Duration::from_secs(5);
    loop {
        if handle
            .take_progress()
            .is_some_and(|progress| progress.processed == expected)
        {
            return;
        }
        assert!(
            Instant::now() < deadline,
            "progress did not reach expected count"
        );
        std::thread::yield_now();
    }
}

struct TestDirectory {
    path: PathBuf,
}

impl TestDirectory {
    fn new() -> Self {
        static NEXT: AtomicUsize = AtomicUsize::new(1);
        let sequence = NEXT.fetch_add(1, Ordering::Relaxed);
        let path = std::env::temp_dir().join(format!(
            "fastiplayer-discovery-session09a-{}-{sequence}",
            std::process::id()
        ));
        fs::create_dir(&path).unwrap();
        Self { path }
    }
}

impl Drop for TestDirectory {
    fn drop(&mut self) {
        fs::remove_dir_all(&self.path).unwrap();
    }
}
